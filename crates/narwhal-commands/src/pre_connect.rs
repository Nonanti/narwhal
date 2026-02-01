//! Pre-connect command runner (L36 #7).
//!
//! Walks the list of [`narwhal_core::PreConnectStep`] attached to a
//! [`narwhal_core::ConnectionParams`], runs each one via `sh -c` with
//! a per-step timeout, captures stdout, and surfaces the result as a
//! `{name -> value}` map suitable for `${preconnect:NAME}`
//! substitution by [`substitute_pre_connect`].
//!
//! Design choices:
//!
//! * **Shell delegation** — we pipe the command line to `sh -c` so
//!   the user keeps pipes, redirections, command substitution, etc.
//!   without us shipping a parser. Windows is supported via `cmd /C`.
//! * **Bounded execution** — every step has its own
//!   `timeout_secs` (default 30); the runner kills a child that
//!   overruns and surfaces a [`PreConnectError::Timeout`].
//! * **Hard-fail by default** — `required = true` aborts the open;
//!   `required = false` logs and keeps going. Mirrors the SSH-tunnel
//!   contract (any failure there *also* aborts the open).
//! * **stdout only** — stderr is captured into the error path on
//!   failure but never substituted. This avoids leaking unrelated
//!   diagnostics into connection strings.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use narwhal_core::PreConnectStep;
use thiserror::Error;
use tokio::process::Command;
use tokio::time::timeout;

const DEFAULT_TIMEOUT_SECS: u32 = 30;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PreConnectError {
    #[error("pre-connect step `{0}`: spawn failed: {1}")]
    Spawn(String, std::io::Error),
    #[error("pre-connect step `{command}`: exited with status {status} — {stderr}")]
    NonZero {
        command: String,
        status: i32,
        stderr: String,
    },
    #[error("pre-connect step `{0}`: timed out after {1}s")]
    Timeout(String, u32),
    #[error("pre-connect step `{0}`: stdout was not valid UTF-8")]
    NotUtf8(String),
}

/// Run every step in order. Each step's trimmed stdout is stored in
/// the returned map under `save_output_to` (when set) so later steps
/// and the connection params can reference it via
/// `${preconnect:NAME}`. Steps without `save_output_to` are still
/// executed for their side effects (notably background processes
/// that need to be started before the driver dials in).
pub async fn run_pre_connect(
    steps: &[PreConnectStep],
) -> Result<HashMap<String, String>, PreConnectError> {
    let mut vars = HashMap::new();
    for step in steps {
        match run_one(step).await {
            Ok(stdout) => {
                if let Some(key) = step.save_output_to.as_deref() {
                    vars.insert(key.to_owned(), stdout);
                }
            }
            Err(error) => {
                if step.required {
                    return Err(error);
                }
                tracing::warn!(
                    target: "narwhal::pre_connect",
                    error = %error,
                    "non-required pre-connect step failed; continuing"
                );
            }
        }
    }
    Ok(vars)
}

async fn run_one(step: &PreConnectStep) -> Result<String, PreConnectError> {
    let secs = step.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS);
    let mut cmd = shell_command(&step.command);
    let spawn = cmd.output();
    let output = match timeout(Duration::from_secs(u64::from(secs)), spawn).await {
        Ok(Ok(output)) => output,
        Ok(Err(io)) => return Err(PreConnectError::Spawn(step.command.clone(), io)),
        Err(_) => return Err(PreConnectError::Timeout(step.command.clone(), secs)),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(PreConnectError::NonZero {
            command: step.command.clone(),
            status: output.status.code().unwrap_or(-1),
            stderr,
        });
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|_| PreConnectError::NotUtf8(step.command.clone()))?;
    Ok(stdout.trim().to_owned())
}

/// Build the shell child with the three hardening flags every
/// long-lived terminal app wants:
///
/// * `stdin(Stdio::null())` — a child that asks for a password / TTY
///   confirmation (sudo, ssh-add, kubectl exec without `-T`) would
///   otherwise hang until the per-step timeout. Mirrors what
///   `narwhal-core::ssh::SshTunnel` does for the very same reason.
/// * `kill_on_drop(true)` — when `tokio::time::timeout` cancels the
///   future, dropping the child handle must actually kill the
///   process; otherwise a wedged `kubectl port-forward` or `sleep`
///   leaks for the lifetime of the narwhal process.
/// * stdout/stderr piped (the default) so `output()` can collect
///   both — we deliberately don't `null` them.
#[cfg(unix)]
fn shell_command(line: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(line)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    cmd
}

