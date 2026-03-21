//! `${env:VAR}` interpolation for connection strings (L36 #6).
//!
//! Two forms are supported:
//!
//! * `${env:NAME}` — fail with [`InterpolateError::MissingVar`] when
//!   the variable is unset.
//! * `${env:NAME:fallback}` — substitute `fallback` (a literal string,
//!   `:` allowed inside) when the variable is unset.
//!
//! Substitution is recursive: a fallback may itself contain another
//! `${env:…}` reference up to a small fixed depth so users can chain
//! `${env:PG_PASS:${env:DEFAULT_PASS:public}}` without us looping
//! forever on a self-referential expression.
//!
//! Only `${env:…}` is recognised. Bare `$VAR` and `${VAR}` are left
//! alone so existing connection strings that legitimately contain a
//! dollar sign (notably scram credentials) keep working.
//!
//! The lookup is parameterised by a closure so tests can drive the
//! expander with a fixed map and never touch `std::env` (which is
//! `unsafe` to mutate under the Rust 2024 edition and would violate
//! this crate's `#![forbid(unsafe_code)]` lint).

use std::env;

use thiserror::Error;

use crate::settings::ConnectionsFile;

/// Hard cap on `${env:OUTER:${env:INNER:…}}` nesting.
///
/// L36 #m5: chosen empirically — every real-world chain we've seen
/// (vault → env → literal fallback, kubernetes-secret → env →
/// literal, …) tops out at three levels. Eight gives ample headroom
/// for unforeseen combinations while still being small enough that
/// a hand-rolled cycle (a fallback that references its own name)
/// bails out in microseconds rather than blowing the stack. Lifting
/// this is safe in principle but the *intent* is to flag suspicious
/// configurations early rather than enable deep recursion.
const MAX_DEPTH: u8 = 8;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum InterpolateError {
    #[error("environment variable `{0}` is not set and no fallback was provided")]
    MissingVar(String),
    #[error("`${{env:…}}` placeholder is missing a closing brace in: {0}")]
    UnterminatedPlaceholder(String),
    #[error("nested `${{env:…}}` fallback exceeded {0} levels — possible cycle")]
    DepthExceeded(u8),
}

/// Wall-clock lookup: forwards to [`std::env::var`].
fn env_lookup(name: &str) -> Option<String> {
    env::var(name).ok()
}

/// In-place expansion of every string field on every connection.
///
/// Operates over `host`, `database`, `username`, `path`, every entry
/// of `options`, the SSL certificate paths (whose `PathBuf`s are
/// rebuilt from interpolated strings), and the SSH tunnel config.
pub fn interpolate_connections(file: &mut ConnectionsFile) -> Result<(), InterpolateError> {
    interpolate_connections_with(file, env_lookup)
}

/// Test-facing variant of [`interpolate_connections`] that takes an
/// explicit lookup closure.
pub fn interpolate_connections_with<F>(
    file: &mut ConnectionsFile,
    lookup: F,
) -> Result<(), InterpolateError>
where
    F: Fn(&str) -> Option<String> + Copy,
{
    for conn in &mut file.connections {
        interpolate_optional(&mut conn.params.host, lookup)?;
        interpolate_optional(&mut conn.params.database, lookup)?;
        interpolate_optional(&mut conn.params.username, lookup)?;
        interpolate_optional(&mut conn.params.path, lookup)?;
        for value in conn.params.options.values_mut() {
            let replaced = interpolate_with(value, lookup)?;
            *value = replaced;
        }
        if let Some(p) = conn.params.ssl_root_cert.take() {
            conn.params.ssl_root_cert = Some(interpolate_path(p, lookup)?);
        }
        if let Some(p) = conn.params.ssl_cert.take() {
            conn.params.ssl_cert = Some(interpolate_path(p, lookup)?);
        }
        if let Some(p) = conn.params.ssl_key.take() {
            conn.params.ssl_key = Some(interpolate_path(p, lookup)?);
        }
        if let Some(ssh) = conn.params.ssh.as_mut() {
            let host = interpolate_with(&ssh.host, lookup)?;
            ssh.host = host;
            let user = interpolate_with(&ssh.user, lookup)?;
            ssh.user = user;
            if let Some(p) = ssh.key_path.take() {
                ssh.key_path = Some(interpolate_path(p, lookup)?);
            }
            if let Some(jh) = ssh.jump_host.take() {
                ssh.jump_host = Some(interpolate_with(&jh, lookup)?);
            }
        }
    }
    Ok(())
}

fn interpolate_optional<F>(slot: &mut Option<String>, lookup: F) -> Result<(), InterpolateError>
where
    F: Fn(&str) -> Option<String> + Copy,
{
    if let Some(s) = slot.take() {
        *slot = Some(interpolate_with(&s, lookup)?);
    }
    Ok(())
}

fn interpolate_path<F>(
    p: std::path::PathBuf,
    lookup: F,
) -> Result<std::path::PathBuf, InterpolateError>
where
    F: Fn(&str) -> Option<String> + Copy,
{
    let s = p.to_string_lossy().into_owned();
    let expanded = interpolate_with(&s, lookup)?;
    Ok(std::path::PathBuf::from(expanded))
}

/// Expand every `${env:…}` placeholder using the real process
/// environment. Convenience wrapper around
/// [`interpolate_with`] for host code that always reads `std::env`.
pub fn interpolate(input: &str) -> Result<String, InterpolateError> {
    interpolate_with(input, env_lookup)
}

