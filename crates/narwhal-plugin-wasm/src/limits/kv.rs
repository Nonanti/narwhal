//! Per-plugin KV byte-budget accounting.
//!
//! Used by the host-fn entry point for `state-set`. The accounting
//! lives on its own type (rather than two raw `usize` fields on
//! [`crate::HostState`]) so the budget math is testable in
//! isolation and the host fn stays focused on the WIT shim.

/// Outcome of a charge attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KvOutcome {
    /// Charge succeeded; the new byte total is in `used`.
    Accepted { used: usize },
    /// Charge would overrun the budget; nothing was mutated.
    Rejected { projected: usize, budget: usize },
}

/// KV byte budget tracker. Cheap `Copy` — pass by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KvAccount {
    used: usize,
    budget: usize,
}

impl KvAccount {
    /// Build an empty account with the given budget.
    #[must_use]
    pub const fn new(budget: usize) -> Self {
        Self { used: 0, budget }
    }

    /// Bytes currently accounted for.
    #[must_use]
    pub const fn used(&self) -> usize {
        self.used
    }

    /// Account budget.
    #[must_use]
    pub const fn budget(&self) -> usize {
        self.budget
    }

    /// Compute the outcome of replacing a `prev_len`-byte value with
    /// a `new_len`-byte value at the same key. Returns the new total
    /// without mutating; callers `commit` only after the wasmtime
    /// trap path has cleared.
    ///
    /// Using `saturating_sub` is paranoid — the only way to get a
    /// negative delta is a bookkeeping bug, and silently overflowing
    /// to `usize::MAX` would silently disable the budget.
    #[must_use]
    pub const fn project(&self, prev_len: usize, new_len: usize) -> KvOutcome {
        let after_remove = self.used.saturating_sub(prev_len);
        let projected = after_remove.saturating_add(new_len);
        if projected > self.budget {
            KvOutcome::Rejected {
                projected,
                budget: self.budget,
            }
        } else {
            KvOutcome::Accepted { used: projected }
        }
    }

    /// Commit a previously-projected `Accepted` outcome.
    pub const fn commit(&mut self, outcome: KvOutcome) {
        if let KvOutcome::Accepted { used } = outcome {
            self.used = used;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_first_write_within_budget() {
        let acct = KvAccount::new(16);
        assert_eq!(acct.project(0, 4), KvOutcome::Accepted { used: 4 });
    }

    #[test]
    fn project_replace_with_same_size() {
        let mut acct = KvAccount::new(16);
        acct.commit(KvOutcome::Accepted { used: 4 });
        assert_eq!(acct.project(4, 4), KvOutcome::Accepted { used: 4 });
    }

    #[test]
    fn project_overrun_is_rejected() {
        let acct = KvAccount::new(4);
        assert_eq!(
            acct.project(0, 5),
            KvOutcome::Rejected {
                projected: 5,
                budget: 4
            }
        );
    }

    #[test]
    fn commit_advances_used() {
        let mut acct = KvAccount::new(16);
        let out = acct.project(0, 4);
        acct.commit(out);
        assert_eq!(acct.used(), 4);
    }

    #[test]
    fn commit_rejected_does_not_mutate() {
        let mut acct = KvAccount::new(4);
        let out = acct.project(0, 5);
        acct.commit(out);
        assert_eq!(acct.used(), 0);
    }
}
