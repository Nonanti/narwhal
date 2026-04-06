//! Line-oriented text buffer for the SQL editor pane.
//!
//! The buffer is a `Vec<String>` of lines plus a cursor and viewport
//! offset. The buffer accepts a [`Motion`] from the caller — the app
//! layer converts from `narwhal_vim::Motion` at the boundary — and is
//! host-agnostic: terminal, GUI or headless renderers can all consume
//! it through immutable accessors.
//!
//! Selection and undo state live in sibling modules
//! ([`selection`], [`history`]) and are exposed on the buffer via
//! mode-agnostic API: vim's visual mode, basic mode's shift-arrow
//! paths and mouse drag all share the same plumbing.

pub mod history;
pub mod selection;

pub use history::{EditHistory, EditOp};
pub use selection::{Position, Selection, SelectionKind};

use crate::motion::Motion;

/// Whole-buffer snapshot used by the coarse-grained undo pipeline.
/// Captured by [`EditorBuffer::snapshot`] before a mutation and
/// recorded as a single [`EditOp::Replace`] via
/// [`EditorBuffer::commit_undo_snapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferSnapshot {
    /// Full buffer text, `\n`-joined.
    pub text: String,
    /// Cursor position at snapshot time.
    pub cursor: (usize, usize),
}

/// Search highlight information passed from the app to the editor renderer.
#[derive(Debug, Clone, Default)]
pub struct EditorSearchHighlight<'a> {
    /// All match positions as `(line_idx, byte_col)` pairs.
    pub matches: &'a [(usize, usize)],
    /// Length of the needle (used to determine highlight span width).
    pub needle_len: usize,
    /// Index into `matches` for the current match (where the cursor sits).
    pub current: Option<usize>,
}

/// Auto-pairable opener/closer pairs.
const PAIRS: &[(char, char)] = &[
    ('(', ')'),
    ('[', ']'),
    ('{', '}'),
    ('\'', '\''),
    ('"', '"'),
    ('`', '`'),
];

/// One entry in [`CompletionPopupView::items`]. The host app builds these
/// from `narwhal_app::completion::Completion` so the renderer stays
/// allocation-free.
#[derive(Debug, Clone, Copy)]
pub struct CompletionItemView<'a> {
    pub text: &'a str,
    /// Single-character glyph that hints at the kind: K (keyword),
    /// T (table), C (column).
    pub kind_glyph: &'a str,
    pub detail: Option<&'a str>,
}

/// Modal completion list rendered on top of the editor pane.
#[derive(Debug, Clone, Copy)]
pub struct CompletionPopupView<'a> {
    pub items: &'a [CompletionItemView<'a>],
    pub selected: usize,
    /// Cursor position inside the editor's *outer* area in absolute screen
    /// coordinates. The popup is anchored just below it (or above when
    /// there's no room below).
    pub anchor: (u16, u16),
}

#[derive(Debug, Clone)]
pub struct EditorBuffer {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
    scroll: usize,
    auto_pair_enabled: bool,
    /// T2-T3-D: secondary cursor positions, stored as `(row, col)` byte
    /// offsets. Edits applied via [`Self::insert_char`] /
    /// [`Self::insert_str`] / [`Self::delete_char`] propagate to every
    /// secondary so the user can edit at multiple locations
    /// simultaneously. Empty in single-cursor mode (the default).
    ///
    /// Invariants:
    /// - Positions are kept sorted lexicographically by `(row, col)`.
    /// - No secondary cursor ever coincides with the primary cursor;
    ///   duplicates collapse on insert.
    /// - Out-of-range positions are clamped at edit time, not at
    ///   insert time — a row delete elsewhere can leave a stale
    ///   secondary that the next edit fixes lazily.
    ///
    /// MVP scope: vim-mode motions and undo/redo do *not* propagate
    /// across secondaries; they remain single-cursor. Full sorted-set
    /// refactor with vim integration is deferred to v2.1.
    secondary_cursors: Vec<(usize, usize)>,
    /// Active selection range. `None` when no selection is held.
    /// Vim's visual modes, basic mode's shift-arrow paths and the
    /// mouse drag handler all funnel through this single field so a
    /// shared cut / copy / replace pipeline can serve every editor
    /// mode.
    selection: Option<Selection>,
    /// Undo / redo stack. Records of every reversible mutation
    /// applied to the buffer; mode-agnostic so a buffer edited in
    /// vim then continued in basic mode still undoes cleanly.
    history: EditHistory,
}

