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

    pub fn set_scroll(&mut self, scroll: usize) {
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
        if !self.auto_pair_enabled {
            self.raw_insert_char(c);
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

        self.raw_insert_char(c);
    }

    /// Set whether auto-pair is enabled.
    pub fn set_auto_pair_enabled(&mut self, on: bool) {
        self.auto_pair_enabled = on;
    }

    /// Returns whether auto-pair is enabled.
    pub const fn auto_pair_enabled(&self) -> bool {
        self.auto_pair_enabled
    }

    pub fn insert_str(&mut self, text: &str) {
        for ch in text.chars() {
            if ch == '\n' {
                let col = self.cursor_col;
                let tail = self.current_line_mut().split_off(col);
                self.lines.insert(self.cursor_row + 1, tail);
                self.cursor_row += 1;
                self.cursor_col = 0;
            } else {
                let col = self.cursor_col;
                self.current_line_mut().insert(col, ch);
                self.cursor_col += ch.len_utf8();
            }
        }
    }

    pub fn delete_char(&mut self) {
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
    pub fn ensure_visible(&mut self, height: usize) {
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
    fn floor_char_boundary_handles_multibyte() {
        let line = "şahin";
        assert_eq!(floor_char_boundary(line, 0), 0);
        assert_eq!(floor_char_boundary(line, 1), 0);
        assert_eq!(floor_char_boundary(line, 2), 2);
        assert_eq!(floor_char_boundary(line, 6), 6);
        assert_eq!(floor_char_boundary(line, 99), 6);
    }
}
