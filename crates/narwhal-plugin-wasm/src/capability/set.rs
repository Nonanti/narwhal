//! Ordered capability collections and host-side grants.
//!
//! Three closely-related types live here:
//!
//! * [`CapabilitySet`] — the *requested* set from a plugin manifest.
//!   Sorted so iteration order is deterministic.
//! * [`Grants`] — the *granted* set the host injects into the
//!   runtime. Constructed from settings (or built explicitly by
//!   embedders); see [`Grants::default`] for the default-deny
//!   posture.
//! * [`CapabilityKind`] — the variant-less projection used by the
//!   audit log and the [`crate::sandbox::Enforcer`] cache keys.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::Capability;

/// The kind of a [`Capability`] — the same enum without arguments.
///
/// Used by:
///
/// * the audit log (so warn lines aren't cluttered with long path
///   prefixes when the operator only cares about which *category*
///   of capability was denied), and
/// * the decision-cache key in [`crate::sandbox`] (so a cache miss
///   on `fs.read:/etc/a` doesn't poison the entry for
///   `fs.read:/etc/b`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum CapabilityKind {
    State,
    Cmd,
    CmdInvoke,
    FsRead,
    FsWrite,
    NetConnect,
    EnvRead,
}

impl CapabilityKind {
    /// Short stable identifier used in tracing fields.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Cmd => "cmd",
            Self::CmdInvoke => "cmd.invoke",
            Self::FsRead => "fs.read",
            Self::FsWrite => "fs.write",
            Self::NetConnect => "net.connect",
            Self::EnvRead => "env.read",
        }
    }
}

impl std::fmt::Display for CapabilityKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A sorted, de-duplicated bag of [`Capability`] tokens.
///
/// The manifest parser hands one of these to [`crate::Runtime::load`].
/// The runtime intersects against [`Grants`] and stashes the result
/// on the plugin's [`crate::HostState`] — the enforcer reads from
/// there on every host-function call.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapabilitySet {
    inner: BTreeSet<Capability>,
}

impl CapabilitySet {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a set from any iterator of capabilities. Duplicate
    /// tokens are silently coalesced.
    pub fn from_caps<I: IntoIterator<Item = Capability>>(iter: I) -> Self {
        Self {
            inner: iter.into_iter().collect(),
        }
    }

    /// True when an *exact* token (argument and all) is present.
    /// Path/host argument matching is the enforcer's job — this is
    /// just set membership.
    #[must_use]
    pub fn contains(&self, cap: &Capability) -> bool {
        self.inner.contains(cap)
    }

    /// True when *any* token of the given kind is present. Used to
    /// gate purely-categorical decisions like "does this plugin
    /// have *any* `state` access?".
    #[must_use]
    pub fn has_kind(&self, kind: CapabilityKind) -> bool {
        self.inner.iter().any(|c| c.kind() == kind)
    }

    pub fn insert(&mut self, cap: Capability) -> bool {
        self.inner.insert(cap)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Capability> + '_ {
        self.inner.iter()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Return the subset of tokens whose [`Capability::kind`]
    /// matches `kind`. Returned slice is in iteration order.
    pub fn of_kind(&self, kind: CapabilityKind) -> impl Iterator<Item = &Capability> + '_ {
        self.inner.iter().filter(move |c| c.kind() == kind)
    }
}

/// Host-side grants — the *outer* envelope of what a runtime allows
/// any plugin to request.
///
/// The runtime intersects the manifest-declared [`CapabilitySet`]
/// against [`Grants`] at load time. A request token survives the
/// intersection only when *some* granted token in the same kind
/// covers it (path-prefix for FS, host/port for net, exact match for
/// env / cmd / state).
///
/// ## Default-deny
///
/// [`Grants::default`] returns an empty grant — every FS/net/env
/// capability the manifest asks for is refused at load time. The
/// runtime's [`crate::WasmPlugin`] never even sees the plugin in
/// that case. Use [`Grants::open_all`] in tests when the suite
/// wants to skip the gate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Grants {
    /// Granted capability tokens. Iteration order is deterministic.
    pub inner: CapabilitySet,
    /// When true, [`Capability::State`] is granted to every plugin
    /// regardless of manifest. Defaults to `true` — the host KV is
    /// per-plugin and has no security boundary in v2.0.
    pub allow_state_default: bool,
    /// When true, [`Capability::Cmd`] is treated as covering every
    /// possible command name (legacy compat). When false the host
    /// requires explicit `cmd.invoke:<name>` tokens. Defaults to
    /// `false` — explicit is safer.
    pub broad_cmd: bool,
}