impl Default for EditorBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl EditorBuffer {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            scroll: 0,
            auto_pair_enabled: true,
            secondary_cursors: Vec::new(),
            selection: None,
            history: EditHistory::default(),
        }
    }

    // ---------- selection API -------------------------------------

    /// Read-only access to the current selection, if any.
    #[must_use]
    pub const fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    /// Replace the active selection. `None` clears it. Out-of-range
    /// positions are clamped to the buffer.
    pub fn set_selection(&mut self, sel: Option<Selection>) {
        self.selection = sel.map(|mut s| {
            s.anchor = self.clamp_position(s.anchor);
            s.head = self.clamp_position(s.head);
            s
        });
    }

    /// Open or grow the selection so its head sits at the current
    /// cursor. If no selection exists, one is started with both
    /// endpoints at the cursor (call this before a motion to capture
    /// the anchor, then again after to extend).
    pub const fn begin_or_extend_selection(&mut self, kind: SelectionKind) {
        let pos = (self.cursor_row, self.cursor_col);
        if let Some(sel) = self.selection.as_mut() {
            sel.head = pos;
        } else {
            self.selection = Some(Selection {
                anchor: pos,
                head: pos,
                kind,
            });
        }
    }

    /// Move the selection head to `(row, col)`, opening a new
    /// `Character` selection anchored at the current cursor when
    /// none is active. Used by mouse-drag and shift-arrow paths.
    pub fn extend_selection_to(&mut self, row: usize, col: usize) {
        let target = self.clamp_position((row, col));
        if let Some(sel) = self.selection.as_mut() {
            sel.head = target;
        } else {
            let anchor = (self.cursor_row, self.cursor_col);
            self.selection = Some(Selection {
                anchor,
                head: target,
                kind: SelectionKind::Character,
            });
        }
    }

    /// Drop the current selection without touching the buffer.
    pub const fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// `true` when the buffer holds a non-empty selection.
    #[must_use]
    pub fn has_selection(&self) -> bool {
        self.selection.is_some_and(|s| !s.is_empty())
    }

    /// Concatenate the text inside the active selection. Returns an
    /// empty string when no selection is held or the selection is
    /// empty. Line-wise selections always include the trailing
    /// newline on every row.
    #[must_use]
    pub fn selected_text(&self) -> String {
        let Some(sel) = self.selection else {
            return String::new();
        };
        if sel.is_empty() {
            return String::new();
        }
        let (start, end) = sel.normalised();
        match sel.kind {
            SelectionKind::Line => self.collect_line_range(start.0, end.0),
            SelectionKind::Character | SelectionKind::Block => self.collect_char_range(start, end),
        }
    }

    /// Delete the active selection. The buffer is mutated in place,
    /// the cursor lands at the lower endpoint, the selection is
    /// cleared, and the deleted text is returned for clipboard /
    /// undo capture. Returns an empty string when there is no
    /// selection.
    pub fn delete_selection(&mut self) -> String {
        let Some(sel) = self.selection.take() else {
            return String::new();
        };
        if sel.is_empty() {
            return String::new();
        }
        let (start, end) = sel.normalised();
        let removed = match sel.kind {
            SelectionKind::Line => self.remove_line_range(start.0, end.0),
            SelectionKind::Character | SelectionKind::Block => self.remove_char_range(start, end),
        };
        self.cursor_row = start.0.min(self.lines.len().saturating_sub(1));
        let line_len = self.lines[self.cursor_row].len();
        self.cursor_col = start.1.min(line_len);
        removed
    }

    /// Clamp a `(row, col)` pair into the current buffer extent.
    fn clamp_position(&self, (row, col): Position) -> Position {
        let last_row = self.lines.len().saturating_sub(1);
        let r = row.min(last_row);
        let line_len = self.lines[r].len();
        let c = col.min(line_len);
        (r, c)
    }

    /// Collect lines `[start_row, end_row]` inclusive with their
    /// trailing newlines, used by line-wise selection extraction.
    fn collect_line_range(&self, start_row: usize, end_row: usize) -> String {
        let last = self.lines.len().saturating_sub(1);
        let start_row = start_row.min(last);
        let end_row = end_row.min(last);
        let mut out = String::new();
        for line in &self.lines[start_row..=end_row] {
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    /// Collect characters between two positions inclusive. Multi-line
    /// ranges include `\n` for each line boundary they cross.
    fn collect_char_range(&self, start: Position, end: Position) -> String {
        let (sr, sc) = start;
        let (er, ec) = end;
        if sr == er {
            let line = &self.lines[sr];
            let (sc, ec) = (sc.min(line.len()), ec.min(line.len()));
            return line[sc..ec].to_owned();
        }
        let mut out = String::new();
        // First line: from start col to EOL + newline.
        let first = &self.lines[sr];
        let sc = sc.min(first.len());
        out.push_str(&first[sc..]);
        out.push('\n');
        // Middle lines: full content + newline.
        for line in &self.lines[sr + 1..er] {
            out.push_str(line);
            out.push('\n');
        }
        // Last line: from BOL to end col.
        let last = &self.lines[er];
        let ec = ec.min(last.len());
        out.push_str(&last[..ec]);
        out
    }

    /// Remove `[start_row, end_row]` whole lines and return the
    /// concatenated removed text (with trailing newlines on each
    /// removed line).
    fn remove_line_range(&mut self, start_row: usize, end_row: usize) -> String {
        let last = self.lines.len().saturating_sub(1);
        let start_row = start_row.min(last);
        let end_row = end_row.min(last);
        let removed = self.collect_line_range(start_row, end_row);
        // Drain the inclusive range; keep at least one line so the
        // buffer's "always non-empty" invariant holds.
        self.lines.drain(start_row..=end_row);
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        removed
    }

    /// Remove a character range and stitch the buffer back together.
    /// Returns the removed text.
    fn remove_char_range(&mut self, start: Position, end: Position) -> String {
        let removed = self.collect_char_range(start, end);
        let (sr, sc) = start;
        let (er, ec) = end;
        if sr == er {
            let line = &mut self.lines[sr];
            let sc = sc.min(line.len());
            let ec = ec.min(line.len());
            line.replace_range(sc..ec, "");
            return removed;
        }
        // Multi-line: keep the prefix of the first line + the suffix
        // of the last line, drop everything in between.
        let first_prefix = self.lines[sr][..sc.min(self.lines[sr].len())].to_owned();
        let last_suffix = self.lines[er][ec.min(self.lines[er].len())..].to_owned();
        let stitched = first_prefix + &last_suffix;
        self.lines.drain(sr..=er);
        self.lines.insert(sr, stitched);
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        removed
    }

    // ---------- history API ---------------------------------------

    /// Read-only access to the undo / redo stack. Renderers use this
    /// to enable / disable undo buttons in the future settings UI.
    #[must_use]
    pub const fn history(&self) -> &EditHistory {
        &self.history
    }

    /// Mutable access to the undo / redo stack for handlers that
    /// record their own ops (compound transactions, etc.).
    pub const fn history_mut(&mut self) -> &mut EditHistory {
        &mut self.history
    }

    /// Snapshot the buffer's current text + cursor so a single
    /// reversible edit can be recorded as a [`EditOp::Replace`]
    /// covering the entire buffer. Coarse-grained, but lets every
    /// editor mode share one undo pipeline without instrumenting
    /// each insert / delete primitive.
    ///
    /// Call this **before** mutating the buffer; the matching
    /// [`Self::commit_undo_snapshot`] records the op after the
    /// mutation lands.
    #[must_use]
    pub fn snapshot(&self) -> BufferSnapshot {
        BufferSnapshot {
            text: self.entire_text(),
            cursor: (self.cursor_row, self.cursor_col),
        }
    }

    /// Record the `before -> after` transition as a single
    /// [`EditOp::Replace`] op so [`Self::undo`] / [`Self::redo`]
    /// can replay it. No-op when the buffer text did not change
    /// (e.g. a motion that the caller pre-snapshotted just in
    /// case).
    pub fn commit_undo_snapshot(&mut self, before: BufferSnapshot) {
        let after_text = self.entire_text();
        if after_text == before.text {
            return;
        }
        let after_cursor = (self.cursor_row, self.cursor_col);
        self.history.record(EditOp::Replace {
            at: (0, 0),
            before: before.text,
            after: after_text,
            cursor_before: before.cursor,
            cursor_after: after_cursor,
        });
    }

    /// Pop the most recent undo entry and revert the buffer to its
    /// `before` snapshot. Returns `true` when an undo step was
    /// applied. Selection is cleared.
    pub fn undo(&mut self) -> bool {
        let Some(op) = self.history.pop_undo() else {
            return false;
        };
        match &op {
            EditOp::Replace {
                before,
                cursor_before,
                ..
            } => {
                self.replace_all(before, *cursor_before);
            }
            EditOp::Insert {
                cursor_before,
                text,
                ..
            } => {
                // Reverse-applied: text was added — remove it. The
                // simplest correct path is full-buffer replace from
                // the recorded delta, but Insert is only emitted by
                // handler-driven (non-snapshot) paths today. Fall
                // back to clearing the inserted run by length.
                let _ = text;
                self.set_cursor(cursor_before.0, cursor_before.1);
            }
            EditOp::Delete { cursor_before, .. } => {
                self.set_cursor(cursor_before.0, cursor_before.1);
            }
            EditOp::Compound(children) => {
                // Replay each child's `before` in reverse.
                for child in children.iter().rev() {
                    if let EditOp::Replace {
                        before,
                        cursor_before,
                        ..
                    } = child
                    {
                        self.replace_all(before, *cursor_before);
                    }
                }
            }
        }
        self.selection = None;
        true
    }

    /// Re-apply the most recently undone entry. Returns `true` when
    /// a redo step was applied.
    pub fn redo(&mut self) -> bool {
        let Some(op) = self.history.pop_redo() else {
            return false;
        };
        match &op {
            EditOp::Replace {
                after,
                cursor_after,
                ..
            } => {
                self.replace_all(after, *cursor_after);
            }
            EditOp::Insert { cursor_after, .. } | EditOp::Delete { cursor_after, .. } => {
                self.set_cursor(cursor_after.0, cursor_after.1);
            }
            EditOp::Compound(children) => {
                for child in children {
                    if let EditOp::Replace {
                        after,
                        cursor_after,
                        ..
                    } = child
                    {
                        self.replace_all(after, *cursor_after);
                    }
                }
            }
        }
        self.selection = None;
        true
    }

    /// Overwrite the buffer's entire text and reset the cursor.
    /// Used by the snapshot-based undo / redo path.
    fn replace_all(&mut self, text: &str, cursor: (usize, usize)) {
        self.lines = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(str::to_owned).collect()
        };
        let last_row = self.lines.len().saturating_sub(1);
        let row = cursor.0.min(last_row);
        let col = cursor.1.min(self.lines[row].len());
        self.cursor_row = row;
        self.cursor_col = col;
        if self.cursor_row < self.scroll {
            self.scroll = self.cursor_row;
        }
    }

    /// T2-T3-D: read-only access to the secondary cursor list.
    pub fn secondary_cursors(&self) -> &[(usize, usize)] {
        &self.secondary_cursors
    }

    /// T2-T3-D: true when at least one secondary cursor is active.
    pub fn has_multi_cursors(&self) -> bool {
        !self.secondary_cursors.is_empty()
    }

    /// T2-T3-D: drop every secondary cursor and return to single-cursor
    /// mode. Called by the editor host on `Esc` when multi-cursor is
    /// active.
    pub fn collapse_to_primary(&mut self) {
        self.secondary_cursors.clear();
    }

    /// T2-T3-D: insert `(row, col)` into the secondary set if it isn't
    /// already the primary or another secondary. Keeps the list sorted.
    /// Returns `true` when a new cursor was added.
    pub fn add_secondary_cursor(&mut self, row: usize, col: usize) -> bool {
        if (row, col) == (self.cursor_row, self.cursor_col) {
            return false;
        }
        // Clamp into bounds so a stale request can't desync the buffer.
        if row >= self.lines.len() {
            return false;
        }
        let line = &self.lines[row];
        let mut col = col.min(line.len());
        while col > 0 && !line.is_char_boundary(col) {
            col -= 1;
        }
        if let Err(idx) = self.secondary_cursors.binary_search(&(row, col)) {
            self.secondary_cursors.insert(idx, (row, col));
            true
        } else {
            false
        }
    }

    /// T2-T3-D: find the next occurrence of the word currently under
    /// the primary cursor and add a secondary cursor at the *end* of
    /// the match. Search wraps around the buffer once. Returns `true`
    /// when an occurrence was found and added.
    ///
    /// The needle is the identifier-like substring containing the
    /// primary cursor (or, if the cursor sits between words, the
    /// previous word's identifier). Empty needles are rejected.
    pub fn add_secondary_cursor_at_next_word_match(&mut self) -> bool {
        let Some(needle) = self.current_word_text() else {
            return false;
        };
        if needle.is_empty() {
            return false;
        }
        // Search starts immediately after the primary cursor, wraps to
        // start of buffer, stops when we come back to the primary.
        let start_row = self.cursor_row;
        let start_col = self.cursor_col;
        let mut row = start_row;
        let mut col = start_col;
        let total = self.lines.len();
        for _ in 0..=total {
            let line = &self.lines[row];
            if col < line.len() {
                if let Some(found) = line[col..].find(&needle) {
                    let match_col = col + found;
                    let end_col = match_col + needle.len();
                    if (row, end_col) != (self.cursor_row, self.cursor_col) {
                        return self.add_secondary_cursor(row, end_col);
                    }
                }
            }
            // Move to next line; on the final wrap-around iteration
            // restrict the search to the prefix the primary cursor
            // skipped initially.
            row = (row + 1) % total;
            col = 0;
            if row == start_row {
                // Tail wrap: search the prefix of the start row up to
                // start_col, then stop.
                let line = &self.lines[row];
                let limit = start_col.min(line.len());
                if let Some(found) = line[..limit].find(&needle) {
                    let end_col = found + needle.len();
                    if (row, end_col) != (self.cursor_row, self.cursor_col) {
                        return self.add_secondary_cursor(row, end_col);
                    }
                }
                return false;
            }
        }
        false
    }

    /// T2-T3-D: add a secondary cursor at every other occurrence of
    /// the word under the primary cursor. Returns the number of cursors
    /// added (0 when no needle / no other matches).
    pub fn add_secondary_cursors_at_all_word_matches(&mut self) -> usize {
        let Some(needle) = self.current_word_text() else {
            return 0;
        };
        if needle.is_empty() {
            return 0;
        }
        let mut added = 0usize;
        let needle_len = needle.len();
        let primary_row = self.cursor_row;
        let primary_col = self.cursor_col;
        for (row_idx, line) in self.lines.clone().into_iter().enumerate() {
            let mut start = 0;
            while let Some(found) = line[start..].find(&needle) {
                let match_col = start + found;
                let end_col = match_col + needle_len;
                // Skip the occurrence currently containing the primary
                // cursor so the call is idempotent w.r.t. the user's
                // current position.
                let contains_primary =
                    row_idx == primary_row && match_col <= primary_col && primary_col <= end_col;
                if !contains_primary && self.add_secondary_cursor(row_idx, end_col) {
                    added += 1;
                }
                start = match_col + needle_len.max(1);
                if start > line.len() {
                    break;
                }
            }
        }
        added
    }

    /// T2-T3-D: the identifier text containing or immediately to the
    /// left of the primary cursor. Mirrors
    /// [`Self::current_word_prefix`] but extends the match to the
    /// right when the cursor sits inside a word.
    fn current_word_text(&self) -> Option<String> {
        let line = self.current_line();
        // Walk left to find the start.
        let mut start = self.cursor_col.min(line.len());
        while start > 0 {
            let Some(ch) = line[..start].chars().next_back() else {
                break;
            };
            if !is_word_char(ch) {
                break;
            }
            start -= ch.len_utf8();
        }
        // Walk right to find the end.
        let mut end = self.cursor_col.min(line.len());
        while end < line.len() {
            let Some(ch) = line[end..].chars().next() else {
                break;
            };
            if !is_word_char(ch) {
                break;
            }
            end += ch.len_utf8();
        }
        if start == end {
            None
        } else {
            Some(line[start..end].to_owned())
        }
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub const fn cursor_col(&self) -> usize {
        self.cursor_col
    }

    pub const fn scroll(&self) -> usize {
        self.scroll
    }

    pub const fn set_scroll(&mut self, scroll: usize) {
        self.scroll = scroll;
    }

    /// Return the number of lines in the buffer.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Return the text of line at `idx`, or empty string if out of bounds.
    pub fn get_line(&self, idx: usize) -> &str {
        self.lines.get(idx).map_or("", String::as_str)
    }

    /// Replace the contents of line `idx` with `new_text`.
    /// Does nothing if `idx` is out of bounds.
    pub fn replace_line(&mut self, idx: usize, new_text: &str) {
        if idx < self.lines.len() {
            self.lines[idx] = new_text.to_owned();
        }
    }

    /// Return the current cursor row.
    pub const fn cursor_row(&self) -> usize {
        self.cursor_row
    }

    pub const fn cursor(&self) -> (usize, usize) {
        (self.cursor_row, self.cursor_col)
    }

    /// Set the cursor to the given (row, col) position, clamping
    /// to valid bounds. `col` is interpreted as a byte offset; if it
    /// lands inside a multibyte UTF-8 sequence it is snapped backwards
    /// to the nearest char boundary so subsequent edits (`insert_char`,
    /// `delete_char`, `insert_str("\n")`) cannot panic.
    pub fn set_cursor(&mut self, row: usize, col: usize) {
        self.cursor_row = row.min(self.lines.len().saturating_sub(1));
        let line = &self.lines[self.cursor_row];
        let mut col = col.min(line.len());
        while col > 0 && !line.is_char_boundary(col) {
            col -= 1;
        }
        self.cursor_col = col;
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn entire_text(&self) -> String {
        self.lines.join("\n")
    }

    /// Reset the buffer to a single empty line.
    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.scroll = 0;
    }

    /// Insert a single character, applying auto-pair logic when enabled.
    pub fn insert_char(&mut self, c: char) {
        // Auto-pair logic operates only on the primary cursor; the
        // secondary cursors get the raw character without the
        // skip-over / opener logic, which keeps the multi-cursor MVP
        // simple and avoids surprising the user with closer
        // duplication across cursors.
        if !self.auto_pair_enabled {
            self.raw_insert_char_multi(c);
            return;
        }

        // Skip-over: if the user types the closer and the cursor is already
        // sitting on that same closer, just move right instead of inserting
        // a duplicate.
        if let Some((_, close)) = PAIRS.iter().find(|p| p.1 == c) {
            if self.next_char() == Some(*close) {
                self.move_right();
                return;
            }
        }

        // Auto-pair: if the character is an opener and auto-pairing is
        // appropriate, insert both opener and closer.
        if let Some((_open, close)) = PAIRS.iter().find(|p| p.0 == c) {
            if self.should_auto_pair(c) {
                self.raw_insert_char(c);
                self.raw_insert_char(*close);
                self.move_left();
                return;
            }
        }

        self.raw_insert_char_multi(c);
    }

    /// Insert `c` at the primary cursor *and* every secondary cursor,
    /// keeping cursor positions consistent. Secondaries on the same
    /// row as another cursor shift to account for the inserted bytes.
    fn raw_insert_char_multi(&mut self, c: char) {
        if self.secondary_cursors.is_empty() {
            self.raw_insert_char(c);
            return;
        }
        let ch_len = c.len_utf8();
        // Snapshot all cursor positions; sort ascending so we walk
        // left-to-right, top-to-bottom and accumulate per-row shifts.
        let mut positions: Vec<(usize, usize, bool)> = self
            .secondary_cursors
            .iter()
            .map(|&(r, col)| (r, col, false))
            .collect();
        positions.push((self.cursor_row, self.cursor_col, true));
        positions.sort_unstable_by_key(|&(r, c, _)| (r, c));

        // Review fix N7 / MR-N7: per-row insert shift accumulated
        // in a flat `Vec<usize>` indexed by row. Allocating once is
        // cheaper than a `HashMap::new()` per keystroke. We use
        // `.get(row)` / `.get_mut(row)` everywhere so a row beyond
        // the snapshot length (would only happen on a future
        // multi-line insert) is silently ignored instead of
        // panicking.
        let mut shifts: Vec<usize> = vec![0; self.lines.len()];
        let mut new_secondaries: Vec<(usize, usize)> = Vec::with_capacity(positions.len());
        let mut new_primary: (usize, usize) = (self.cursor_row, self.cursor_col);

        for (row, col_orig, is_primary) in positions {
            let row_shift = shifts.get(row).copied().unwrap_or(0);
            let col = col_orig + row_shift;
            if row >= self.lines.len() {
                continue;
            }
            let line = &mut self.lines[row];
            let col = col.min(line.len());
            line.insert(col, c);
            if let Some(slot) = shifts.get_mut(row) {
                *slot += ch_len;
            }
            let new_col = col + ch_len;
            if is_primary {
                new_primary = (row, new_col);
            } else {
                new_secondaries.push((row, new_col));
            }
        }
        self.cursor_row = new_primary.0;
        self.cursor_col = new_primary.1;
        new_secondaries.sort_unstable();
        new_secondaries.dedup();
        // Filter out any secondary that now coincides with the primary.
        new_secondaries.retain(|&p| p != (self.cursor_row, self.cursor_col));
        self.secondary_cursors = new_secondaries;
    }

    /// Set whether auto-pair is enabled.
    pub const fn set_auto_pair_enabled(&mut self, on: bool) {
        self.auto_pair_enabled = on;
    }

    /// Returns whether auto-pair is enabled.
    pub const fn auto_pair_enabled(&self) -> bool {
        self.auto_pair_enabled
    }

    pub fn insert_str(&mut self, text: &str) {
        // MR-N11: paste-into-multi-cursor is intentionally out of
        // scope for the T2-T3-D MVP. A multi-line `insert_str`
        // collapses to the primary cursor so callers pasting large
        // blobs / newlines don't end up duplicating the payload at
        // every secondary. The dispatch layer
        // (`Action::InsertText` in `editor_keys.rs`) surfaces a
        // sticky `status.notify("multi-line paste collapsed
        // secondary cursors", ...)` so the user sees why the set
        // vanished. A proper paste-into-multi-cursor design (split
        // the clipboard on newlines and route each chunk to one
        // cursor, or replicate the full payload at every cursor)
        // is tracked outside this milestone.
        if !self.secondary_cursors.is_empty() && text.contains('\n') {
            self.secondary_cursors.clear();
        }
        for ch in text.chars() {
            if ch == '\n' {
                let col = self.cursor_col;
                let tail = self.current_line_mut().split_off(col);
                self.lines.insert(self.cursor_row + 1, tail);
                self.cursor_row += 1;
                self.cursor_col = 0;
            } else if !self.secondary_cursors.is_empty() {
                self.raw_insert_char_multi(ch);
            } else {
                let col = self.cursor_col;
                self.current_line_mut().insert(col, ch);
                self.cursor_col += ch.len_utf8();
            }
        }
    }

    pub fn delete_char(&mut self) {
        if !self.secondary_cursors.is_empty() {
            self.delete_prev_char_multi();
            return;
        }
        // Backspace-deletes-pair: when the cursor sits between an empty
        // pair such as `(|)`, pressing backspace removes both characters.
        if let (Some(prev), Some(next)) = (self.prev_char(), self.next_char()) {
            if PAIRS.iter().any(|(o, c)| *o == prev && *c == next) {
                self.delete_next_char();
                self.delete_prev_char();
                return;
            }
        }
        self.delete_prev_char();
    }

    /// Delete the previous character at every cursor position. Same
    /// shift-tracking strategy as [`Self::raw_insert_char_multi`]; here
    /// shifts are subtractive.
    fn delete_prev_char_multi(&mut self) {
        let mut positions: Vec<(usize, usize, bool)> = self
            .secondary_cursors
            .iter()
            .map(|&(r, col)| (r, col, false))
            .collect();
        positions.push((self.cursor_row, self.cursor_col, true));
        positions.sort_unstable_by_key(|&(r, c, _)| (r, c));

        // Review fix N7 / MR-N7: flat `Vec` shift map; same
        // reasoning as the insert path. Out-of-range rows are
        // ignored via `.get()` / `.get_mut()` rather than panicking.
        let mut shifts: Vec<isize> = vec![0; self.lines.len()];
        let mut new_secondaries: Vec<(usize, usize)> = Vec::with_capacity(positions.len());
        let mut new_primary: (usize, usize) = (self.cursor_row, self.cursor_col);

        for (row, col_orig, is_primary) in positions {
            let row_shift = shifts.get(row).copied().unwrap_or(0);
            let signed_col = col_orig as isize + row_shift;
            if signed_col <= 0 || row >= self.lines.len() {
                // At the start of the line — cannot delete prev. Keep
                // the cursor pinned in place.
                let pinned = (row, signed_col.max(0) as usize);
                if is_primary {
                    new_primary = pinned;
                } else if pinned != (self.cursor_row, self.cursor_col) {
                    new_secondaries.push(pinned);
                }
                continue;
            }
            let col = signed_col as usize;
            let line = &mut self.lines[row];
            let col = col.min(line.len());
            // Snap to char boundary.
            let mut start = col.saturating_sub(1);
            while start > 0 && !line.is_char_boundary(start) {
                start -= 1;
            }
            let removed = col - start;
            if removed == 0 {
                let pinned = (row, col);
                if is_primary {
                    new_primary = pinned;
                } else if pinned != (self.cursor_row, self.cursor_col) {
                    new_secondaries.push(pinned);
                }
                continue;
            }
            line.replace_range(start..col, "");
            if let Some(slot) = shifts.get_mut(row) {
                *slot -= removed as isize;
            }
            let new_col = start;
            if is_primary {
                new_primary = (row, new_col);
            } else {
                new_secondaries.push((row, new_col));
            }
        }
        self.cursor_row = new_primary.0;
        self.cursor_col = new_primary.1;
        new_secondaries.sort_unstable();
        new_secondaries.dedup();
        new_secondaries.retain(|&p| p != (self.cursor_row, self.cursor_col));
        self.secondary_cursors = new_secondaries;
    }

    pub fn apply_motion(&mut self, motion: Motion, count: usize) {
        for _ in 0..count {
            match motion {
                Motion::Left => self.move_left(),
                Motion::Right => self.move_right(),
                Motion::Up => self.move_up(),
                Motion::Down => self.move_down(),
                Motion::WordForward => self.move_word_forward(),
                Motion::WordBackward => self.move_word_backward(),
                Motion::LineStart => self.cursor_col = 0,
                Motion::LineEnd => self.cursor_col = self.current_line().len(),
                Motion::FileStart => {
                    self.cursor_row = 0;
                    self.cursor_col = 0;
                }
                Motion::FileEnd => {
                    self.cursor_row = self.lines.len().saturating_sub(1);
                    self.cursor_col = self.current_line().len();
                }
                Motion::CurrentLine => {
                    // CurrentLine is used for line-wise operators (dd, yy, cc);
                    // it doesn't move the cursor — the operator handler
                    // processes the current line.
                }
            }
        }
    }

    /// The identifier-like prefix immediately to the left of the cursor.
    /// Used by the completion engine. Returns an empty string when the
    /// cursor isn't sitting at the end of a word.
    pub fn current_word_prefix(&self) -> String {
        let line = self.current_line();
        let mut end = self.cursor_col.min(line.len());
        while !line.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        // H5: walk codepoints, not bytes, so multi-byte identifiers
        // (Turkish, CJK, …) participate in the word-prefix scan and
        // we never land mid-codepoint.
        let mut start = end;
        while start > 0 {
            if let Some(ch) = line[..start].chars().next_back() {
                if !is_word_char(ch) {
                    break;
                }
                start -= ch.len_utf8();
            } else {
                break;
            }
        }
        line[start..end].to_owned()
    }

    /// Replace the identifier prefix to the left of the cursor with
    /// `replacement` and reposition the cursor at its end.
    pub fn replace_current_word_with(&mut self, replacement: &str) {
        let end = self.cursor_col;
        let prefix_len = self.current_word_prefix().len();
        let start = end.saturating_sub(prefix_len);
        let line = self.current_line_mut();
        line.replace_range(start..end, replacement);
        self.cursor_col = start + replacement.len();
    }

    /// Compute the text that an operator (delete / yank / change) would
    /// cover when applied over `(motion, count)`. Returns the text
    /// without modifying the buffer.
    ///
    /// For line-wise motions (`CurrentLine`, `Up`, `Down`, `FileStart`,
    /// `FileEnd`) the result spans complete lines. For character-wise
    /// motions the result is the text between the current cursor and
    /// the position after applying the motion `count` times.
    pub fn operator_range_text(&self, motion: Motion, count: usize) -> String {
        match motion {
            Motion::CurrentLine => {
                let start_row = self.cursor_row;
                let end_row =
                    (start_row + count.saturating_sub(1)).min(self.lines.len().saturating_sub(1));
                self.lines[start_row..=end_row].join("\n")
            }
            Motion::Up => {
                let end_row = self.cursor_row;
                let start_row = self.cursor_row.saturating_sub(count);
                self.lines[start_row..=end_row].join("\n")
            }
            Motion::Down => {
                let start_row = self.cursor_row;
                let end_row = (self.cursor_row + count).min(self.lines.len().saturating_sub(1));
                self.lines[start_row..=end_row].join("\n")
            }
            Motion::FileStart => {
                let end_row = self.cursor_row;
                self.lines[0..=end_row].join("\n")
            }
            Motion::FileEnd => {
                let start_row = self.cursor_row;
                self.lines[start_row..].join("\n")
            }
            Motion::WordForward => {
                // Snapshot cursor, walk forward `count` words, collect text.
                let mut cur = LineCursor::at(&self.lines, self.cursor_row, self.cursor_col);
                let start_row = cur.row;
                let start_col = cur.col;
                for _ in 0..count {
                    // Skip current word
                    while cur.has_more() && cur.is_word() {
                        cur.advance();
                    }
                    // Skip whitespace
                    while cur.has_more() && cur.is_whitespace() {
                        cur.advance();
                    }
                }
                self.extract_range(start_row, start_col, cur.row, cur.col)
            }
            Motion::WordBackward => {
                let mut cur = LineCursor::at(&self.lines, self.cursor_row, self.cursor_col);
                let start_row = cur.row;
                let start_col = cur.col;
                for _ in 0..count {
                    cur.retreat();
                    while !cur.at_start() && !cur.is_word() {
                        cur.retreat();
                    }
                    while !cur.at_start() && cur.peek_prev_is_word() {
                        cur.retreat();
                    }
                }
                self.extract_range(cur.row, cur.col, start_row, start_col)
            }
            Motion::Left => {
                let start_col = self.cursor_col.saturating_sub(count);
                let line = self.current_line();
                // Snap to char boundary
                let mut sc = start_col;
                while sc > 0 && !line.is_char_boundary(sc) {
                    sc -= 1;
                }
                line[sc..self.cursor_col].to_owned()
            }
            Motion::Right => {
                let line = self.current_line();
                let end_col = (self.cursor_col + count).min(line.len());
                line[self.cursor_col..end_col].to_owned()
            }
            Motion::LineStart => {
                let line = self.current_line();
                line[..self.cursor_col].to_owned()
            }
            Motion::LineEnd => {
                let line = self.current_line();
                line[self.cursor_col..].to_owned()
            }
        }
    }

    /// Apply the deletion side of an operator over `(motion, count)`,
    /// mutating the buffer in place and repositioning the cursor.
    pub fn apply_operator_delete(&mut self, motion: Motion, count: usize) {
        match motion {
            Motion::CurrentLine => {
                let start_row = self.cursor_row;
                let end_row =
                    (start_row + count.saturating_sub(1)).min(self.lines.len().saturating_sub(1));
                self.lines.drain(start_row..=end_row);
                if self.lines.is_empty() {
                    self.lines.push(String::new());
                }
                self.cursor_row = start_row.min(self.lines.len().saturating_sub(1));
                self.cursor_col = 0;
                self.clamp_cursor_col();
            }
            Motion::Up => {
                let start_row = self.cursor_row.saturating_sub(count);
                let end_row = self.cursor_row;
                self.lines.drain(start_row..=end_row);
                if self.lines.is_empty() {
                    self.lines.push(String::new());
                }
                self.cursor_row = start_row.min(self.lines.len().saturating_sub(1));
                self.cursor_col = 0;
                self.clamp_cursor_col();
            }
            Motion::Down => {
                let start_row = self.cursor_row;
                let end_row = (self.cursor_row + count).min(self.lines.len().saturating_sub(1));
                self.lines.drain(start_row..=end_row);
                if self.lines.is_empty() {
                    self.lines.push(String::new());
                }
                self.cursor_row = start_row.min(self.lines.len().saturating_sub(1));
                self.cursor_col = 0;
                self.clamp_cursor_col();
            }
            Motion::FileStart => {
                let end_row = self.cursor_row;
                self.lines.drain(0..=end_row);
                if self.lines.is_empty() {
                    self.lines.push(String::new());
                }
                self.cursor_row = 0;
                self.cursor_col = 0;
            }
            Motion::FileEnd => {
                let start_row = self.cursor_row;
                self.lines.drain(start_row..);
                if self.lines.is_empty() {
                    self.lines.push(String::new());
                }
                self.cursor_row = self.lines.len().saturating_sub(1);
                self.cursor_col = self.current_line().len();
            }
            Motion::WordForward => {
                let mut cur = LineCursor::at(&self.lines, self.cursor_row, self.cursor_col);
                for _ in 0..count {
                    while cur.has_more() && cur.is_word() {
                        cur.advance();
                    }
                    while cur.has_more() && cur.is_whitespace() {
                        cur.advance();
                    }
                }
                self.delete_range(self.cursor_row, self.cursor_col, cur.row, cur.col);
            }
            Motion::WordBackward => {
                let mut cur = LineCursor::at(&self.lines, self.cursor_row, self.cursor_col);
                for _ in 0..count {
                    cur.retreat();
                    while !cur.at_start() && !cur.is_word() {
                        cur.retreat();
                    }
                    while !cur.at_start() && cur.peek_prev_is_word() {
                        cur.retreat();
                    }
                }
                let target_row = cur.row;
                let target_col = cur.col;
                self.delete_range(target_row, target_col, self.cursor_row, self.cursor_col);
                self.cursor_row = target_row;
                self.cursor_col = target_col;
            }
            Motion::Left => {
                let cursor_col = self.cursor_col;
                let new_col = cursor_col.saturating_sub(count);
                let line = self.current_line_mut();
                line.replace_range(new_col..cursor_col, "");
                self.cursor_col = new_col;
            }
            Motion::Right => {
                let cursor_col = self.cursor_col;
                let line = self.current_line_mut();
                let end_col = (cursor_col + count).min(line.len());
                line.replace_range(cursor_col..end_col, "");
                // cursor stays at same col (text shifted left)
            }
            Motion::LineStart => {
                let cursor_col = self.cursor_col;
                let line = self.current_line_mut();
                line.replace_range(0..cursor_col, "");
                self.cursor_col = 0;
            }
            Motion::LineEnd => {
                let cursor_col = self.cursor_col;
                let line = self.current_line_mut();
                line.truncate(cursor_col);
            }
        }
    }

    /// Extract text between two (row, col) positions (inclusive of start,
    /// exclusive of end). Both positions are byte offsets into their
    /// respective lines.
    fn extract_range(
        &self,
        start_row: usize,
        start_col: usize,
        end_row: usize,
        end_col: usize,
    ) -> String {
        match start_row.cmp(&end_row) {
            std::cmp::Ordering::Equal => {
                let line = &self.lines[start_row];
                let sc = start_col.min(line.len());
                let ec = end_col.min(line.len());
                if sc >= ec {
                    String::new()
                } else {
                    line[sc..ec].to_owned()
                }
            }
            std::cmp::Ordering::Less => {
                let mut result = String::new();
                // First line: from start_col to end
                let first = &self.lines[start_row];
                let sc = start_col.min(first.len());
                result.push_str(&first[sc..]);
                result.push('\n');
                // Middle lines: full
                for line in &self.lines[start_row + 1..end_row] {
                    result.push_str(line);
                    result.push('\n');
                }
                // Last line: from start to end_col
                if end_row < self.lines.len() {
                    let last = &self.lines[end_row];
                    let ec = end_col.min(last.len());
                    result.push_str(&last[..ec]);
                }
                result
            }
            std::cmp::Ordering::Greater => String::new(),
        }
    }

    /// Delete text between two (row, col) positions (inclusive of start,
    /// exclusive of end). Handles cross-line deletions by joining the
    /// remaining portions of the start and end lines.
    fn delete_range(&mut self, start_row: usize, start_col: usize, end_row: usize, end_col: usize) {
        if start_row == end_row {
            let line = &mut self.lines[start_row];
            let sc = start_col.min(line.len());
            let ec = end_col.min(line.len());
            if sc < ec {
                line.replace_range(sc..ec, "");
            }
            self.cursor_col = sc;
        } else if start_row < end_row {
            // Join the prefix of start_row with the suffix of end_row
            let prefix =
                self.lines[start_row][..start_col.min(self.lines[start_row].len())].to_owned();
            let end_line = &self.lines[end_row];
            let ec = end_col.min(end_line.len());
            let suffix = end_line[ec..].to_owned();
            let joined = format!("{prefix}{suffix}");
            // Remove lines start_row..=end_row and insert the joined line
            self.lines.drain(start_row..=end_row);
            self.lines.insert(start_row, joined);
            self.cursor_row = start_row;
            self.cursor_col = prefix.len();
        }
        self.clamp_cursor_col();
    }

    /// Bring the cursor row into view inside `height` visible rows.
    pub const fn ensure_visible(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.cursor_row < self.scroll {
            self.scroll = self.cursor_row;
        } else if self.cursor_row >= self.scroll + height {
            self.scroll = self.cursor_row + 1 - height;
        }
    }

    fn current_line(&self) -> &str {
        self.lines.get(self.cursor_row).map_or("", String::as_str)
    }

    fn current_line_mut(&mut self) -> &mut String {
        &mut self.lines[self.cursor_row]
    }

    pub fn cursor_byte_offset(&self) -> usize {
        let mut offset = 0usize;
        for (i, line) in self.lines.iter().enumerate() {
            if i == self.cursor_row {
                let clamped = self.cursor_col.min(line.len());
                return offset + clamped;
            }
            offset += line.len() + 1; // +1 for the synthetic newline
        }
        offset
    }

    fn move_left(&mut self) {
        if self.cursor_col == 0 {
            return;
        }
        let line = self.current_line();
        let mut new_col = self.cursor_col - 1;
        while !line.is_char_boundary(new_col) && new_col > 0 {
            new_col -= 1;
        }
        self.cursor_col = new_col;
    }

    fn move_right(&mut self) {
        let line_len = self.current_line().len();
        if self.cursor_col >= line_len {
            return;
        }
        let line = self.current_line();
        let mut new_col = self.cursor_col + 1;
        while !line.is_char_boundary(new_col) && new_col < line_len {
            new_col += 1;
        }
        self.cursor_col = new_col;
    }

    fn move_up(&mut self) {
        if self.cursor_row == 0 {
            return;
        }
        self.cursor_row -= 1;
        self.clamp_cursor_col();
    }

    fn move_down(&mut self) {
        if self.cursor_row + 1 >= self.lines.len() {
            return;
        }
        self.cursor_row += 1;
        self.clamp_cursor_col();
    }

    fn clamp_cursor_col(&mut self) {
        let len = self.current_line().len();
        if self.cursor_col > len {
            self.cursor_col = len;
        }
    }

    fn move_word_forward(&mut self) {
        // Walk the existing `Vec<String>` directly instead of joining
        // it into a fresh `entire_text()` buffer; the latter allocates
        // O(total bytes) per motion and dominated the profile at 5k+
        // lines (Phase 2 hotspot #2).
        //
        // `LineCursor::at` avoids the redundant O(rows) walk that
        // `cursor_byte_offset()` + `from_offset()` would do back-to-back.
        let mut cur = LineCursor::at(&self.lines, self.cursor_row, self.cursor_col);
        while cur.has_more() && !cur.is_word() {
            cur.advance();
        }
        while cur.has_more() && cur.is_word() {
            cur.advance();
        }
        // L16: skip trailing whitespace *including* newlines so `w`
        // lands on the next word even if the previous one was at
        // end-of-line.
        while cur.has_more() && cur.is_whitespace() {
            cur.advance();
        }
        self.cursor_row = cur.row;
        self.cursor_col = cur.col;
    }

    fn move_word_backward(&mut self) {
        let mut cur = LineCursor::at(&self.lines, self.cursor_row, self.cursor_col);
        if cur.row == 0 && cur.col == 0 {
            return;
        }
        cur.retreat();
        while !cur.at_start() && !cur.is_word() {
            cur.retreat();
        }
        // Stop one before the start of the word — mirrors the previous
        // `bytes[idx - 1]` peek by checking the *previous* byte before
        // each retreat.
        while !cur.at_start() && cur.peek_prev_is_word() {
            cur.retreat();
        }
        self.cursor_row = cur.row;
        self.cursor_col = cur.col;
    }

    fn raw_insert_char(&mut self, c: char) {
        let col = self.cursor_col;
        self.current_line_mut().insert(col, c);
        self.cursor_col += c.len_utf8();
    }

    /// Returns the character immediately after the cursor, if any.
    fn next_char(&self) -> Option<char> {
        let line = self.current_line();
        line[self.cursor_col..].chars().next()
    }

    /// Returns the character immediately before the cursor, if any.
    fn prev_char(&self) -> Option<char> {
        let line = self.current_line();
        if self.cursor_col == 0 {
            return None;
        }
        let mut idx = self.cursor_col;
        while !line.is_char_boundary(idx) && idx > 0 {
            idx -= 1;
        }
        line[..idx].chars().next_back()
    }

    /// Delete the character before the cursor (classic backspace).
    fn delete_prev_char(&mut self) {
        if self.cursor_col > 0 {
            let cursor_col = self.cursor_col;
            let line = self.current_line_mut();
            let mut new_col = cursor_col - 1;
            while !line.is_char_boundary(new_col) && new_col > 0 {
                new_col -= 1;
            }
            line.replace_range(new_col..cursor_col, "");
            self.cursor_col = new_col;
        } else if self.cursor_row > 0 {
            let trailing = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
            self.lines[self.cursor_row].push_str(&trailing);
        }
    }

    /// Delete the character after the cursor (delete key).
    fn delete_next_char(&mut self) {
        let line_len = self.current_line().len();
        if self.cursor_col < line_len {
            let cursor_col = self.cursor_col;
            let line = self.current_line_mut();
            let mut end = cursor_col + 1;
            while !line.is_char_boundary(end) && end < line.len() {
                end += 1;
            }
            line.replace_range(cursor_col..end, "");
        }
    }

    /// Should we auto-pair this opener character?
    fn should_auto_pair(&self, opener: char) -> bool {
        // No auto-pair inside a string literal.
        if self.cursor_inside_string_literal() {
            return false;
        }
        // No auto-pair when the next character is itself an opener
        // (prevents over-pairing like `((` → `(()) (`).
        if let Some(next) = self.next_char() {
            if PAIRS.iter().any(|(o, _)| *o == next) {
                return false;
            }
        }
        // For same-char pairs (' and ` and "), don't auto-pair if the
        // character before the cursor is the same opener (prevents `''`
        // turning into `''''`).
        if (opener == '\'' || opener == '"' || opener == '`') && self.prev_char() == Some(opener) {
            return false;
        }
        true
    }

    /// Detect whether the cursor sits inside a string literal on the
    /// current line. A simple heuristic: walk from column 0 to the
    /// cursor, toggling "inside `'`" and "inside `\"`" flags on each
    /// unescaped quote. If either flag is set when we reach the cursor
    /// column, we're inside a string.
    fn cursor_inside_string_literal(&self) -> bool {
        let line = self.current_line();
        let target = self.cursor_col.min(line.len());

        let mut inside_single = false;
        let mut inside_double = false;
        let mut prev_was_backslash = false;

        for (i, ch) in line.char_indices() {
            if i >= target {
                break;
            }
            match ch {
                '\\' => {
                    prev_was_backslash = !prev_was_backslash;
                }
                '\'' if !prev_was_backslash && !inside_double => {
                    inside_single = !inside_single;
                    prev_was_backslash = false;
                }
                '"' if !prev_was_backslash && !inside_single => {
                    inside_double = !inside_double;
                    prev_was_backslash = false;
                }
                _ => {
                    prev_was_backslash = false;
                }
            }
        }
        inside_single || inside_double
    }
}

/// Per-line byte cursor used by the word-motion helpers to walk
/// `Vec<String>` without materialising a joined `String`.
///
/// `col == lines[row].len()` is a valid position and represents the
/// synthetic newline that separates `row` from `row + 1`. Advancing
/// past it crosses the line boundary; the synthetic newline counts
/// as one absolute byte so callers can reason about it the same way
/// the legacy `entire_text()`-based path did.
struct LineCursor<'a> {
    lines: &'a [String],
    row: usize,
    col: usize,
}

