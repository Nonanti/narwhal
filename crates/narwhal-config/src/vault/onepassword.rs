//! 1Password CLI provider.
//!
//! Shells out to `op read "op://Vault/Item/field"` and returns the
//! trimmed stdout as the secret. The whole `op://…` URI is taken
//! verbatim from [`super::Reference::path`] so callers don't have to
//! reassemble it from parts.
//!
//! # Auth flow
//!
//! For unattended (CI / service-account) use, the operator must set
//! `OP_SERVICE_ACCOUNT_TOKEN` (or another env var named in
//! [`crate::settings::OnePasswordVaultSettings::service_account_token_env`]).
//! The provider does **not** carry the token itself — `op` reads it
//! from the environment we inherit. We do verify that the env var is
//! set before invoking, so the user gets a clear error rather than
//! `op`'s opaque "no authentication" stderr.
//!
//! For interactive use (developer laptop with `op signin` already
//! done), no env var is required; the provider invokes `op` and
//! lets the CLI handle session lookup.
//!
//! # Binary discovery
//!
//! By default the provider invokes `op` from `$PATH`. Tests and
//! sandboxed environments may override with
//! [`crate::settings::OnePasswordVaultSettings::op_binary`] —
//! pointing this at a shell script that prints a canned value is
//! exactly how the integration tests avoid needing a real 1Password
//! account.
//!
//! # Timeout
//!
//! Default 10 s. CLI cold-start (especially on macOS where the
//! Keychain is unlocked the first time) dominates the wall-clock
//! cost, hence the larger budget than Hashicorp (5 s).

use std::sync::Arc;
use std::time::Duration;

use secrecy::SecretString;
use tokio::process::Command;

use super::{Reference, VaultError, VaultProvider};

/// Default per-call timeout for `op read`.
pub const DEFAULT_OP_TIMEOUT_SECS: u64 = 10;

#[derive(Debug)]
pub struct OnepasswordCli {
    name: String,
    binary: String,
    account: Option<String>,
    /// When `Some`, the provider checks `std::env::var(&self.service_account_token_env)`
    /// is non-empty before invoking `op`. Provides a faster, clearer
    /// error than the CLI itself for the canonical CI misconfiguration.
    service_account_token_env: Option<String>,
    timeout: Duration,
}

impl OnepasswordCli {
    pub fn from_settings(
        name: impl Into<String>,
        settings: &crate::settings::OnePasswordVaultSettings,
    ) -> Result<Self, VaultError> {
        let name = name.into();
        let binary = settings
            .op_binary
            .clone()
            .unwrap_or_else(|| "op".to_owned());
        let timeout = Duration::from_secs(settings.timeout_secs.unwrap_or(DEFAULT_OP_TIMEOUT_SECS));
        Ok(Self {
            name,
            binary,
            account: settings.account.clone(),
            service_account_token_env: settings.service_account_token_env.clone(),
            timeout,
        })
    }

    fn pre_flight(&self) -> Result<(), VaultError> {
        if let Some(env_name) = &self.service_account_token_env {
            match std::env::var(env_name) {
                Ok(v) if !v.is_empty() => {}
                _ => {
                    return Err(VaultError::NotConfigured {
                        provider: self.name.clone(),
                        reason: format!(
                            "env var `{env_name}` (settings.vault.providers.onepassword.service_account_token_env) \
                             is unset or empty"
                        ),
                    });
                }
            }
        }
        Ok(())
    }
}

impl VaultProvider for OnepasswordCli {
    fn name(&self) -> &str {
        &self.name
    }

    fn resolve<'a>(
        &'a self,
        reference: &'a Reference,
    ) -> futures::future::BoxFuture<'a, Result<Arc<SecretString>, VaultError>> {
        Box::pin(async move {
            self.pre_flight()?;

            let mut cmd = Command::new(&self.binary);
            cmd.arg("read").arg(&reference.path);
            if let Some(account) = &self.account {
                cmd.arg("--account").arg(account);
            }
            // Inherit stderr so a user running narwhal in a terminal
            // sees `op`'s own progress / prompt messages. Stdout is
            // the secret; we capture it.
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());

            let child = cmd.spawn().map_err(|e| {
                // Treat ENOENT as Unreachable (the user has not
                // installed `op`) so it slots into the same UI
                // bucket as "Hashicorp DNS failed".
                if e.kind() == std::io::ErrorKind::NotFound {
                    VaultError::Unreachable {
                        reference: reference.raw.clone(),
                        reason: format!("`{}` not found on PATH", self.binary),
                    }
                } else {
                    VaultError::Unreachable {
                        reference: reference.raw.clone(),
                        reason: format!("spawn `{}`: {e}", self.binary),
                    }
                }
            })?;

            let wait_fut = child.wait_with_output();
            let output = match tokio::time::timeout(self.timeout, wait_fut).await {
                Ok(Ok(out)) => out,
                Ok(Err(e)) => {
                    return Err(VaultError::Unreachable {
                        reference: reference.raw.clone(),
                        reason: format!("wait `{}`: {e}", self.binary),
                    });
                }
                Err(_) => {
                    return Err(VaultError::Timeout {
                        reference: reference.raw.clone(),
                        seconds: self.timeout.as_secs(),
                    });
                }
            };

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stderr_l = stderr.to_lowercase();
                let err = if stderr_l.contains("not found") || stderr_l.contains("doesn't exist") {
                    VaultError::NotFound {
                        reference: reference.raw.clone(),
                    }
                } else if stderr_l.contains("rate")
                    || stderr_l.contains("unauthorized")
                    || stderr_l.contains("authentication")
                {
                    VaultError::Denied {
                        reference: reference.raw.clone(),
                        reason: stderr.trim().to_owned(),
                    }
                } else {
                    VaultError::BadResponse {
                        reference: reference.raw.clone(),
                        reason: format!(
                            "`{}` exit {}: {}",
                            self.binary,
                            output
                                .status
                                .code()
                                .map_or_else(|| "<signal>".into(), |c| c.to_string()),
                            stderr.trim()
                        ),
                    }
                };
                return Err(err);
            }

            // `op read` prints the secret as a single line with a
            // trailing newline. Strip it; never trim leading
            // whitespace, since the secret itself may legitimately
            // contain spaces.
            let raw = String::from_utf8(output.stdout).map_err(|e| VaultError::BadResponse {
                reference: reference.raw.clone(),
                reason: format!("non-UTF8 stdout from `op`: {e}"),
            })?;
            let trimmed = raw.trim_end_matches(['\n', '\r']);
            if trimmed.is_empty() {
                return Err(VaultError::BadResponse {
                    reference: reference.raw.clone(),
                    reason: "`op read` produced empty output".into(),
                });
            }
            Ok(Arc::new(SecretString::new(
                trimmed.to_owned().into_boxed_str(),
            )))
        })
    }
}
