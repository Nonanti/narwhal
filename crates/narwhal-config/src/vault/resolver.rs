//! Vault reference parser.
//!
//! Recognised shapes:
//!
//! | Wire form                                          | provider     | path                          | field         |
//! | -------------------------------------------------- | ------------ | ----------------------------- | ------------- |
//! | `vault:hashicorp/secret/data/db/prod#password`     | `hashicorp`  | `secret/data/db/prod`         | `Some("password")` |
//! | `vault:hashicorp/secret/data/db/prod`              | `hashicorp`  | `secret/data/db/prod`         | `None`        |
//! | `vault:my-cluster/secret/data/db/prod#password`    | `my-cluster` | `secret/data/db/prod`         | `Some("password")` |
//! | `1password:op://Vault/Postgres/password`           | `1password`  | `op://Vault/Postgres/password`| `None`        |
//!
//! For `vault:` URIs the first path segment is the *provider name*.
//! That lets users register more than one Hashicorp cluster (think
//! `vault:prod-cluster/…` and `vault:dr-cluster/…`) without inventing
//! a new top-level scheme. The conventional default name is
//! `hashicorp`.
//!
//! For `1password:` URIs the whole tail (`op://…`) is handed to the
//! `op` CLI verbatim, because `op read` already speaks that exact
//! syntax. The reference parser stores the provider as the literal
//! `"1password"` and the path as the entire `op://…` suffix.
//!
//! Anything not matching either prefix returns `None` from
//! [`Reference::try_parse`] — the caller then treats the password
//! string as a literal (the resolver-vs-literal decision is made
//! exactly once, in [`crate::credentials::resolve_password`]).
//!
//! Parsing is deliberately permissive on the *path* portion: vault
//! KV paths can contain almost any printable byte and we do not
//! second-guess the operator's mount layout. The only fail-fast
//! validation is the prefix and (for 1Password) the requirement that
//! the tail begin with `op://`.

use crate::vault::error::VaultError;

/// A parsed vault reference.
///
/// Construct via [`Reference::try_parse`]; the fields are public so
/// downstream code can pattern-match on `provider` without going
/// through accessors, but the type is `#[non_exhaustive]` to leave
/// room for future shapes (`aws:`, `azurekv:`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Reference {
    /// Provider name registered with the
    /// [`crate::vault::VaultRegistry`]. For `vault:` URIs this is
    /// the first path segment; for `1password:` URIs it is the
    /// literal string `"1password"`.
    pub provider: String,
    /// Provider-specific path. Hashicorp KV v2 paths look like
    /// `secret/data/<mount>/<key>`; 1Password tails look like
    /// `op://Vault/Item/field`.
    pub path: String,
    /// Optional field selector after `#`. Hashicorp KV stores a
    /// map under each path, so the caller must pick which field;
    /// 1Password's `op://…/field` already encodes the field in the
    /// path so this is always `None` for 1Password.
    pub field: Option<String>,
    /// The original wire form, preserved verbatim for error messages
    /// and structured logs. Never contains the *secret value* — only
    /// the reference.
    pub raw: String,
}

impl Reference {
    /// Attempt to parse `input` as a vault reference.
    ///
    /// Returns:
    /// * `Ok(Some(reference))` — input is a well-formed vault reference;
    /// * `Ok(None)` — input has no recognised prefix; the caller
    ///   should treat the string as a literal password;
    /// * `Err(_)` — input *starts with* a recognised prefix but its
    ///   tail is malformed. Surfaced as an error rather than `None`
    ///   so a user typo (`vault:` with no path) is not silently
    ///   treated as a literal password.
    pub fn try_parse(input: &str) -> Result<Option<Self>, VaultError> {
        if let Some(tail) = input.strip_prefix("vault:") {
            return Self::parse_vault_tail(input, tail).map(Some);
        }
        if let Some(tail) = input.strip_prefix("1password:") {
            return Self::parse_onepassword_tail(input, tail).map(Some);
        }
        Ok(None)
    }

