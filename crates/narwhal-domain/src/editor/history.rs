//! Undo / redo stack for [`super::EditorBuffer`].
//!
//! Every mutation that the buffer wants reversible records an
//! [`EditOp`] on the [`EditHistory`]. The history holds two stacks
//! (undo + redo) with a capacity bound so a runaway insert loop
//! cannot blow heap; the oldest entries are dropped first.
//!
//! Compound edits — for example "delete a multi-line selection and
//! type the replacement" — are wrapped in [`EditOp::Compound`] so a
//! single `Ctrl+Z` reverts the whole intent rather than the
//! individual primitives. The buffer wraps a [`HistoryTxn`] guard
//! around such operations: on drop, the recorded primitives become
//! one compound entry.
//!
//! The history is **mode-agnostic** — vim, basic and emacs handlers
//! all funnel their mutations through the same buffer methods, so a
//! buffer edited in vim then continued in basic mode still undoes
//! cleanly because the stack lives on the buffer, not on the mode
//! state machine.

use super::selection::Position;

/// A reversible primitive edit. `before` / `after` captures hold the
/// text that has to be re-applied (redo) or rolled back (undo); the
/// position is the *start* of the edit so the cursor can be
/// restored.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EditOp {
    /// Inserted `text` at `at`. Undo deletes the same range.
    Insert {
        at: Position,
        text: String,
        cursor_before: Position,
        cursor_after: Position,
    },
    /// Deleted `text` starting at `at`. Undo re-inserts.
    Delete {
        at: Position,
        text: String,
        cursor_before: Position,
        cursor_after: Position,
    },
    /// Replaced `before` with `after` over a contiguous range
    /// starting at `at`. Covers selection-replace and search/replace.
    Replace {
        at: Position,
        before: String,
        after: String,
        cursor_before: Position,
        cursor_after: Position,
    },
    /// Atomic group: undoing the whole group restores the pre-group
    /// state, redoing replays in order. Always contains at least one
    /// child; the buffer's transaction guard skips emitting an empty
    /// compound.
    Compound(Vec<Self>),
}

impl EditOp {
    /// Cursor position captured before the edit. For a compound op
    /// this is the first child's `cursor_before`.
    #[must_use]
    pub fn cursor_before(&self) -> Position {
        match self {
            Self::Insert { cursor_before, .. }
            | Self::Delete { cursor_before, .. }
            | Self::Replace { cursor_before, .. } => *cursor_before,
            Self::Compound(children) => children.first().map_or((0, 0), Self::cursor_before),
        }
    }

    /// Cursor position the buffer should land on after applying
    /// (redo) this op. For a compound op this is the last child's
    /// `cursor_after`.
    #[must_use]
    pub fn cursor_after(&self) -> Position {
        match self {
            Self::Insert { cursor_after, .. }
            | Self::Delete { cursor_after, .. }
            | Self::Replace { cursor_after, .. } => *cursor_after,
            Self::Compound(children) => children.last().map_or((0, 0), Self::cursor_after),
        }
    }
}

/// Bounded undo / redo stack.
///
/// Capacity defaults to 1000 entries; the oldest entry is dropped
/// when a new push would exceed the bound. A new mutation clears
/// the redo stack — the canonical "branch off on edit" behaviour.
#[derive(Debug, Clone)]
pub struct EditHistory {
    undo: Vec<EditOp>,
    redo: Vec<EditOp>,
    capacity: usize,
    /// Transaction depth. While `> 0`, [`Self::record`] appends to
    /// the pending compound instead of the main undo stack.
    txn_depth: u32,
    txn_pending: Vec<EditOp>,
}

impl Default for EditHistory {
    fn default() -> Self {
        Self::with_capacity(1000)
    }
}