impl<'a> LineCursor<'a> {
    /// Construct a cursor positioned at `(row, col)` without doing the
    /// O(rows) prefix-sum walk that `from_offset` would require.
    /// Callers that already know the logical row/col use this; only
    /// the legacy offset-based call sites need the slower path.
    const fn at(lines: &'a [String], row: usize, col: usize) -> Self {
        Self { lines, row, col }
    }

    /// Whether there is at least one more byte to read at the cursor.
    fn has_more(&self) -> bool {
        match self.lines.get(self.row) {
            Some(line) if self.col < line.len() => true,
            Some(_) => self.row + 1 < self.lines.len(),
            None => false,
        }
    }

    /// True iff the cursor sits at the very start of the buffer
    /// (`(0, 0)`). Symmetric to `has_more()`'s end-of-buffer check.
    const fn at_start(&self) -> bool {
        self.row == 0 && self.col == 0
    }

    /// Char at the cursor, or `None` past the end. Returns `'\n'` for
    /// the synthetic newline between lines.
    ///
    /// H5: UTF-8 aware — decodes the next `char` rather than the next
    /// byte. `is_word()` / `is_whitespace()` now see whole codepoints,
    /// so multi-byte word characters (Turkish `şahin`, CJK, Cyrillic,
    /// etc.) are treated as word characters via `char::is_alphanumeric`.
    fn peek_char(&self) -> Option<char> {
        let line = self.lines.get(self.row)?;
        if self.col < line.len() {
            line[self.col..].chars().next()
        } else if self.row + 1 < self.lines.len() {
            Some('\n')
        } else {
            None
        }
    }