    fn parse_vault_tail(raw: &str, tail: &str) -> Result<Self, VaultError> {
        if tail.is_empty() {
            return Err(VaultError::MalformedReference {
                reference: raw.to_owned(),
                reason: "empty body after `vault:` prefix".into(),
            });
        }
        // Split off the optional `#field` selector first so a `#` in
        // the *path* (rare but legal in some KV mounts) is not lost.
        // We split from the right because Hashicorp KV paths cannot
        // contain `#`, but 1Password-style paths can — however
        // 1Password uses its own scheme, so `vault:` always splits
        // cleanly on the last `#`.
        let (path_and_provider, field) = match tail.rsplit_once('#') {
            Some((head, f)) if !f.is_empty() => (head, Some(f.to_owned())),
            Some((_, _)) => {
                return Err(VaultError::MalformedReference {
                    reference: raw.to_owned(),
                    reason: "trailing `#` without field name".into(),
                });
            }
            None => (tail, None),
        };
        // First path segment is the provider name.
        let (provider, path) =
            path_and_provider
                .split_once('/')
                .ok_or_else(|| VaultError::MalformedReference {
                    reference: raw.to_owned(),
                    reason: "missing `/` between provider name and path \
                         (expected `vault:<provider>/<path>`)"
                        .into(),
                })?;
        if provider.is_empty() {
            return Err(VaultError::MalformedReference {
                reference: raw.to_owned(),
                reason: "empty provider name".into(),
            });
        }
        if path.is_empty() {
            return Err(VaultError::MalformedReference {
                reference: raw.to_owned(),
                reason: "empty path after provider name".into(),
            });
        }
        Ok(Self {
            provider: provider.to_owned(),
            path: path.to_owned(),
            field,
            raw: raw.to_owned(),
        })
    }

    fn parse_onepassword_tail(raw: &str, tail: &str) -> Result<Self, VaultError> {
        if !tail.starts_with("op://") {
            return Err(VaultError::MalformedReference {
                reference: raw.to_owned(),
                reason: "1password references must use `op://Vault/Item/field` syntax".into(),
            });
        }
        // `op read` parses the path itself, so we hand it the whole
        // `op://…` string verbatim. No field selector at this layer.
        Ok(Self {
            provider: "1password".into(),
            path: tail.to_owned(),
            field: None,
            raw: raw.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_string_is_not_a_reference() {
        assert_eq!(Reference::try_parse("plain-password").unwrap(), None);
        assert_eq!(Reference::try_parse("").unwrap(), None);
        assert_eq!(Reference::try_parse("postgres://foo").unwrap(), None);
    }

    #[test]
    fn hashicorp_with_field() {
        let r = Reference::try_parse("vault:hashicorp/secret/data/db/prod#password")
            .unwrap()
            .unwrap();
        assert_eq!(r.provider, "hashicorp");
        assert_eq!(r.path, "secret/data/db/prod");
        assert_eq!(r.field.as_deref(), Some("password"));
        assert_eq!(r.raw, "vault:hashicorp/secret/data/db/prod#password");
    }

    #[test]
    fn hashicorp_without_field() {
        let r = Reference::try_parse("vault:hashicorp/secret/data/x")
            .unwrap()
            .unwrap();
        assert_eq!(r.provider, "hashicorp");
        assert_eq!(r.path, "secret/data/x");
        assert!(r.field.is_none());
    }

    #[test]
    fn vault_custom_provider_name() {
        let r = Reference::try_parse("vault:prod-cluster/secret/data/db#pw")
            .unwrap()
            .unwrap();
        assert_eq!(r.provider, "prod-cluster");
        assert_eq!(r.path, "secret/data/db");
        assert_eq!(r.field.as_deref(), Some("pw"));
    }

    #[test]
    fn onepassword_reference() {
        let r = Reference::try_parse("1password:op://Vault/Postgres/password")
            .unwrap()
            .unwrap();
        assert_eq!(r.provider, "1password");
        assert_eq!(r.path, "op://Vault/Postgres/password");
        assert!(r.field.is_none());
    }

    #[test]
    fn malformed_empty_body() {
        assert!(Reference::try_parse("vault:").is_err());
        assert!(Reference::try_parse("1password:").is_err());
    }

    #[test]
    fn malformed_missing_provider_separator() {
        // `vault:` with no `/`: only the provider name, no path.
        assert!(Reference::try_parse("vault:hashicorp").is_err());
    }

    #[test]
    fn malformed_trailing_hash() {
        assert!(Reference::try_parse("vault:hashicorp/secret/data/x#").is_err());
    }

    #[test]
    fn malformed_onepassword_without_op_scheme() {
        assert!(Reference::try_parse("1password:Vault/Item/password").is_err());
    }

    #[test]
    fn malformed_never_panics_on_unicode() {
        // Defensive: weird inputs must not panic on byte-slicing.
        for s in [
            "vault:\u{1f9d9}/secret/x#\u{1f389}",
            "vault:provider/path\u{0000}field",
            "1password:op://\u{1f680}/x/y",
        ] {
            let _ = Reference::try_parse(s);
        }
    }
}