impl Grants {
    /// Default-deny grants except for `state` (the per-plugin KV
    /// has no security boundary in v2.0 — it's just a scratch area
    /// scoped to one plugin's namespace).
    #[must_use]
    pub fn deny_all() -> Self {
        Self {
            inner: CapabilitySet::new(),
            allow_state_default: true,
            broad_cmd: false,
        }
    }

    /// Open every capability category at the widest scope. **For
    /// tests only** — never construct this in production code.
    #[must_use]
    pub fn open_all() -> Self {
        use super::scope::{EnvVar, HostPort, PathScope};
        Self {
            inner: CapabilitySet::from_caps([
                Capability::State,
                Capability::Cmd,
                Capability::FsRead(PathScope::root()),
                Capability::FsWrite(PathScope::root()),
                Capability::NetConnect(HostPort::wildcard()),
                Capability::EnvRead(EnvVar::wildcard()),
            ]),
            allow_state_default: true,
            broad_cmd: true,
        }
    }

    /// Build grants from any iterator of capabilities.
    pub fn from_caps<I: IntoIterator<Item = Capability>>(iter: I) -> Self {
        Self {
            inner: CapabilitySet::from_caps(iter),
            ..Self::default()
        }
    }

    /// True when this grant set covers `cap`.
    ///
    /// Kind-specific coverage:
    ///
    /// * `State` — always allowed when [`Grants::allow_state_default`]
    ///   is set, else exact match.
    /// * `Cmd` — allowed when [`Grants::broad_cmd`] is set or an
    ///   explicit `Cmd` token is in the inner set.
    /// * `CmdInvoke(name)` — covered by an explicit
    ///   `CmdInvoke(name)` or by an explicit `Cmd` / `broad_cmd`.
    /// * `FsRead(path)` / `FsWrite(path)` — covered when *any*
    ///   granted same-kind token's [`super::PathScope`] contains
    ///   `path`.
    /// * `NetConnect(host, port)` — covered when *any* granted
    ///   same-kind token's [`super::HostPort`] matches.
    /// * `EnvRead(var)` — covered when *any* granted same-kind
    ///   token's [`super::EnvVar`] matches.
    #[must_use]
    pub fn covers(&self, cap: &Capability) -> bool {
        match cap {
            Capability::State => self.allow_state_default || self.inner.contains(cap),
            Capability::Cmd => self.broad_cmd || self.inner.contains(cap),
            Capability::CmdInvoke(name) => {
                if self.broad_cmd || self.inner.contains(&Capability::Cmd) {
                    return true;
                }
                self.inner.contains(&Capability::CmdInvoke(name.clone()))
            }
            Capability::FsRead(scope) => self.inner.of_kind(CapabilityKind::FsRead).any(
                |c| matches!(c, Capability::FsRead(granted) if granted.contains(scope.as_path())),
            ),
            Capability::FsWrite(scope) => self.inner.of_kind(CapabilityKind::FsWrite).any(
                |c| matches!(c, Capability::FsWrite(granted) if granted.contains(scope.as_path())),
            ),
            Capability::NetConnect(hp) => {
                self.inner
                    .of_kind(CapabilityKind::NetConnect)
                    .any(|c| match c {
                        Capability::NetConnect(granted) => {
                            let host_ok =
                                granted.host == "*" || granted.host.eq_ignore_ascii_case(&hp.host);
                            let port_ok = granted.port.is_none() || granted.port == hp.port;
                            host_ok && port_ok
                        }
                        _ => false,
                    })
            }
            Capability::EnvRead(var) => self.inner.of_kind(CapabilityKind::EnvRead).any(
                |c| matches!(c, Capability::EnvRead(granted) if granted.matches(var.as_str())),
            ),
        }
    }

