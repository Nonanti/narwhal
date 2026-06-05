//! Line-oriented text buffer for the SQL editor pane.
//!
//! The buffer is a `Vec<String>` of lines plus a cursor and viewport
//! offset. The buffer accepts a [`Motion`] from the caller — the app
//! layer converts from `narwhal_vim::Motion` at the boundary — and is
//! host-agnostic: terminal, GUI or headless renderers can all consume
//! it through immutable accessors.

use crate::motion::Motion;

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
        positions.sort_by_key(|&(r, c, _)| (r, c));

        let mut shifts: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
        let mut new_secondaries: Vec<(usize, usize)> = Vec::with_capacity(positions.len());
        let mut new_primary: (usize, usize) = (self.cursor_row, self.cursor_col);

        for (row, col_orig, is_primary) in positions {
            let row_shift = *shifts.get(&row).unwrap_or(&0);
            let col = col_orig + row_shift;
            if row >= self.lines.len() {
                continue;
            }
            let line = &mut self.lines[row];
            let col = col.min(line.len());
            line.insert(col, c);
            *shifts.entry(row).or_insert(0) += ch_len;
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
        // Multi-cursor insert_str collapses to the primary so callers
        // pasting large blobs / newlines don't end up duplicating the
        // payload at every secondary. The brief defers paste-into-
        // multi-cursor behaviour to v2.1; for now we explicitly clear
        // the secondary set so it's not silently desynced.
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
        positions.sort_by_key(|&(r, c, _)| (r, c));

        let mut shifts: std::collections::HashMap<usize, isize> = std::collections::HashMap::new();
        let mut new_secondaries: Vec<(usize, usize)> = Vec::with_capacity(positions.len());
        let mut new_primary: (usize, usize) = (self.cursor_row, self.cursor_col);

        for (row, col_orig, is_primary) in positions {
            let row_shift = *shifts.get(&row).unwrap_or(&0);
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
            *shifts.entry(row).or_insert(0) -= removed as isize;
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
}