impl EditHistory {
    /// Build an empty history with `capacity` undo entries.
    #[must_use]
    pub const fn with_capacity(capacity: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            capacity,
            txn_depth: 0,
            txn_pending: Vec::new(),
        }
    }

    /// Number of undo entries currently held.
    #[must_use]
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Number of redo entries currently held.
    #[must_use]
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// `true` when at least one undo entry is available outside an
    /// open transaction.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty() && self.txn_depth == 0
    }

    /// `true` when at least one redo entry is available.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty() && self.txn_depth == 0
    }

    /// Record a new edit. Clears the redo stack and trims the undo
    /// stack to `capacity`. While a transaction is open, the op is
    /// appended to the pending compound instead.
    pub fn record(&mut self, op: EditOp) {
        if self.txn_depth > 0 {
            self.txn_pending.push(op);
            return;
        }
        self.redo.clear();
        if self.undo.len() >= self.capacity && !self.undo.is_empty() {
            self.undo.remove(0);
        }
        self.undo.push(op);
    }

    /// Open a compound-edit transaction. Every `record` until the
    /// matching [`Self::commit`] is bundled into a single
    /// [`EditOp::Compound`]. Nested transactions are flattened.
    pub const fn begin(&mut self) {
        self.txn_depth = self.txn_depth.saturating_add(1);
    }

    /// Close the top-most transaction. If at least one op was
    /// recorded while the transaction was open, a single
    /// [`EditOp::Compound`] is pushed onto the undo stack. A
    /// transaction that recorded nothing is a no-op.
    pub fn commit(&mut self) {
        if self.txn_depth == 0 {
            return;
        }
        self.txn_depth -= 1;
        if self.txn_depth > 0 {
            // Still inside an outer transaction \u2014 keep buffering.
            return;
        }
        if self.txn_pending.is_empty() {
            return;
        }
        // Single-op transactions don't need the Compound wrapper.
        let op = if self.txn_pending.len() == 1 {
            self.txn_pending.remove(0)
        } else {
            EditOp::Compound(std::mem::take(&mut self.txn_pending))
        };
        self.txn_pending.clear();
        // Reuse the top-level record path so capacity and redo
        // semantics stay consistent.
        let prev_depth = self.txn_depth;
        self.txn_depth = 0;
        self.record(op);
        self.txn_depth = prev_depth;
    }

    /// Discard the in-flight transaction without recording anything.
    /// Used when an operation fails partway and the buffer rolls
    /// back its own state outside the history machinery.
    pub fn rollback(&mut self) {
        if self.txn_depth > 0 {
            self.txn_depth -= 1;
            if self.txn_depth == 0 {
                self.txn_pending.clear();
            }
        }
    }

    /// Pop the next undo entry. The caller is responsible for
    /// reverting the buffer state; the history just yields the op.
    /// Returns `None` when the stack is empty or a transaction is
    /// open.
    pub fn pop_undo(&mut self) -> Option<EditOp> {
        if self.txn_depth > 0 {
            return None;
        }
        let op = self.undo.pop()?;
        self.redo.push(op.clone());
        Some(op)
    }

    /// Pop the next redo entry. Mirrors [`Self::pop_undo`].
    pub fn pop_redo(&mut self) -> Option<EditOp> {
        if self.txn_depth > 0 {
            return None;
        }
        let op = self.redo.pop()?;
        self.undo.push(op.clone());
        Some(op)
    }

    /// Drop every recorded op. Used when the buffer is reset (new
    /// query, snippet load, ...).
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.txn_pending.clear();
        self.txn_depth = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ins(at: Position, text: &str, before: Position, after: Position) -> EditOp {
        EditOp::Insert {
            at,
            text: text.into(),
            cursor_before: before,
            cursor_after: after,
        }
    }

    #[test]
    fn record_clears_redo() {
        let mut h = EditHistory::default();
        h.record(ins((0, 0), "a", (0, 0), (0, 1)));
        let _ = h.pop_undo();
        assert_eq!(h.redo_len(), 1);
        h.record(ins((0, 1), "b", (0, 1), (0, 2)));
        assert_eq!(h.redo_len(), 0);
    }

    #[test]
    fn pop_undo_moves_to_redo() {
        let mut h = EditHistory::default();
        h.record(ins((0, 0), "a", (0, 0), (0, 1)));
        let op = h.pop_undo().expect("undo available");
        assert!(matches!(op, EditOp::Insert { .. }));
        assert!(h.can_redo());
        let redone = h.pop_redo().expect("redo available");
        assert!(matches!(redone, EditOp::Insert { .. }));
        assert!(h.can_undo());
    }

    #[test]
    fn capacity_drops_oldest() {
        let mut h = EditHistory::with_capacity(3);
        for i in 0..5_usize {
            h.record(ins((0, i), "x", (0, i), (0, i + 1)));
        }
        assert_eq!(h.undo_len(), 3);
        // First two were dropped, the oldest remaining op started at col 2.
        let oldest = h.pop_undo();
        let _newer = h.pop_undo();
        let _newest = h.pop_undo();
        assert!(matches!(oldest, Some(EditOp::Insert { at: (0, 4), .. })));
    }

    #[test]
    fn transaction_groups_into_compound() {
        let mut h = EditHistory::default();
        h.begin();
        h.record(ins((0, 0), "a", (0, 0), (0, 1)));
        h.record(ins((0, 1), "b", (0, 1), (0, 2)));
        h.commit();
        let op = h.pop_undo().expect("compound present");
        match op {
            EditOp::Compound(children) => assert_eq!(children.len(), 2),
            other => panic!("expected Compound, got {other:?}"),
        }
    }

    #[test]
    fn single_op_transaction_is_unwrapped() {
        let mut h = EditHistory::default();
        h.begin();
        h.record(ins((0, 0), "a", (0, 0), (0, 1)));
        h.commit();
        let op = h.pop_undo().expect("op present");
        assert!(matches!(op, EditOp::Insert { .. }));
    }

    #[test]
    fn empty_transaction_records_nothing() {
        let mut h = EditHistory::default();
        h.begin();
        h.commit();
        assert_eq!(h.undo_len(), 0);
    }

    #[test]
    fn rollback_discards_pending() {
        let mut h = EditHistory::default();
        h.begin();
        h.record(ins((0, 0), "x", (0, 0), (0, 1)));
        h.rollback();
        assert_eq!(h.undo_len(), 0);
    }

    #[test]
    fn nested_transactions_flatten() {
        let mut h = EditHistory::default();
        h.begin();
        h.record(ins((0, 0), "a", (0, 0), (0, 1)));
        h.begin();
        h.record(ins((0, 1), "b", (0, 1), (0, 2)));
        h.commit();
        h.record(ins((0, 2), "c", (0, 2), (0, 3)));
        h.commit();
        // All three children land in one outer Compound.
        let op = h.pop_undo().expect("compound present");
        match op {
            EditOp::Compound(children) => assert_eq!(children.len(), 3),
            other => panic!("expected Compound, got {other:?}"),
        }
    }

    #[test]
    fn cursor_after_uses_last_child_for_compound() {
        let mut h = EditHistory::default();
        h.begin();
        h.record(ins((0, 0), "a", (0, 0), (0, 1)));
        h.record(ins((0, 1), "b", (0, 1), (0, 2)));
        h.commit();
        let op = h.pop_undo().expect("compound");
        assert_eq!(op.cursor_after(), (0, 2));
        assert_eq!(op.cursor_before(), (0, 0));
    }
}
