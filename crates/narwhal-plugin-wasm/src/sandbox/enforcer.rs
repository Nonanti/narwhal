//! [`Enforcer`] — the per-call policy guard.
//!
//! Embedders rarely implement this trait themselves; the runtime
//! ships [`StandardEnforcer`] which evaluates against the plugin's
//! effective [`crate::capability::CapabilitySet`] + the active
//! [`crate::capability::Grants`]. Custom enforcers are useful in
//! tests when you want to assert *which* path the host took (e.g.
//! denying every operation to verify the trap path).

use crate::capability::{Capability, CapabilityKind, CapabilitySet};

use super::audit::{AuditSink, record};
use super::cache::{CachedDecision, DecisionCache};
use super::decision::Decision;
use super::operation::Operation;

/// Policy guard the host-function entry points call before doing
/// any work. Decisions are cheap to make and can be cached;
/// implementations are encouraged to (the bundled
/// [`StandardEnforcer`] does so via [`DecisionCache`]).
pub trait Enforcer: Send + Sync {
    /// Evaluate `op` against the effective set for `plugin`. Returning
    /// [`Decision::Deny`] causes the host function to surface a
    /// wasmtime trap (or, for state-write operations, drop the call
    /// silently — see the host-fn implementation for the policy
    /// matrix).
    fn check(&self, plugin: &str, op: &Operation) -> Decision;
}

/// Default enforcer: walks the plugin's effective set and matches
/// each operation against it. Decisions are cached per-operation so
/// the steady-state cost is a `HashMap` lookup.
///
/// One enforcer is created per [`crate::WasmPlugin`] — sharing the
/// same `StandardEnforcer` across plugins would conflate cache
/// entries (one plugin's allow becoming another's). The bundled
/// [`crate::Runtime`] handles construction; embedders that need
/// their own [`Enforcer`] inject it via [`crate::HostState::with_enforcer`].
pub struct StandardEnforcer {
    /// The plugin's effective set (manifest ∩ grants).
    effective: CapabilitySet,
    /// Hot-path cache.
    cache: DecisionCache,
    /// Audit sink; default is [`super::TracingAuditSink`].
    audit: std::sync::Arc<dyn AuditSink>,
    /// When true, an explicit `Cmd` grant covers every command name.
    broad_cmd: bool,
}

impl StandardEnforcer {
    /// Build an enforcer with the given effective set and an audit
    /// sink. `broad_cmd` mirrors [`crate::capability::Grants::broad_cmd`]
    /// — passing it in saves a back-reference to `Grants` from every
    /// host fn entry.
    #[must_use]
    pub fn new(
        effective: CapabilitySet,
        audit: std::sync::Arc<dyn AuditSink>,
        broad_cmd: bool,
    ) -> Self {
        Self {
            effective,
            cache: DecisionCache::new(),
            audit,
            broad_cmd,
        }
    }

    /// Borrow the cached decision count. Used by tests to verify
    /// the hot path actually warms.
    #[must_use]
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Borrow the underlying effective set. Used by tests and
    /// diagnostics.
    #[must_use]
    pub const fn effective(&self) -> &CapabilitySet {
        &self.effective
    }

    /// Walk the effective set for a matching token. Pure function
    /// — no logging side-effects, so the cache can short-circuit it.
    fn evaluate(&self, op: &Operation) -> (bool, &'static str) {
        match op {
            Operation::StateAccess => {
                if self.effective.has_kind(CapabilityKind::State) {
                    (true, "")
                } else {
                    (false, "manifest does not declare 'state' capability")
                }
            }
            Operation::CmdInvoke { name } => {
                if self.broad_cmd && self.effective.contains(&Capability::Cmd) {
                    return (true, "");
                }
                if self.effective.contains(&Capability::Cmd) {
                    // Even without `broad_cmd`, a literal `Cmd` in
                    // the effective set is a backwards-compat
                    // catch-all. Operators turning broad_cmd off get
                    // an extra defence by also removing the bare
                    // `Cmd` from grants.
                    return (true, "");
                }
                if self
                    .effective
                    .contains(&Capability::CmdInvoke(name.clone()))
                {
                    return (true, "");
                }
                (false, "no matching cmd.invoke grant")
            }
            Operation::FsRead { path } => self.match_fs(CapabilityKind::FsRead, path),
            Operation::FsWrite { path } => self.match_fs(CapabilityKind::FsWrite, path),
            Operation::NetConnect { host, port } => self.match_net(host, *port),
            Operation::EnvRead { var } => self.match_env(var),
        }
    }