#[cfg(windows)]
fn shell_command(line: &str) -> Command {
    // NOTE: Windows quoting differs from POSIX `sh -c`; `&`, `|`,
    // `^`, `"` and `%VAR%` follow `cmd.exe` rules. For complex
    // pipelines users are advised to dispatch through PowerShell:
    //   command = 'powershell -Command "Get-Secret | ConvertTo-Json"'
    let mut cmd = Command::new("cmd");
    cmd.arg("/C")
        .arg(line)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    cmd
}

/// In-place substitution of `${preconnect:NAME}` placeholders in
/// every string field of `params` against the variable map produced
/// by [`run_pre_connect`]. Missing keys are surfaced as
/// [`SubstitutionError::MissingVar`].
///
/// NOTE: the `password` channel is *not* on `ConnectionParams` (it
/// arrives separately from the keyring / pgpass / env-var chain).
/// Use [`substitute_password`] alongside this call to expand a
/// `${preconnect:NAME}` reference inside a fetched password.
pub fn substitute_pre_connect(
    params: &mut narwhal_core::ConnectionParams,
    vars: &HashMap<String, String>,
) -> Result<(), SubstitutionError> {
    substitute_opt(&mut params.host, vars)?;
    substitute_opt(&mut params.database, vars)?;
    substitute_opt(&mut params.username, vars)?;
    substitute_opt(&mut params.path, vars)?;
    for value in params.options.values_mut() {
        let replaced = substitute_str(value, vars)?;
        *value = replaced;
    }
    Ok(())
}

/// L36 #C3: expand `${preconnect:NAME}` placeholders inside the
/// optional password value the credential store handed us.
///
/// This closes the most important pre-connect use case — a vault
/// step that produces a short-lived password, referenced by name
/// in the keyring entry (or in `PGPASSWORD`, etc.). Without this
/// step the placeholder would reach the driver verbatim and the
/// connection would fail with an opaque authentication error.
///
/// `None` passes through unchanged. A password that has no
/// placeholder also passes through unchanged — callers can invoke
/// this unconditionally.
pub fn substitute_password(
    password: Option<String>,
    vars: &HashMap<String, String>,
) -> Result<Option<String>, SubstitutionError> {
    let Some(pw) = password else {
        return Ok(None);
    };
    if !pw.contains("${preconnect:") {
        return Ok(Some(pw));
    }
    Ok(Some(substitute_str(&pw, vars)?))
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SubstitutionError {
    #[error(
        "connection params reference `${{preconnect:{0}}}` but no pre-connect step saved that key"
    )]
    MissingVar(String),
    #[error("`${{preconnect:…}}` placeholder is missing a closing brace in: {0}")]
    Unterminated(String),
}

fn substitute_opt(
    slot: &mut Option<String>,
    vars: &HashMap<String, String>,
) -> Result<(), SubstitutionError> {
    if let Some(s) = slot.take() {
        *slot = Some(substitute_str(&s, vars)?);
    }
    Ok(())
}