    /// Validate that every entry in `requested` is covered. Returns
    /// the first uncovered token for a precise error message.
    pub fn intersect<'a>(
        &self,
        requested: &'a CapabilitySet,
    ) -> Result<CapabilitySet, &'a Capability> {
        for cap in requested.iter() {
            if !self.covers(cap) {
                return Err(cap);
            }
        }
        Ok(requested.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::super::scope::{EnvVar, HostPort, PathScope};
    use super::*;

    #[test]
    fn empty_set_is_empty() {
        let s = CapabilitySet::new();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn has_kind_matches_any_argument() {
        let s = CapabilitySet::from_caps([Capability::FsRead(PathScope::parse("/etc").unwrap())]);
        assert!(s.has_kind(CapabilityKind::FsRead));
        assert!(!s.has_kind(CapabilityKind::FsWrite));
    }

    #[test]
    fn deny_all_blocks_fs() {
        let grants = Grants::deny_all();
        let req = Capability::FsRead(PathScope::parse("/etc").unwrap());
        assert!(!grants.covers(&req));
    }

    #[test]
    fn open_all_covers_every_kind() {
        let grants = Grants::open_all();
        for cap in [
            Capability::State,
            Capability::Cmd,
            Capability::FsRead(PathScope::parse("/anywhere").unwrap()),
            Capability::FsWrite(PathScope::parse("/tmp").unwrap()),
            Capability::NetConnect(HostPort::parse("example.com:443").unwrap()),
            Capability::EnvRead(EnvVar::parse("HOME").unwrap()),
        ] {
            assert!(grants.covers(&cap), "open_all should cover {cap:?}");
        }
    }

    #[test]
    fn path_grant_covers_child_only() {
        let grants = Grants::from_caps([Capability::FsRead(PathScope::parse("/etc").unwrap())]);
        assert!(grants.covers(&Capability::FsRead(
            PathScope::parse("/etc/passwd").unwrap()
        )));
        assert!(!grants.covers(&Capability::FsRead(PathScope::parse("/home").unwrap())));
    }

    #[test]
    fn fs_grant_does_not_cover_other_kind() {
        let grants = Grants::from_caps([Capability::FsRead(PathScope::parse("/").unwrap())]);
        assert!(
            !grants.covers(&Capability::FsWrite(PathScope::parse("/tmp").unwrap())),
            "fs.read should not imply fs.write"
        );
    }

    #[test]
    fn cmd_invoke_covered_by_broad_cmd() {
        let mut grants = Grants::deny_all();
        grants.broad_cmd = true;
        assert!(grants.covers(&Capability::CmdInvoke("run".into())));
    }

    #[test]
    fn cmd_invoke_covered_by_explicit_token() {
        let grants = Grants::from_caps([Capability::CmdInvoke("run".into())]);
        assert!(grants.covers(&Capability::CmdInvoke("run".into())));
        assert!(!grants.covers(&Capability::CmdInvoke("delete".into())));
    }

    #[test]
    fn net_grant_specific_port() {
        let grants = Grants::from_caps([Capability::NetConnect(
            HostPort::parse("api.test:443").unwrap(),
        )]);
        assert!(grants.covers(&Capability::NetConnect(
            HostPort::parse("api.test:443").unwrap()
        )));
        assert!(!grants.covers(&Capability::NetConnect(
            HostPort::parse("api.test:80").unwrap()
        )));
    }

    #[test]
    fn net_grant_any_port_when_unspecified() {
        let grants =
            Grants::from_caps([Capability::NetConnect(HostPort::parse("api.test").unwrap())]);
        assert!(grants.covers(&Capability::NetConnect(
            HostPort::parse("api.test:80").unwrap()
        )));
        assert!(grants.covers(&Capability::NetConnect(
            HostPort::parse("api.test:443").unwrap()
        )));
    }

    #[test]
    fn intersect_returns_first_uncovered() {
        let grants = Grants::from_caps([Capability::FsRead(PathScope::parse("/etc").unwrap())]);
        let req = CapabilitySet::from_caps([
            Capability::FsRead(PathScope::parse("/etc/passwd").unwrap()),
            Capability::NetConnect(HostPort::parse("api.test").unwrap()),
        ]);
        match grants.intersect(&req) {
            Err(cap) => assert!(matches!(cap, Capability::NetConnect(_))),
            Ok(_) => panic!("expected denial"),
        }
    }

    #[test]
    fn intersect_returns_clone_on_success() {
        let grants = Grants::open_all();
        let req = CapabilitySet::from_caps([Capability::State]);
        let effective = grants.intersect(&req).expect("covered");
        assert!(effective.contains(&Capability::State));
    }
}
