//! [`Edit`] — a buffer mutation described in terms tree-sitter understands.
//!
//! The TUI editor speaks in `(row, col)` cursor coordinates and byte
//! offsets; tree-sitter speaks in [`tree_sitter::InputEdit`]. [`Edit`]
//! is the shared shape — a `#[non_exhaustive]` plain-data struct that
//! callers build with [`Edit::with`] (Tier-0 builder pattern) and the
//! parser converts to the C-binding type at the seam.

use tree_sitter::{InputEdit, Point};

/// One buffer mutation, suitable for incremental reparse.
///
/// All offsets are **byte** offsets into the buffer *before* the
/// mutation; `(row, col)` positions are zero-based UTF-8 byte columns,
/// matching what the editor uses internally.
///
/// Build via [`Edit::with`] so the struct can grow new fields without
/// breaking call sites.
///
/// ```
/// use narwhal_sql::treesitter::Edit;
/// let edit = Edit::with(|e| {
///     e.start_byte = 8;
///     e.old_end_byte = 8;
///     e.new_end_byte = 14;
///     e.start_row = 0;
///     e.start_col = 8;
///     e.old_end_row = 0;
///     e.old_end_col = 8;
///     e.new_end_row = 0;
///     e.new_end_col = 14;
/// });
/// assert_eq!(edit.start_byte, 8);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Edit {
    /// Byte offset where the change begins.
    pub start_byte: usize,
    /// Byte offset where the *replaced* range ended (in the old text).
    pub old_end_byte: usize,
    /// Byte offset where the *new* range ends (in the new text).
    pub new_end_byte: usize,

    /// Row of `start_byte` in the old text (0-based).
    pub start_row: usize,
    /// Column of `start_byte` in the old text (0-based byte column).
    pub start_col: usize,
    /// Row of `old_end_byte` in the old text.
    pub old_end_row: usize,
    /// Column of `old_end_byte` in the old text.
    pub old_end_col: usize,
    /// Row of `new_end_byte` in the new text.
    pub new_end_row: usize,
    /// Column of `new_end_byte` in the new text.
    pub new_end_col: usize,
}

impl Edit {
    /// Builder per Tier-0 convention.
    #[must_use]
    pub fn with(f: impl FnOnce(&mut Self)) -> Self {
        let mut e = Self::default();
        f(&mut e);
        e
    }

    /// Convenience: build an [`Edit`] from the old buffer and the byte
    /// offsets, computing the row/col fields automatically.
    ///
    /// `new_end_byte` must point into the *new* text but `old_end_byte`
    /// and `start_byte` must point into `old_source`; the caller knows
    /// both texts because the editor produces the edit at the moment
    /// the buffer transitions.
    ///
    /// `new_end_row` / `new_end_col` are computed by walking
    /// `start_byte..new_end_byte` worth of inserted characters from
    /// `new_source`; pass the same `new_source` that will be handed to
    /// [`crate::treesitter::Parser::reparse`].
    #[must_use]
    pub fn from_diff(
        old_source: &str,
        new_source: &str,
        start_byte: usize,
        old_end_byte: usize,
        new_end_byte: usize,
    ) -> Self {
        let (start_row, start_col) = byte_position(old_source, start_byte);
        let (old_end_row, old_end_col) = byte_position(old_source, old_end_byte);
        let (new_end_row, new_end_col) = byte_position(new_source, new_end_byte);
        Self {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_row,
            start_col,
            old_end_row,
            old_end_col,
            new_end_row,
            new_end_col,
        }
    }

    pub(crate) const fn into_input_edit(self) -> InputEdit {
        InputEdit {
            start_byte: self.start_byte,
            old_end_byte: self.old_end_byte,
            new_end_byte: self.new_end_byte,
            start_position: Point::new(self.start_row, self.start_col),
            old_end_position: Point::new(self.old_end_row, self.old_end_col),
            new_end_position: Point::new(self.new_end_row, self.new_end_col),
        }
    }
}

/// Return the `(row, col)` of `byte_offset` inside `src`, clamping at
/// the end of the buffer. Byte-column, not display-column — the
/// tree-sitter `Point` uses bytes.
fn byte_position(src: &str, byte_offset: usize) -> (usize, usize) {
    let limit = byte_offset.min(src.len());
    let mut row = 0usize;
    let mut last_newline = 0usize;
    for (i, &b) in src.as_bytes()[..limit].iter().enumerate() {
        if b == b'\n' {
            row += 1;
            last_newline = i + 1;
        }
    }
    (row, limit - last_newline)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_position_handles_multi_line() {
        let src = "abc\ndef\nghij";
        assert_eq!(byte_position(src, 0), (0, 0));
        assert_eq!(byte_position(src, 3), (0, 3));
        assert_eq!(byte_position(src, 4), (1, 0));
        assert_eq!(byte_position(src, 7), (1, 3));
        assert_eq!(byte_position(src, 8), (2, 0));
        assert_eq!(byte_position(src, 12), (2, 4));
        // Out-of-range clamps to end.
        assert_eq!(byte_position(src, 999), (2, 4));
    }

    #[test]
    fn from_diff_computes_positions() {
        let old = "SELECT a FROM t;";
        let new = "SELECT a, b FROM t;";
        // Inserted ", b" at byte 8.
        let e = Edit::from_diff(old, new, 8, 8, 11);
        assert_eq!(e.start_byte, 8);
        assert_eq!(e.old_end_byte, 8);
        assert_eq!(e.new_end_byte, 11);
        assert_eq!(e.start_row, 0);
        assert_eq!(e.start_col, 8);
        assert_eq!(e.new_end_row, 0);
        assert_eq!(e.new_end_col, 11);
    }
}