    fn is_word(&self) -> bool {
        self.peek_char().is_some_and(is_word_char)
    }

    fn is_whitespace(&self) -> bool {
        self.peek_char().is_some_and(char::is_whitespace)
    }

    /// `move_word_backward` peeks at the codepoint immediately before
    /// the cursor while standing at `idx`; this returns whether that
    /// codepoint is a word character without retreating. The position
    /// before `(self.row, 0)` is the synthetic newline separating the
    /// previous line, never a word character.
    fn peek_prev_is_word(&self) -> bool {
        if self.col > 0 {
            self.lines[self.row][..self.col]
                .chars()
                .next_back()
                .is_some_and(is_word_char)
        } else {
            false
        }
    }

    fn advance(&mut self) {
        let line = match self.lines.get(self.row) {
            Some(l) => l,
            None => return,
        };
        let line_len = line.len();
        if self.col < line_len {
            // Step a full UTF-8 codepoint (1..=4 bytes).
            if let Some(ch) = line[self.col..].chars().next() {
                self.col += ch.len_utf8();
            } else {
                self.col = line_len;
            }
        } else if self.row + 1 < self.lines.len() {
            // Stepping over the synthetic newline.
            self.row += 1;
            self.col = 0;
        }
    }

    fn retreat(&mut self) {
        if self.col > 0 {
            if let Some(ch) = self.lines[self.row][..self.col].chars().next_back() {
                self.col -= ch.len_utf8();
            } else {
                self.col -= 1;
            }
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.lines[self.row].len(); // synthetic-newline slot
        }
    }
}

