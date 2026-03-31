//! Editor selection ranges.
//!
//! A [`Selection`] is an `anchor` + `head` pair plus a
//! [`SelectionKind`] that tells the renderer (and the cut/copy
//! helpers) how to interpret the range. The buffer exposes selection
//! state through [`super::EditorBuffer::selection`] /
//! [`super::EditorBuffer::set_selection`] /
//! [`super::EditorBuffer::extend_selection_to`] and convenience
//! helpers for selected-text extraction and bulk deletion. Vim's
//! visual mode, the basic-mode shift-arrow paths and the
//! mouse-drag handler all funnel through the same API so a single
//! cut/copy/paste pipeline can serve all three.

/// `(row, col)` byte offset inside an [`super::EditorBuffer`]. The
/// column is a **byte** offset to stay consistent with the rest of
/// the buffer API; renderers convert to display columns when they
/// project a row.
pub type Position = (usize, usize);

/// Granularity of a [`Selection`].
///
/// `Character` selects an inclusive byte range from `anchor` to
/// `head`. `Line` always covers full lines from the lower of
/// `anchor.row` / `head.row` to the upper, regardless of column.
/// `Block` reserves space for a future column-rectangle visual mode
/// (vim's `Ctrl-V`); it currently behaves like `Character`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum SelectionKind {
    #[default]
    Character,
    Line,
    Block,
}

/// One contiguous selection range.
///
/// `anchor` is the position where the selection began (Shift-arrow
/// start, mouse-down, vim `v` entry). `head` tracks the cursor as
/// it moves. Either side may be lower or upper in row/column order;
/// helpers expose a `normalised()` view when you need a
/// `start <= end` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Position,
    pub head: Position,
    pub kind: SelectionKind,
}

impl Selection {
    /// Build a single-position selection at `pos`. Useful as the
    /// initial value when the user just pressed Shift+arrow once
    /// or clicked without dragging yet.
    #[must_use]
    pub const fn at(pos: Position) -> Self {
        Self {
            anchor: pos,
            head: pos,
            kind: SelectionKind::Character,
        }
    }

    /// Build a character selection with an explicit anchor and head.
    #[must_use]
    pub const fn character(anchor: Position, head: Position) -> Self {
        Self {
            anchor,
            head,
            kind: SelectionKind::Character,
        }
    }

    /// Build a line-wise selection.
    #[must_use]
    pub const fn line(anchor: Position, head: Position) -> Self {
        Self {
            anchor,
            head,
            kind: SelectionKind::Line,
        }
    }

    /// `true` when anchor and head occupy the same position.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.anchor.0 == self.head.0 && self.anchor.1 == self.head.1
    }

    /// Returns `(start, end)` with `start <= end` lexicographically.
    /// For line-wise selections the columns are pinned to `0` / line
    /// length at the buffer-level extraction sites; this helper only
    /// orders the row/column pair.
    #[must_use]
    pub fn normalised(&self) -> (Position, Position) {
        if (self.anchor.0, self.anchor.1) <= (self.head.0, self.head.1) {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// Move `head` to `pos`, keeping `anchor` fixed.
    pub const fn extend_to(&mut self, pos: Position) {
        self.head = pos;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_is_empty() {
        assert!(Selection::at((3, 5)).is_empty());
    }

    #[test]
    fn normalised_orders_endpoints() {
        let s = Selection::character((4, 2), (1, 8));
        let (start, end) = s.normalised();
        assert_eq!(start, (1, 8));
        assert_eq!(end, (4, 2));
    }

    #[test]
    fn extend_to_moves_head_only() {
        let mut s = Selection::at((2, 0));
        s.extend_to((2, 5));
        assert_eq!(s.anchor, (2, 0));
        assert_eq!(s.head, (2, 5));
        assert!(!s.is_empty());
    }

    #[test]
    fn line_kind_preserved_by_extend() {
        let mut s = Selection::line((1, 0), (1, 0));
        s.extend_to((3, 4));
        assert_eq!(s.kind, SelectionKind::Line);
    }
}