/// Expand every `${env:…}` placeholder, looking up names through
/// `lookup`. Used by both the host (which forwards to `std::env`) and
/// the test suite (which injects a fixed map).
pub fn interpolate_with<F>(input: &str, lookup: F) -> Result<String, InterpolateError>
where
    F: Fn(&str) -> Option<String> + Copy,
{
    interpolate_inner(input, lookup, 0)
}

fn interpolate_inner<F>(input: &str, lookup: F, depth: u8) -> Result<String, InterpolateError>
where
    F: Fn(&str) -> Option<String> + Copy,
{
    if depth >= MAX_DEPTH {
        return Err(InterpolateError::DepthExceeded(MAX_DEPTH));
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${env:") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "${env:".len()..];
        // Brace-aware end search so nested `${env:…}` fallbacks
        // (e.g. `${env:OUTER:${env:INNER}}`) are not cut short on
        // the inner `}`. Walks the byte stream, increments on every
        // opening `${`, decrements on `}`, and treats the first `}`
        // that brings the depth back to -1 as the end of *this*
        // placeholder.
        let mut end = None;
        let mut brace_depth: i32 = 0;
        let bytes = after.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                brace_depth += 1;
                i += 2;
                continue;
            }
            if bytes[i] == b'}' {
                if brace_depth == 0 {
                    end = Some(i);
                    break;
                }
                brace_depth -= 1;
            }
            i += 1;
        }
        let Some(end) = end else {
            return Err(InterpolateError::UnterminatedPlaceholder(input.into()));
        };
        let body = &after[..end];
        let (name, fallback) = match body.split_once(':') {
            Some((n, f)) => (n, Some(f.to_owned())),
            None => (body, None),
        };
        let value = match lookup(name) {
            Some(v) => v,
            None => match fallback {
                Some(f) => interpolate_inner(&f, lookup, depth + 1)?,
                None => return Err(InterpolateError::MissingVar(name.into())),
            },
        };
        out.push_str(&value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::literal_string_with_formatting_args)] // `${env:…}` is our DSL, not a format arg.
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn map_lookup<'a>(
        map: &'a HashMap<&'static str, &'static str>,
    ) -> impl Fn(&str) -> Option<String> + Copy + 'a {
        move |name: &str| map.get(name).map(|s| (*s).to_owned())
    }

    #[test]
    fn passthrough_when_no_placeholder() {
        let m = HashMap::new();
        let l = map_lookup(&m);
        assert_eq!(interpolate_with("plain", l).unwrap(), "plain");
        assert_eq!(
            interpolate_with("$VAR untouched", l).unwrap(),
            "$VAR untouched"
        );
        assert_eq!(interpolate_with("${OTHER}", l).unwrap(), "${OTHER}");
    }

    #[test]
    fn substitutes_env_var() {
        let mut m = HashMap::new();
        m.insert("PROBE", "value");
        let l = map_lookup(&m);
        assert_eq!(
            interpolate_with("prefix-${env:PROBE}-suffix", l).unwrap(),
            "prefix-value-suffix"
        );
    }

    #[test]
    fn missing_var_errors() {
        let m = HashMap::new();
        let l = map_lookup(&m);
        let err = interpolate_with("${env:NOPE}", l).unwrap_err();
        match err {
            InterpolateError::MissingVar(name) => assert_eq!(name, "NOPE"),
            other => panic!("expected MissingVar, got {other:?}"),
        }
    }

    #[test]
    fn fallback_used_when_var_unset() {
        let m = HashMap::new();
        let l = map_lookup(&m);
        assert_eq!(
            interpolate_with("${env:NOPE:default}", l).unwrap(),
            "default"
        );
    }

    #[test]
    fn nested_fallback_resolves() {
        let mut m = HashMap::new();
        m.insert("INNER", "ok");
        let l = map_lookup(&m);
        assert_eq!(
            interpolate_with("${env:UNSET_OUTER:${env:INNER}}", l).unwrap(),
            "ok"
        );
    }

    #[test]
    fn unterminated_placeholder_errors() {
        let m = HashMap::new();
        let l = map_lookup(&m);
        let err = interpolate_with("${env:WHATEVER", l).unwrap_err();
        assert!(matches!(err, InterpolateError::UnterminatedPlaceholder(_)));
    }

    #[test]
    fn multiple_placeholders_in_one_string() {
        let mut m = HashMap::new();
        m.insert("A", "alpha");
        m.insert("B", "beta");
        let l = map_lookup(&m);
        assert_eq!(
            interpolate_with("${env:A}-${env:B}", l).unwrap(),
            "alpha-beta"
        );
    }

    #[test]
    fn connections_file_fields_interpolated() {
        use narwhal_core::{ConnectionConfig, ConnectionParams};
        use uuid::Uuid;
        let mut file = ConnectionsFile {
            logical_relations: Vec::new(),
            connections: vec![ConnectionConfig {
                id: Uuid::nil(),
                name: "prod".into(),
                driver: "postgres".into(),
                params: ConnectionParams::with(|p| {
                    p.host = Some("${env:PGHOST}".into());
                    p.username = Some("${env:PGUSER:admin}".into());
                    p.database = Some("appdb".into());
                }),
            }],
        };
        let mut m = HashMap::new();
        m.insert("PGHOST", "db.example.com");
        let l = map_lookup(&m);
        interpolate_connections_with(&mut file, l).unwrap();
        assert_eq!(
            file.connections[0].params.host.as_deref(),
            Some("db.example.com")
        );
        assert_eq!(
            file.connections[0].params.username.as_deref(),
            Some("admin")
        );
        assert_eq!(
            file.connections[0].params.database.as_deref(),
            Some("appdb")
        );
    }
}
