//! Errors emitted by the secret-vault providers.
//!
//! Every variant carries the *reference* (e.g.
//! `vault:hashicorp/secret/data/db/prod#password`) so logs are
//! debuggable without ever exposing the resolved secret. The wrapped
//! `source` strings come from the underlying transport (`reqwest`,
//! `tokio::process`) — we stringify them at the boundary because:
//!
//! 1. `reqwest::Error` is **not** `Clone`, and the in-flight dedup
//!    map fans an error out to every concurrent waiter, which means
//!    the error must be cloneable.
//! 2. The transport-level structure is rarely actionable for the
//!    user; a human-readable message is the right granularity at
//!    this layer.
//!
//! `VaultError` therefore is `Clone + Debug + std::error::Error` and
//! its `Display` impl always shows the reference first so the
//! operator can paste the line into a vault CLI to reproduce.
//!
//! # Security: never log secrets
//!
//! The crate-level rule is enforceable here because every variant's
//! `Display` formatter is bounded to the reference + an error class
//! string. No constructor takes a resolved [`secrecy::SecretString`]
//! — there is no path that could leak the secret material through
//! the error channel.

use thiserror::Error;

/// All failure modes of [`crate::vault::VaultProvider::resolve`].
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum VaultError {
    /// The reference parses but no provider with that name is
    /// registered. The caller should re-check `settings.toml` or
    /// register the provider before opening the connection.
    #[error("vault: no provider registered for `{provider}` (reference `{reference}`)")]
    UnknownProvider { provider: String, reference: String },

    /// Reference syntactically malformed (missing `#field` where the
    /// provider requires one, or non-UTF8 input).
    #[error("vault: malformed reference `{reference}`: {reason}")]
    MalformedReference { reference: String, reason: String },

    /// Provider configuration is missing or incomplete
    /// (`settings.vault.providers.hashicorp` absent, `token_env`
    /// not set, env var pointed to by `token_env` empty, etc.).
    #[error("vault: provider `{provider}` not configured: {reason}")]
    NotConfigured { provider: String, reason: String },

    /// The secret could not be found at the given reference. Hashicorp
    /// returns HTTP 404; 1Password's `op read` exits non-zero with the
    /// reference quoted in stderr.
    #[error("vault: reference `{reference}` not found")]
    NotFound { reference: String },

    /// Vault denied the request. Hashicorp returns 403 (token expired
    /// / no policy); 1Password returns "rate-limited" or "not
    /// authorized" on stderr. We surface the class so callers know
    /// whether a retry is worth attempting.
    #[error("vault: access denied for `{reference}`: {reason}")]
    Denied { reference: String, reason: String },

    /// Transport failure — DNS, refused connection, TLS handshake,
    /// missing binary on PATH. The user gets a fast error instead of
    /// a 30s hang, per the brief's offline-mode tricky bit.
    #[error("vault: provider unreachable for `{reference}`: {reason}")]
    Unreachable { reference: String, reason: String },

    /// The provider answered but the payload shape was unexpected
    /// (missing `data.data.field` on Hashicorp, empty stdout from
    /// `op read`). This is *almost always* a configuration error in
    /// the secret itself, not in narwhal.
    #[error("vault: unexpected response shape for `{reference}`: {reason}")]
    BadResponse { reference: String, reason: String },

    /// Per-call timeout fired. Default 5 s for Hashicorp, 10 s for
    /// 1Password CLI (CLI cold-start dominates that path).
    #[error("vault: timed out resolving `{reference}` after {seconds}s")]
    Timeout { reference: String, seconds: u64 },

    /// In-flight dedup broadcast channel closed unexpectedly. Should
    /// be unreachable in normal operation; surfaced as a distinct
    /// variant so it shows up in audit logs if it ever happens.
    #[error("vault: internal dedup channel closed for `{reference}`")]
    DedupChannelClosed { reference: String },
}

impl VaultError {
    /// Convenience: classify an HTTP status into the right variant.
    /// Returns `None` for 2xx so call sites can `.map_err(…)?` on
    /// the unhappy path only.
    pub(crate) fn from_http_status(
        reference: &str,
        status: reqwest::StatusCode,
        body_hint: &str,
    ) -> Option<Self> {
        if status.is_success() {
            return None;
        }
        Some(match status.as_u16() {
            404 => Self::NotFound {
                reference: reference.to_owned(),
            },
            401 | 403 => Self::Denied {
                reference: reference.to_owned(),
                reason: format!("HTTP {status}"),
            },
            _ => Self::BadResponse {
                reference: reference.to_owned(),
                reason: format!("HTTP {status}: {}", truncate(body_hint, 256)),
            },
        })
    }

    /// Convenience for `reqwest::Error` → classified `VaultError`.
    pub(crate) fn from_reqwest(reference: &str, err: &reqwest::Error) -> Self {
        if err.is_timeout() {
            // The caller has its own timeout layer; reqwest's own
            // timeout being hit still gets classified as Unreachable
            // because the *result* to the user is identical.
            Self::Unreachable {
                reference: reference.to_owned(),
                reason: format!("reqwest timeout: {err}"),
            }
        } else if err.is_connect() || err.is_request() {
            Self::Unreachable {
                reference: reference.to_owned(),
                reason: err.to_string(),
            }
        } else {
            Self::BadResponse {
                reference: reference.to_owned(),
                reason: err.to_string(),
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_owned()
    } else {
        format!("{}\u{2026}", &s[..max])
    }
}