    fn match_fs(&self, kind: CapabilityKind, path: &std::path::Path) -> (bool, &'static str) {
        // Refuse any traversal segments before scanning grants so
        // the plugin can't smuggle `..` past a /etc grant.
        for comp in path.components() {
            if matches!(comp, std::path::Component::ParentDir) {
                return (false, "path contains traversal segment");
            }
        }
        let any = self.effective.of_kind(kind).any(|c| match c {
            Capability::FsRead(scope) | Capability::FsWrite(scope) => scope.contains(path),
            _ => false,
        });
        if any {
            (true, "")
        } else {
            (false, "no matching fs grant")
        }
    }

    fn match_net(&self, host: &str, port: u16) -> (bool, &'static str) {
        let any = self
            .effective
            .of_kind(CapabilityKind::NetConnect)
            .any(|c| match c {
                Capability::NetConnect(granted) => {
                    let host_ok = granted.host == "*" || granted.host.eq_ignore_ascii_case(host);
                    let port_ok = granted.port.is_none() || granted.port == Some(port);
                    host_ok && port_ok
                }
                _ => false,
            });
        if any {
            (true, "")
        } else {
            (false, "no matching net.connect grant")
        }
    }

    fn match_env(&self, var: &str) -> (bool, &'static str) {
        let any = self
            .effective
            .of_kind(CapabilityKind::EnvRead)
            .any(|c| matches!(c, Capability::EnvRead(granted) if granted.matches(var)));
        if any {
            (true, "")
        } else {
            (false, "no matching env.read grant")
        }
    }
}

impl Enforcer for StandardEnforcer {
    fn check(&self, plugin: &str, op: &Operation) -> Decision {
        let key = op.cache_key();
        // Fast path: cache hit.
        if let Some(cached) = self.cache.get(&key) {
            return match cached {
                CachedDecision::Allow => Decision::Allow,
                CachedDecision::Deny { audit_id } => Decision::Deny {
                    kind: op.kind(),
                    reason: "cached denial".to_owned(),
                    audit_id,
                },
            };
        }
        // Slow path: evaluate, log, cache.
        let (allowed, reason) = self.evaluate(op);
        if allowed {
            self.cache.record_allow(&key);
            Decision::Allow
        } else {
            let audit_id = record(self.audit.as_ref(), plugin, op, reason);
            self.cache.record_deny(&key, audit_id);
            Decision::Deny {
                kind: op.kind(),
                reason: reason.to_owned(),
                audit_id,
            }
        }
    }
}

