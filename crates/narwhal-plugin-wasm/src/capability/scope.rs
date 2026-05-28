//! Argument types used by [`Capability`](super::Capability).
//!
//! Each type is responsible for:
//!
//! * normalising its input (trimming, casing, separator choice),
//! * exposing a stable `as_str()` projection for log and round-trip,
//! * implementing the `contains(...)` predicate used by the enforcer.
//!
//! Construction is fallible — invalid inputs are caught at manifest
//! parse time so the runtime never has to reason about half-formed
//! scopes on the hot path.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::CapabilityParseError;

/// A filesystem prefix scope. Always **absolute** and **lexically
/// normalised** (no `.`, no `..`). The normalisation runs at
/// construction so the enforcer's matching logic stays a cheap byte
/// comparison on the hot path.
///
/// > The scope is matched **lexically**, not via
/// > [`std::fs::canonicalize`]. Symlinks are part of the trusted
/// > directory layout — operators who arrange a writable symlink
/// > pointing into a denied area have already lost. See
/// > `docs/plugins/security.md`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PathScope(PathBuf);

impl PathScope {
    /// Build a scope from a raw path string. Empty input maps to the
    /// root scope (`/`), matching the legacy `fs-read` token shape.
    /// Returns an error when the input contains `..` or other
    /// disallowed components.
    pub fn parse(raw: &str) -> Result<Self, CapabilityParseError> {
        let trimmed = raw.trim();
        let candidate = if trimmed.is_empty() {
            PathBuf::from("/")
        } else {
            PathBuf::from(trimmed)
        };
        if !candidate.is_absolute() {
            return Err(CapabilityParseError::PathNotAbsolute(trimmed.to_owned()));
        }
        // Walk components to reject ".." and "." segments. Allowing
        // `Component::ParentDir` here would let a manifest dodge a
        // sibling grant via `fs.read:/etc/../home` and the operator
        // staring at the manifest would never notice.
        for comp in candidate.components() {
            match comp {
                Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {}
                Component::CurDir | Component::ParentDir => {
                    return Err(CapabilityParseError::PathTraversal(trimmed.to_owned()));
                }
            }
        }
        Ok(Self(candidate))
    }

    /// The wildcard scope (`/`). Used by the parser when a legacy
    /// unit-form capability is encountered, so plugins built against
    /// keep loading.
    #[must_use]
    pub fn root() -> Self {
        Self(PathBuf::from("/"))
    }

    /// The scope as the canonical manifest string. Slashes are
    /// preserved as written; the round-trip via [`PathScope::parse`]
    /// is lossless.
    #[must_use]
    pub fn as_str(&self) -> std::borrow::Cow<'_, str> {
        self.0.to_string_lossy()
    }

    /// The scope as a borrowed `Path`.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// True when `query` is reachable under this scope.
    ///
    /// The match is **prefix-on-components**, not byte-prefix. So
    /// `fs.read:/etc` allows `/etc/passwd` but NOT `/etcd-data/x`.
    /// `query` is also walked for `..` segments — the enforcer
    /// rejects traversal attempts before consulting the cache.
    pub fn contains(&self, query: &Path) -> bool {
        if !query.is_absolute() {
            return false;
        }
        for comp in query.components() {
            // A query containing ParentDir means the plugin is
            // trying to escape its declared prefix; refuse before
            // matching.
            if matches!(comp, Component::ParentDir) {
                return false;
            }
        }
        let mut q_iter = query.components();
        for s_comp in self.0.components() {
            match q_iter.next() {
                Some(q_comp) if q_comp == s_comp => {}
                _ => return false,
            }
        }
        true
    }
}

/// A `host:port` pair. `port` is optional — `None` means "any port
/// on this host". `host` is lowercased on parse so case mismatches
/// don't sneak past the enforcer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HostPort {
    /// Lowercased host name or `*` for the wildcard scope.
    pub host: String,
    /// Optional explicit port. `None` matches any.
    pub port: Option<u16>,
}

