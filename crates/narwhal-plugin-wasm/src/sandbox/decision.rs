//! [`Decision`] — the enforcer's verdict on one host-function call.
//!
//! Carried back to the host-function entry point which converts
//! `Decision::Deny` into a wasmtime trap so the guest can't paper
//! over the denial. `Decision::Allow` falls straight through to the
//! original logic.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::capability::CapabilityKind;

/// Per-process monotonic identifier carried by every audit event so
/// operators can correlate a denial across log lines.
///
/// A `u64` counter is enough: at 1 M denials / s a single host runs
/// for ~584 000 years before wrapping. The wrap-around lands at
/// `u64::MAX` and resets to `0` — the audit consumer sees the gap
/// rather than a collision, which is acceptable for the use case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuditId(pub u64);

impl AuditId {
    pub(crate) fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// Underlying counter value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for AuditId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "audit-{}", self.0)
    }
}

/// The enforcer's verdict.
///
/// `Decision::Deny` carries the *category* of the denied capability
/// (not the full token argument) because the audit log records the
/// argument separately and matching on the kind is the common
/// downstream pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Decision {
    /// The capability covers the requested operation. The host
    /// function continues normally.
    Allow,
    /// The capability does not cover the operation; the host should
    /// trap. `reason` is a short human-readable explanation
    /// (already structured-logged); `audit_id` correlates with the
    /// emitted audit event.
    Deny {
        kind: CapabilityKind,
        reason: String,
        audit_id: AuditId,
    },
}

impl Decision {
    /// True when the verdict is [`Decision::Allow`].
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }

    /// Borrow the audit id when this decision was a denial.
    #[must_use]
    pub const fn audit_id(&self) -> Option<AuditId> {
        match self {
            Self::Allow => None,
            Self::Deny { audit_id, .. } => Some(*audit_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_ids_are_monotonically_unique() {
        let a = AuditId::next();
        let b = AuditId::next();
        assert!(b.get() > a.get());
    }

    #[test]
    fn allow_is_allowed() {
        assert!(Decision::Allow.is_allowed());
        assert!(Decision::Allow.audit_id().is_none());
    }

    #[test]
    fn deny_carries_audit_id() {
        let id = AuditId::next();
        let d = Decision::Deny {
            kind: CapabilityKind::FsRead,
            reason: "no scope".into(),
            audit_id: id,
        };
        assert!(!d.is_allowed());
        assert_eq!(d.audit_id(), Some(id));
    }
}