impl std::fmt::Debug for StandardEnforcer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `audit` is intentionally omitted (it is a trait object;
        // a real impl pointer in the debug output is noise) — hence
        // `finish_non_exhaustive`.
        f.debug_struct("StandardEnforcer")
            .field("effective_len", &self.effective.len())
            .field("cache_len", &self.cache.len())
            .field("broad_cmd", &self.broad_cmd)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::audit::RecordingAuditSink;
    use super::*;
    use crate::capability::{EnvVar, HostPort, PathScope};

    fn enforcer_with(caps: Vec<Capability>) -> (StandardEnforcer, Arc<RecordingAuditSink>) {
        let audit = Arc::new(RecordingAuditSink::new());
        let enf = StandardEnforcer::new(
            CapabilitySet::from_caps(caps),
            audit.clone() as Arc<dyn AuditSink>,
            false,
        );
        (enf, audit)
    }

    #[test]
    fn state_access_allowed_when_state_in_set() {
        let (enf, audit) = enforcer_with(vec![Capability::State]);
        let d = enf.check("p", &Operation::StateAccess);
        assert!(d.is_allowed());
        assert!(audit.is_empty());
    }

    #[test]
    fn state_access_denied_when_state_absent() {
        let (enf, audit) = enforcer_with(vec![]);
        let d = enf.check("p", &Operation::StateAccess);
        match d {
            Decision::Deny { kind, .. } => assert_eq!(kind, CapabilityKind::State),
            Decision::Allow => panic!("expected deny"),
        }
        assert_eq!(audit.len(), 1);
    }

    #[test]
    fn fs_read_traversal_query_is_denied() {
        let (enf, audit) =
            enforcer_with(vec![Capability::FsRead(PathScope::parse("/etc").unwrap())]);
        let d = enf.check(
            "p",
            &Operation::FsRead {
                path: std::path::PathBuf::from("/etc/../home/.ssh"),
            },
        );
        assert!(!d.is_allowed(), "traversal must be denied");
        let event = &audit.snapshot()[0];
        assert!(event.reason.contains("traversal"));
    }

    #[test]
    fn fs_read_scoped_grant_covers_child() {
        let (enf, _) = enforcer_with(vec![Capability::FsRead(PathScope::parse("/etc").unwrap())]);
        let d = enf.check(
            "p",
            &Operation::FsRead {
                path: std::path::PathBuf::from("/etc/passwd"),
            },
        );
        assert!(d.is_allowed());
    }

    #[test]
    fn fs_read_scoped_grant_denies_sibling() {
        let (enf, _) = enforcer_with(vec![Capability::FsRead(PathScope::parse("/etc").unwrap())]);
        let d = enf.check(
            "p",
            &Operation::FsRead {
                path: std::path::PathBuf::from("/home/.ssh"),
            },
        );
        assert!(!d.is_allowed());
    }

    #[test]
    fn net_grant_matches_specific_port() {
        let (enf, _) = enforcer_with(vec![Capability::NetConnect(
            HostPort::parse("api.test:443").unwrap(),
        )]);
        let allow = enf.check(
            "p",
            &Operation::NetConnect {
                host: "api.test".into(),
                port: 443,
            },
        );
        assert!(allow.is_allowed());
        let deny = enf.check(
            "p",
            &Operation::NetConnect {
                host: "api.test".into(),
                port: 80,
            },
        );
        assert!(!deny.is_allowed());
    }

    #[test]
    fn env_grant_specific_var_only() {
        let (enf, _) = enforcer_with(vec![Capability::EnvRead(EnvVar::parse("HOME").unwrap())]);
        assert!(
            enf.check("p", &Operation::EnvRead { var: "HOME".into() })
                .is_allowed()
        );
        assert!(
            !enf.check("p", &Operation::EnvRead { var: "PATH".into() })
                .is_allowed()
        );
    }

    #[test]
    fn cmd_invoke_with_explicit_grant() {
        let (enf, _) = enforcer_with(vec![Capability::CmdInvoke("run".into())]);
        let allow = enf.check("p", &Operation::CmdInvoke { name: "run".into() });
        assert!(allow.is_allowed());
        let deny = enf.check(
            "p",
            &Operation::CmdInvoke {
                name: "delete".into(),
            },
        );
        assert!(!deny.is_allowed());
    }

    #[test]
    fn cmd_invoke_with_bare_cmd_grant_covers_all() {
        let (enf, _) = enforcer_with(vec![Capability::Cmd]);
        let d = enf.check(
            "p",
            &Operation::CmdInvoke {
                name: "anything".into(),
            },
        );
        assert!(d.is_allowed());
    }

    #[test]
    fn cache_hits_skip_audit_on_repeated_deny() {
        let (enf, audit) = enforcer_with(vec![]);
        let op = Operation::FsRead {
            path: std::path::PathBuf::from("/etc"),
        };
        let first = enf.check("p", &op);
        let second = enf.check("p", &op);
        assert!(!first.is_allowed());
        assert!(!second.is_allowed());
        // Only ONE audit emission for the same operation key.
        assert_eq!(audit.len(), 1);
        // Both decisions reference the same audit id.
        assert_eq!(first.audit_id(), second.audit_id());
    }

    #[test]
    fn cache_warms_on_allow() {
        let (enf, _) = enforcer_with(vec![Capability::State]);
        assert_eq!(enf.cache_len(), 0);
        enf.check("p", &Operation::StateAccess);
        enf.check("p", &Operation::StateAccess);
        assert_eq!(enf.cache_len(), 1);
    }
}
