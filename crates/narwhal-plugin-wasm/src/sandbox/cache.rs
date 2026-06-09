//! Decision cache for the enforcer hot path.
//!
//! The cache is keyed on the [`Operation::cache_key`] string — a
//! stable projection of the operation's kind + argument. Cached
//! values are `bool` (allow / deny). Denials are also cached so a
//! plugin that repeatedly tries the same blocked operation doesn't
//! re-walk the grants set every time; the audit log only emits on
//! the *first* denial of each operation.
//!
//! `RwLock` keeps reads cheap. The cache is per-plugin (stored on
//! [`crate::HostState`]) so cross-plugin grants never collide and
//! reset is implicit when a plugin is reloaded.

use std::collections::HashMap;
use std::sync::RwLock;

use super::decision::AuditId;

/// Cached verdict from a prior enforcer check. `Deny` always carries the
/// original audit id so repeat probes reference it without re-emission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CachedDecision {
    Allow,
    Deny { audit_id: AuditId },
}

/// Hot-path cache mapping operation cache keys → decisions.
#[derive(Debug, Default)]
pub struct DecisionCache {
    inner: RwLock<HashMap<String, CachedDecision>>,
}

impl DecisionCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a cached decision. Returns `None` on first contact
    /// (caller computes, then stores).
    pub(crate) fn get(&self, key: &str) -> Option<CachedDecision> {
        self.inner
            .read()
            .ok()
            .and_then(|guard| guard.get(key).copied())
    }

    /// Store an `Allow` decision.
    pub(crate) fn record_allow(&self, key: &str) {
        if let Ok(mut guard) = self.inner.write() {
            guard.insert(key.to_owned(), CachedDecision::Allow);
        }
    }

    /// Store a `Deny` decision with the audit id of the first
    /// denial so repeat probes can reference it.
    pub(crate) fn record_deny(&self, key: &str, audit_id: AuditId) {
        if let Ok(mut guard) = self.inner.write() {
            guard.insert(key.to_owned(), CachedDecision::Deny { audit_id });
        }
    }

    /// Number of cached entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.read().map_or(0, |g| g.len())
    }

    /// True when the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drop all entries. Hosts call this when the plugin's grants
    /// have been changed at runtime so stale allows can't survive.
    pub fn clear(&self) {
        if let Ok(mut guard) = self.inner.write() {
            guard.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn miss_returns_none() {
        let c = DecisionCache::new();
        assert!(c.get("nope").is_none());
        assert!(c.is_empty());
    }

    #[test]
    fn record_and_replay_allow() {
        let c = DecisionCache::new();
        c.record_allow("k");
        let v = c.get("k").unwrap();
        assert!(matches!(v, CachedDecision::Allow));
    }

    #[test]
    fn record_deny_carries_audit_id() {
        let c = DecisionCache::new();
        let id = AuditId::next();
        c.record_deny("k", id);
        let v = c.get("k").unwrap();
        match v {
            CachedDecision::Deny { audit_id } => assert_eq!(audit_id, id),
            CachedDecision::Allow => panic!("expected deny"),
        }
    }

    #[test]
    fn clear_resets() {
        let c = DecisionCache::new();
        c.record_allow("a");
        c.record_allow("b");
        assert_eq!(c.len(), 2);
        c.clear();
        assert!(c.is_empty());
    }
}