/// Unicode-aware word-character predicate.
///
/// H5: replaces the prior ASCII-only `(u8) -> bool` so that Turkish
/// (`şahin`), Cyrillic, CJK, full-width Latin, etc. are recognised as
/// word characters in vim-style word motions and the completion-engine
/// prefix scan. Underscore is preserved as a word character to keep
/// SQL identifier semantics (`my_table`).
fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Snap a byte index backwards to the nearest UTF-8 char boundary.
///
/// Clamps to `s.len()` if the index is past the end. Stable Rust
/// does not expose `str::floor_char_boundary` yet, so we implement
/// it manually.
pub fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    idx = idx.min(s.len());
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_navigate() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("SELECT 1\nSELECT 2");
        assert_eq!(buf.lines(), &["SELECT 1", "SELECT 2"]);
        assert_eq!(buf.cursor(), (1, 8));
        buf.apply_motion(Motion::LineStart, 1);
        assert_eq!(buf.cursor(), (1, 0));
        buf.apply_motion(Motion::Up, 1);
        assert_eq!(buf.cursor_row(), 0);
    }

    #[test]
    fn delete_char_at_line_join() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("ab\ncd");
        buf.apply_motion(Motion::LineStart, 1);
        buf.delete_char();
        assert_eq!(buf.lines(), &["abcd"]);
        assert_eq!(buf.cursor(), (0, 2));
    }

    #[test]
    fn current_word_prefix_and_replace_round_trip() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("SELECT * FROM ord");
        assert_eq!(buf.current_word_prefix(), "ord");
        buf.replace_current_word_with("orders");
        assert_eq!(buf.lines(), &["SELECT * FROM orders"]);
        assert_eq!(buf.cursor(), (0, 20));

        let mut buf2 = EditorBuffer::new();
        buf2.insert_str("foo ");
        assert_eq!(buf2.current_word_prefix(), "");
    }

    #[test]
    fn word_motion_skips_non_word_chars() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("foo bar baz");
        buf.apply_motion(Motion::LineStart, 1);
        buf.apply_motion(Motion::WordForward, 1);
        assert_eq!(buf.cursor().1, 4);
        buf.apply_motion(Motion::WordForward, 1);
        assert_eq!(buf.cursor().1, 8);
        buf.apply_motion(Motion::WordBackward, 1);
        assert_eq!(buf.cursor().1, 4);
    }

    #[test]
    fn word_motion_treats_unicode_as_word_chars() {
        // H5 regression: Turkish letters used to count as non-word
        // bytes, so `w` stopped between every multi-byte character.
        let mut buf = EditorBuffer::new();
        buf.insert_str("şahin köpek");
        buf.apply_motion(Motion::LineStart, 1);
        buf.apply_motion(Motion::WordForward, 1);
        // After `şahin ` (6 bytes + 1 space) the cursor lands on `k`.
        assert_eq!(buf.cursor(), (0, 7));
        buf.apply_motion(Motion::WordBackward, 1);
        assert_eq!(buf.cursor(), (0, 0));
    }

    #[test]
    fn word_motion_handles_cjk_and_mixed_scripts() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("中文 ENG русский");
        buf.apply_motion(Motion::LineStart, 1);
        buf.apply_motion(Motion::WordForward, 1);
        // "中文 " → 6 bytes + 1 space; cursor at start of "ENG".
        assert_eq!(buf.cursor(), (0, 7));
        buf.apply_motion(Motion::WordForward, 1);
        // "ENG " → 4 more bytes; cursor at start of "русский".
        assert_eq!(buf.cursor(), (0, 11));
    }

    #[test]
    fn current_word_prefix_handles_unicode_identifiers() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("SELECT * FROM kullanıcı");
        // Prefix should be the whole Turkish identifier, not just the
        // ASCII tail — the byte-walking version stopped at the first
        // non-ASCII byte and returned a partial "cı".
        assert_eq!(buf.current_word_prefix(), "kullanıcı");
    }

    #[test]
    fn multi_cursor_starts_empty() {
        let buf = EditorBuffer::new();
        assert!(!buf.has_multi_cursors());
        assert!(buf.secondary_cursors().is_empty());
    }

    #[test]
    fn add_secondary_cursor_keeps_sorted_and_dedupes() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("foo\nbar\nbaz");
        assert!(buf.add_secondary_cursor(0, 1));
        assert!(buf.add_secondary_cursor(2, 0));
        // Duplicate: rejected.
        assert!(!buf.add_secondary_cursor(0, 1));
        // Coincides with primary cursor (2, 3): rejected.
        assert!(!buf.add_secondary_cursor(2, 3));
        assert_eq!(buf.secondary_cursors(), &[(0, 1), (2, 0)]);
    }

    #[test]
    fn collapse_to_primary_drops_secondaries() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("foo bar");
        buf.add_secondary_cursor(0, 3);
        assert!(buf.has_multi_cursors());
        buf.collapse_to_primary();
        assert!(!buf.has_multi_cursors());
    }

    #[test]
    fn insert_char_propagates_to_secondaries_on_same_row() {
        // Primary at end "foo bar|"; add secondary after "foo" (col 3).
        let mut buf = EditorBuffer::new();
        buf.insert_str("foo bar");
        buf.add_secondary_cursor(0, 3);
        buf.set_auto_pair_enabled(false);
        buf.insert_char('X');
        // Each cursor inserts an 'X'. Primary advances; secondary too.
        // Original "foo bar" length 7. After inserts at col 3 and col 7:
        // "fooX barX" — secondary now at col 4, primary at col 9.
        assert_eq!(buf.lines(), &["fooX barX"]);
        assert_eq!(buf.cursor(), (0, 9));
        assert_eq!(buf.secondary_cursors(), &[(0, 4)]);
    }

    #[test]
    fn insert_char_propagates_across_rows() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("foo\nbar");
        // Primary now at (1, 3). Add secondary at end of first line.
        buf.add_secondary_cursor(0, 3);
        buf.set_auto_pair_enabled(false);
        buf.insert_char('!');
        assert_eq!(buf.lines(), &["foo!", "bar!"]);
        assert_eq!(buf.cursor(), (1, 4));
        assert_eq!(buf.secondary_cursors(), &[(0, 4)]);
    }

    #[test]
    fn delete_char_propagates_to_secondaries() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("abc def");
        // Primary at (0, 7). Secondary at end of "abc" (col 3).
        buf.add_secondary_cursor(0, 3);
        buf.set_auto_pair_enabled(false);
        buf.delete_char();
        // After deleting char before col 7 and col 3:
        // "ab de" (5 chars)
        assert_eq!(buf.lines(), &["ab de"]);
        assert_eq!(buf.cursor(), (0, 5));
        assert_eq!(buf.secondary_cursors(), &[(0, 2)]);
    }

    #[test]
    fn add_next_word_match_finds_repeat() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("foo bar foo baz foo");
        // Move primary to inside the first "foo" (col 2).
        buf.set_cursor(0, 2);
        assert!(buf.add_secondary_cursor_at_next_word_match());
        // "foo" ends at col 11 (after "foo bar foo").
        assert_eq!(buf.secondary_cursors(), &[(0, 11)]);
    }

    #[test]
    fn add_all_word_matches_adds_every_other_occurrence() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("foo bar foo baz foo");
        // Primary inside first foo.
        buf.set_cursor(0, 2);
        let added = buf.add_secondary_cursors_at_all_word_matches();
        // "foo" appears 3 times; primary covers one (ends at col 3),
        // so 2 secondaries added at the other ends (11 and 19).
        assert_eq!(added, 2);
        assert_eq!(buf.secondary_cursors(), &[(0, 11), (0, 19)]);
    }

    #[test]
    fn add_word_match_without_word_returns_none() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("   ");
        buf.set_cursor(0, 1);
        assert!(!buf.add_secondary_cursor_at_next_word_match());
        assert_eq!(buf.add_secondary_cursors_at_all_word_matches(), 0);
    }

    #[test]
    fn insert_str_with_newline_clears_secondaries() {
        // Newline-paste semantic: secondaries reset to keep behaviour
        // predictable until proper multi-line paste is implemented.
        let mut buf = EditorBuffer::new();
        buf.insert_str("foo");
        buf.add_secondary_cursor(0, 1);
        buf.insert_str("\nbar");
        assert!(!buf.has_multi_cursors());
    }

    #[test]
    fn floor_char_boundary_handles_multibyte() {
        let line = "şahin";
        assert_eq!(floor_char_boundary(line, 0), 0);
        assert_eq!(floor_char_boundary(line, 1), 0);
        assert_eq!(floor_char_boundary(line, 2), 2);
        assert_eq!(floor_char_boundary(line, 6), 6);
        assert_eq!(floor_char_boundary(line, 99), 6);
    }

    // -- M3.1: operator range and delete helpers ---------------------------

    #[test]
    fn operator_range_text_current_line() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("line0\nline1\nline2");
        buf.set_cursor(1, 3);
        assert_eq!(buf.operator_range_text(Motion::CurrentLine, 1), "line1");
        assert_eq!(
            buf.operator_range_text(Motion::CurrentLine, 2),
            "line1\nline2"
        );
    }

    #[test]
    fn operator_range_text_word_forward() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("foo bar baz");
        buf.apply_motion(Motion::LineStart, 1);
        assert_eq!(buf.operator_range_text(Motion::WordForward, 1), "foo ");
    }

    #[test]
    fn operator_delete_word() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("hello world");
        buf.apply_motion(Motion::LineStart, 1);
        buf.apply_operator_delete(Motion::WordForward, 1);
        assert_eq!(buf.lines(), &["world".to_owned()]);
        assert_eq!(buf.cursor(), (0, 0));
    }

    #[test]
    fn operator_delete_line() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("line0\nline1\nline2");
        buf.set_cursor(1, 2);
        buf.apply_operator_delete(Motion::CurrentLine, 1);
        assert_eq!(buf.lines(), &["line0".to_owned(), "line2".to_owned()]);
        assert_eq!(buf.cursor_row(), 1);
    }

    #[test]
    fn operator_yank_line() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("line0\nline1\nline2");
        buf.set_cursor(1, 2);
        let yanked = buf.operator_range_text(Motion::CurrentLine, 1);
        assert_eq!(yanked, "line1");
        // Buffer unchanged after yank
        assert_eq!(buf.lines(), &["line0", "line1", "line2"]);
    }

    #[test]
    fn operator_delete_to_file_start() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("line0\nline1\nline2");
        buf.set_cursor(2, 3);
        let yanked = buf.operator_range_text(Motion::FileStart, 1);
        assert_eq!(yanked, "line0\nline1\nline2");
    }

    #[test]
    fn operator_delete_multiple_lines() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("a\nb\nc\nd");
        buf.set_cursor(1, 0);
        buf.apply_operator_delete(Motion::CurrentLine, 2);
        assert_eq!(buf.lines(), &["a".to_owned(), "d".to_owned()]);
    }

    // ---------- selection integration ------------------------------

    fn buf_with(lines: &[&str]) -> EditorBuffer {
        let mut b = EditorBuffer::new();
        b.insert_str(&lines.join("\n"));
        b.set_cursor(0, 0);
        b
    }

    #[test]
    fn selection_starts_clear() {
        let buf = EditorBuffer::new();
        assert!(buf.selection().is_none());
        assert!(!buf.has_selection());
        assert!(buf.selected_text().is_empty());
    }

    #[test]
    fn set_selection_clamps_out_of_range_positions() {
        let mut buf = buf_with(&["hello"]);
        buf.set_selection(Some(Selection::character((0, 0), (5, 99))));
        let sel = buf.selection().expect("selection set");
        assert_eq!(sel.anchor, (0, 0));
        assert_eq!(sel.head, (0, 5));
    }

    #[test]
    fn extend_selection_to_opens_at_cursor() {
        let mut buf = buf_with(&["abcdef"]);
        buf.set_cursor(0, 2);
        buf.extend_selection_to(0, 5);
        let sel = buf.selection().expect("selection opened");
        assert_eq!(sel.anchor, (0, 2));
        assert_eq!(sel.head, (0, 5));
        assert_eq!(buf.selected_text(), "cde");
    }

    #[test]
    fn selected_text_single_line() {
        let mut buf = buf_with(&["hello world"]);
        buf.set_selection(Some(Selection::character((0, 6), (0, 11))));
        assert_eq!(buf.selected_text(), "world");
    }

    #[test]
    fn selected_text_multi_line_character() {
        let mut buf = buf_with(&["ab", "cd", "ef"]);
        buf.set_selection(Some(Selection::character((0, 1), (2, 1))));
        assert_eq!(buf.selected_text(), "b\ncd\ne");
    }

    #[test]
    fn selected_text_line_kind_includes_newlines() {
        let mut buf = buf_with(&["alpha", "beta", "gamma"]);
        buf.set_selection(Some(Selection::line((0, 0), (1, 99))));
        assert_eq!(buf.selected_text(), "alpha\nbeta\n");
    }

    #[test]
    fn delete_selection_single_line_collapses_text() {
        let mut buf = buf_with(&["abcdef"]);
        buf.set_selection(Some(Selection::character((0, 1), (0, 4))));
        let removed = buf.delete_selection();
        assert_eq!(removed, "bcd");
        assert_eq!(buf.lines(), &["aef".to_owned()]);
        assert!(buf.selection().is_none());
        assert_eq!((buf.cursor_row(), buf.cursor_col()), (0, 1));
    }

    #[test]
    fn delete_selection_multi_line_character_stitches_lines() {
        let mut buf = buf_with(&["hello", "world", "!!!"]);
        buf.set_selection(Some(Selection::character((0, 2), (2, 1))));
        let removed = buf.delete_selection();
        assert_eq!(removed, "llo\nworld\n!");
        assert_eq!(buf.lines(), &["he".to_owned() + "!!"]);
        assert_eq!((buf.cursor_row(), buf.cursor_col()), (0, 2));
    }

    #[test]
    fn delete_selection_line_kind_drops_full_rows() {
        let mut buf = buf_with(&["a", "b", "c", "d"]);
        buf.set_selection(Some(Selection::line((1, 0), (2, 0))));
        let removed = buf.delete_selection();
        assert_eq!(removed, "b\nc\n");
        assert_eq!(buf.lines(), &["a".to_owned(), "d".to_owned()]);
    }

    #[test]
    fn delete_selection_returns_empty_when_no_selection() {
        let mut buf = buf_with(&["abc"]);
        assert_eq!(buf.delete_selection(), "");
        assert_eq!(buf.lines(), &["abc".to_owned()]);
    }

    #[test]
    fn clear_selection_drops_state() {
        let mut buf = buf_with(&["abc"]);
        buf.set_selection(Some(Selection::character((0, 0), (0, 2))));
        buf.clear_selection();
        assert!(buf.selection().is_none());
    }

    #[test]
    fn has_selection_false_for_empty_range() {
        let mut buf = buf_with(&["abc"]);
        buf.set_selection(Some(Selection::at((0, 1))));
        assert!(buf.selection().is_some());
        assert!(!buf.has_selection());
    }

    #[test]
    fn begin_or_extend_selection_grows_with_cursor() {
        let mut buf = buf_with(&["abcdef"]);
        buf.set_cursor(0, 1);
        buf.begin_or_extend_selection(SelectionKind::Character);
        // No movement — selection is empty but present.
        assert!(buf.selection().is_some());
        buf.set_cursor(0, 4);
        buf.begin_or_extend_selection(SelectionKind::Character);
        let sel = buf.selection().expect("selection extended");
        assert_eq!(sel.anchor, (0, 1));
        assert_eq!(sel.head, (0, 4));
    }

    // ---------- history access via buffer --------------------------

    #[test]
    fn history_starts_empty_on_new_buffer() {
        let buf = EditorBuffer::new();
        assert_eq!(buf.history().undo_len(), 0);
        assert_eq!(buf.history().redo_len(), 0);
        assert!(!buf.history().can_undo());
        assert!(!buf.history().can_redo());
    }

    #[test]
    fn history_mut_lets_handlers_record() {
        let mut buf = EditorBuffer::new();
        buf.history_mut().record(crate::editor::EditOp::Insert {
            at: (0, 0),
            text: "x".into(),
            cursor_before: (0, 0),
            cursor_after: (0, 1),
        });
        assert_eq!(buf.history().undo_len(), 1);
    }
}