fn substitute_str(
    input: &str,
    vars: &HashMap<String, String>,
) -> Result<String, SubstitutionError> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${preconnect:") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "${preconnect:".len()..];
        let Some(end) = after.find('}') else {
            return Err(SubstitutionError::Unterminated(input.into()));
        };
        let name = &after[..end];
        let value = vars
            .get(name)
            .ok_or_else(|| SubstitutionError::MissingVar(name.into()))?;
        out.push_str(value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_steps_yield_empty_map() {
        let vars = run_pre_connect(&[]).await.unwrap();
        assert!(vars.is_empty());
    }

    fn params_with_host(host: &str) -> narwhal_core::ConnectionParams {
        narwhal_core::ConnectionParams {
            host: Some(host.into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn captures_stdout_into_named_var() {
        let steps = vec![PreConnectStep::new("echo hello")
            .with_save_output_to("GREETING")
            .with_timeout_secs(5)];
        let vars = run_pre_connect(&steps).await.unwrap();
        assert_eq!(vars.get("GREETING").map(String::as_str), Some("hello"));
    }

    #[tokio::test]
    async fn step_without_save_output_runs_but_does_not_populate_map() {
        let steps = vec![PreConnectStep::new("true").with_timeout_secs(5)];
        let vars = run_pre_connect(&steps).await.unwrap();
        assert!(vars.is_empty());
    }

    #[tokio::test]
    async fn required_failure_aborts_sequence() {
        let steps = vec![
            PreConnectStep::new("false").with_timeout_secs(5),
            PreConnectStep::new("echo should-not-run")
                .with_save_output_to("X")
                .with_timeout_secs(5),
        ];
        let err = run_pre_connect(&steps).await.unwrap_err();
        assert!(matches!(err, PreConnectError::NonZero { .. }));
    }

    #[tokio::test]
    async fn non_required_failure_continues() {
        let steps = vec![
            PreConnectStep::new("false")
                .with_save_output_to("UNSET")
                .with_timeout_secs(5)
                .with_required(false),
            PreConnectStep::new("echo ok")
                .with_save_output_to("OK")
                .with_timeout_secs(5),
        ];
        let vars = run_pre_connect(&steps).await.unwrap();
        assert!(!vars.contains_key("UNSET"));
        assert_eq!(vars.get("OK").map(String::as_str), Some("ok"));
    }

    #[tokio::test]
    async fn timeout_returns_timeout_error() {
        let steps = vec![PreConnectStep::new("sleep 5").with_timeout_secs(1)];
        let err = run_pre_connect(&steps).await.unwrap_err();
        assert!(matches!(err, PreConnectError::Timeout(_, 1)));
    }

    #[test]
    fn substitute_replaces_placeholder() {
        let mut params = params_with_host("${preconnect:HOST}.example.com");
        params.database = Some("plain".into());
        let mut vars = HashMap::new();
        vars.insert("HOST".into(), "db01".into());
        substitute_pre_connect(&mut params, &vars).unwrap();
        assert_eq!(params.host.as_deref(), Some("db01.example.com"));
        assert_eq!(params.database.as_deref(), Some("plain"));
    }

    #[test]
    fn substitute_errors_on_missing_var() {
        let mut params = params_with_host("${preconnect:NOPE}");
        let vars = HashMap::new();
        let err = substitute_pre_connect(&mut params, &vars).unwrap_err();
        assert!(matches!(err, SubstitutionError::MissingVar(ref n) if n == "NOPE"));
    }

    #[test]
    fn substitute_password_none_passes_through() {
        let vars = HashMap::new();
        assert_eq!(substitute_password(None, &vars).unwrap(), None);
    }

    #[test]
    fn substitute_password_without_placeholder_passes_through() {
        let vars = HashMap::new();
        let out = substitute_password(Some("plain-secret".into()), &vars).unwrap();
        assert_eq!(out.as_deref(), Some("plain-secret"));
    }

    #[test]
    fn substitute_password_expands_placeholder() {
        let mut vars = HashMap::new();
        vars.insert("VAULT_PASS".into(), "hunter2".into());
        let out = substitute_password(Some("${preconnect:VAULT_PASS}".into()), &vars).unwrap();
        assert_eq!(out.as_deref(), Some("hunter2"));
    }

    #[test]
    fn substitute_password_errors_on_missing_var() {
        let vars = HashMap::new();
        let err = substitute_password(Some("${preconnect:GONE}".into()), &vars).unwrap_err();
        assert!(matches!(err, SubstitutionError::MissingVar(ref n) if n == "GONE"));
    }

    #[test]
    fn substitute_passthrough_when_no_placeholder() {
        let mut params = params_with_host("plain.example.com");
        let vars = HashMap::new();
        substitute_pre_connect(&mut params, &vars).unwrap();
        assert_eq!(params.host.as_deref(), Some("plain.example.com"));
    }
}