impl HostPort {
    /// Parse a `host[:port]` string. The wildcard `*` matches any
    /// host (used as a backwards-compat shim for the bare `net`
    /// token).
    pub fn parse(raw: &str) -> Result<Self, CapabilityParseError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(CapabilityParseError::EmptyHost);
        }
        if let Some((host, port)) = trimmed.rsplit_once(':') {
            let port: u16 = port
                .parse()
                .map_err(|_| CapabilityParseError::InvalidPort(port.to_owned()))?;
            Ok(Self {
                host: host.to_ascii_lowercase(),
                port: Some(port),
            })
        } else {
            Ok(Self {
                host: trimmed.to_ascii_lowercase(),
                port: None,
            })
        }
    }

    /// Wildcard host/port — legacy `net` token expands to this.
    #[must_use]
    pub fn wildcard() -> Self {
        Self {
            host: "*".to_owned(),
            port: None,
        }
    }

    /// Canonical manifest projection. `*` and no-port forms are
    /// preserved.
    #[must_use]
    pub fn as_str(&self) -> String {
        match self.port {
            Some(p) => format!("{}:{p}", self.host),
            None => self.host.clone(),
        }
    }

    /// True when `host:port` is covered by this scope.
    pub fn matches(&self, host: &str, port: u16) -> bool {
        let host_ok = self.host == "*" || self.host.eq_ignore_ascii_case(host);
        let port_ok = self.port.is_none_or(|p| p == port);
        host_ok && port_ok
    }
}

/// An environment-variable name scope. `*` is the wildcard for the
/// legacy bare `env` token.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EnvVar(String);

impl EnvVar {
    /// Parse a variable name. Empty input is rejected; the wildcard
    /// `*` is permitted to support legacy manifests.
    pub fn parse(raw: &str) -> Result<Self, CapabilityParseError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(CapabilityParseError::EmptyEnvVar);
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// Wildcard scope — legacy `env` expands to this.
    #[must_use]
    pub fn wildcard() -> Self {
        Self("*".to_owned())
    }

    /// Canonical string projection.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True when this scope grants access to a concrete variable.
    pub fn matches(&self, var: &str) -> bool {
        self.0 == "*" || self.0 == var
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_scope_rejects_relative_input() {
        assert!(matches!(
            PathScope::parse("etc"),
            Err(CapabilityParseError::PathNotAbsolute(_))
        ));
    }

    #[test]
    fn path_scope_rejects_parent_segments() {
        assert!(matches!(
            PathScope::parse("/etc/../home"),
            Err(CapabilityParseError::PathTraversal(_))
        ));
    }

    #[test]
    fn path_scope_contains_component_prefix_only() {
        let scope = PathScope::parse("/etc").unwrap();
        assert!(scope.contains(Path::new("/etc")));
        assert!(scope.contains(Path::new("/etc/passwd")));
        assert!(!scope.contains(Path::new("/etcd-data/x")));
        assert!(!scope.contains(Path::new("/home")));
    }

    #[test]
    fn path_scope_refuses_traversal_in_query() {
        let scope = PathScope::parse("/etc").unwrap();
        assert!(!scope.contains(Path::new("/etc/../home/x")));
    }

    #[test]
    fn host_port_parses_split_form() {
        let hp = HostPort::parse("Example.COM:443").unwrap();
        assert_eq!(hp.host, "example.com");
        assert_eq!(hp.port, Some(443));
        assert!(hp.matches("example.com", 443));
        assert!(!hp.matches("example.com", 80));
    }

    #[test]
    fn host_port_wildcard_matches_any() {
        let hp = HostPort::wildcard();
        assert!(hp.matches("anything.test", 1));
    }

    #[test]
    fn host_port_no_port_matches_any_port() {
        let hp = HostPort::parse("api.example.com").unwrap();
        assert!(hp.matches("api.example.com", 80));
        assert!(hp.matches("api.example.com", 443));
        assert!(!hp.matches("other.test", 80));
    }

    #[test]
    fn env_var_wildcard_matches_any() {
        assert!(EnvVar::wildcard().matches("HOME"));
        assert!(EnvVar::wildcard().matches("PATH"));
    }

    #[test]
    fn env_var_specific_matches_one() {
        let v = EnvVar::parse("HOME").unwrap();
        assert!(v.matches("HOME"));
        assert!(!v.matches("PATH"));
    }
}
