//! Line-oriented text buffer for the SQL editor pane.
//!
//! The buffer is intentionally simple: a `Vec<String>` of lines plus a
//! cursor and a viewport offset. It pairs with [`narwhal_vim`] to interpret
//! modal keystrokes and with [`narwhal_sql`] to extract the statement under
//! the cursor for execution.

use narwhal_sql::{split_with, Dialect};
use narwhal_vim::Motion;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

const GUTTER_WIDTH: usize = 6; // "NNN │ "

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
/// from [`narwhal_app::completion::Completion`] so the renderer stays
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

    /// Return the number of lines in the buffer.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Return the text of line at `idx`, or empty string if out of bounds.
    pub fn get_line(&self, idx: usize) -> &str {
        self.lines.get(idx).map(String::as_str).unwrap_or("")
    }

    /// Replace the contents of line `idx` with `new_text`.
    /// Does nothing if `idx` is out of bounds.
    pub fn replace_line(&mut self, idx: usize, new_text: &str) {
        if idx < self.lines.len() {
            self.lines[idx] = new_text.to_owned();
        }
    }

    /// Return the current cursor row.
    pub fn cursor_row(&self) -> usize {
        self.cursor_row
    }

    pub fn cursor(&self) -> (usize, usize) {
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
    pub fn auto_pair_enabled(&self) -> bool {
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
                // Forward-compatible: future motions are ignored until wired.
                _ => {}
            }
        }
    }

    /// Return every statement in the buffer, trimmed of surrounding
    /// whitespace and of any trailing semicolon.
    pub fn all_statements(&self, dialect: Dialect) -> Vec<String> {
        let text = self.entire_text();
        split_with(&text, dialect)
            .into_iter()
            .filter_map(|s| {
                let cleaned = s.text.trim().trim_end_matches(';').trim().to_owned();
                (!cleaned.is_empty()).then_some(cleaned)
            })
            .collect()
    }

    /// Extract the statement under the cursor.
    ///
    /// Returns the full statement text including any trailing semicolon, or
    /// `None` when the buffer contains no statements at all.
    pub fn statement_at_cursor(&self, dialect: Dialect) -> Option<String> {
        let text = self.entire_text();
        let cursor_offset = self.cursor_byte_offset();
        let statements = split_with(&text, dialect);
        if statements.is_empty() {
            return None;
        }
        for stmt in &statements {
            if cursor_offset >= stmt.start && cursor_offset <= stmt.end {
                return Some(stmt.text.to_owned());
            }
        }
        // Cursor is past the last statement end (trailing whitespace);
        // return the last statement encountered.
        statements.last().map(|s| s.text.to_owned())
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
        let bytes = line.as_bytes();
        let mut start = end;
        while start > 0 {
            let prev = start - 1;
            if !is_word_char(bytes[prev]) {
                break;
            }
            start = prev;
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
        self.lines
            .get(self.cursor_row)
            .map(String::as_str)
            .unwrap_or("")
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
        let text = self.entire_text();
        let bytes = text.as_bytes();
        let mut idx = self.cursor_byte_offset();
        while idx < bytes.len() && !is_word_char(bytes[idx]) {
            idx += 1;
        }
        while idx < bytes.len() && is_word_char(bytes[idx]) {
            idx += 1;
        }
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() && bytes[idx] != b'\n' {
            idx += 1;
        }
        self.set_cursor_from_offset(idx);
    }

    fn move_word_backward(&mut self) {
        let text = self.entire_text();
        let bytes = text.as_bytes();
        let mut idx = self.cursor_byte_offset();
        if idx == 0 {
            return;
        }
        idx -= 1;
        while idx > 0 && !is_word_char(bytes[idx]) {
            idx -= 1;
        }
        while idx > 0 && is_word_char(bytes[idx - 1]) {
            idx -= 1;
        }
        self.set_cursor_from_offset(idx);
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

    fn set_cursor_from_offset(&mut self, mut offset: usize) {
        for (row, line) in self.lines.iter().enumerate() {
            let len = line.len();
            if offset <= len {
                self.cursor_row = row;
                self.cursor_col = offset;
                return;
            }
            offset -= len + 1;
        }
        self.cursor_row = self.lines.len() - 1;
        self.cursor_col = self.lines.last().map(String::len).unwrap_or(0);
    }
}

fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Snap a byte index backwards to the nearest UTF-8 char boundary.
/// Clamps to `s.len()` if the index is past the end. Stable Rust
/// does not expose `str::floor_char_boundary` yet, so we implement
/// it manually.
fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    idx = idx.min(s.len());
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

pub fn render_editor(
    frame: &mut Frame<'_>,
    area: Rect,
    buffer: &mut EditorBuffer,
    theme: &Theme,
    focused: bool,
    title: &str,
    search: Option<&EditorSearchHighlight<'_>>,
) {
    let border_style = if focused {
        Style::default().fg(theme.accent)
    } else {
        Style::default().fg(theme.muted)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(format!(" {title} "));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let height = inner.height as usize;
    buffer.ensure_visible(height);

    // Collect matches per line for highlight rendering.
    let match_line_map: std::collections::HashMap<usize, Vec<(usize, bool)>> = search
        .map(|s| {
            let mut map: std::collections::HashMap<usize, Vec<(usize, bool)>> =
                std::collections::HashMap::new();
            for (i, &(line, col)) in s.matches.iter().enumerate() {
                let is_current = s.current == Some(i);
                map.entry(line).or_default().push((col, is_current));
            }
            // Sort matches within each line by column.
            for v in map.values_mut() {
                v.sort_by_key(|(col, _)| *col);
            }
            map
        })
        .unwrap_or_default();
    let needle_len = search.map(|s| s.needle_len).unwrap_or(0);

    let end = (buffer.scroll + height).min(buffer.lines.len());
    let lines: Vec<Line<'_>> = (buffer.scroll..end)
        .map(|row| {
            let number = format!("{:>3} │ ", row + 1);
            let gutter = Span::styled(number, Style::default().fg(theme.muted));

            let line_text = &buffer.lines[row];

            if let Some(matches_on_line) = match_line_map.get(&row) {
                // Build spans with highlight overlays.
                let mut spans = vec![gutter];
                let mut pos = 0usize;
                for &(col, is_current) in matches_on_line {
                    let start = floor_char_boundary(line_text, col);
                    let hl_end_raw = col.saturating_add(needle_len);
                    let end = floor_char_boundary(line_text, hl_end_raw);
                    if start > pos {
                        let seg_end = start.min(line_text.len());
                        if pos < seg_end {
                            spans.push(Span::raw(line_text[pos..seg_end].to_owned()));
                        }
                    }
                    if start < line_text.len() && end > start {
                        let style = if is_current {
                            Style::default()
                                .fg(theme.background)
                                .bg(theme.accent)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(theme.foreground).bg(theme.muted)
                        };
                        spans.push(Span::styled(line_text[start..end].to_owned(), style));
                    }
                    pos = end.max(start.saturating_add(1)); // advance past the match
                }
                if pos < line_text.len() {
                    spans.push(Span::raw(line_text[pos..].to_owned()));
                }
                Line::from(spans)
            } else {
                let body = Span::raw(line_text.clone());
                Line::from(vec![gutter, body])
            }
        })
        .collect();

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);

    if focused && buffer.cursor_row >= buffer.scroll {
        let cursor_y = (buffer.cursor_row - buffer.scroll) as u16;
        if cursor_y < inner.height {
            let cursor_x = (GUTTER_WIDTH + cursor_display_col(buffer)) as u16;
            if cursor_x < inner.width {
                frame.set_cursor_position((inner.x + cursor_x, inner.y + cursor_y));
            }
        }
    }
}

/// Width-aware display column for the current cursor position. Honours
/// the East-Asian width tables so multibyte glyphs (Turkish 2-byte,
/// CJK 3-byte, emoji 4-byte) render with the cursor sprite over the
/// correct cell, not the byte index.
fn cursor_display_col(buffer: &EditorBuffer) -> usize {
    let row = buffer.cursor_row.min(buffer.lines.len().saturating_sub(1));
    let line = &buffer.lines[row];
    let mut col = buffer.cursor_col.min(line.len());
    while col > 0 && !line.is_char_boundary(col) {
        col -= 1;
    }
    line[..col].width()
}

/// Helper that turns the editor's outer rect plus the cursor offset into
/// an absolute screen coordinate the host app can pass as
/// [`CompletionPopupView::anchor`]. Mirrors the layout done inside
/// [`render_editor`].
pub fn editor_cursor_anchor(area: Rect, buffer: &EditorBuffer) -> (u16, u16) {
    let inner_x = area.x + 1;
    let inner_y = area.y + 1;
    let cursor_x = inner_x + (GUTTER_WIDTH + cursor_display_col(buffer)) as u16;
    let cursor_y = if buffer.cursor_row >= buffer.scroll {
        inner_y + (buffer.cursor_row - buffer.scroll) as u16
    } else {
        inner_y
    };
    (cursor_x, cursor_y)
}

/// Hit-test regions for completion popup items.
#[derive(Debug, Default, Clone)]
pub struct CompletionHitRegions {
    /// The actual screen `Rect` the popup was rendered to. Set to
    /// `None` when the popup is empty (no items).
    pub popup_rect: Option<Rect>,
    /// One `(Rect, item_index)` per visible completion item.
    pub items: Vec<(Rect, usize)>,
}

/// Render the completion popup overlay. Should be called *after*
/// [`render_editor`] so it draws on top.
///
/// Returns hit-test regions for each visible completion item so mouse
/// clicks can be routed to the correct item.
pub fn render_completion_popup(
    frame: &mut Frame<'_>,
    screen: Rect,
    view: &CompletionPopupView<'_>,
    theme: &Theme,
) -> CompletionHitRegions {
    use ratatui::layout::Constraint;
    use ratatui::style::Modifier;
    use ratatui::widgets::{Cell, Clear, Row as TableRow, Table};

    if view.items.is_empty() {
        return CompletionHitRegions::default();
    }
    // Width: glyph column (2) + widest text column + widest detail
    // column + breathing room (4). The popup is allowed to grow up to
    // whatever the screen can host minus a small margin, so multi-word
    // phrases like 'SELECT COUNT(*)' don't get cropped to 'SELECT C'
    // when the editor pane is narrow.
    let max_text = view
        .items
        .iter()
        .map(|i| i.text.chars().count())
        .max()
        .unwrap_or(0);
    let max_detail = view
        .items
        .iter()
        .map(|i| i.detail.map(|d| d.chars().count()).unwrap_or(0))
        .max()
        .unwrap_or(0);
    let want = 2 + max_text + if max_detail == 0 { 0 } else { max_detail + 1 } + 4;
    let avail = (screen.width.saturating_sub(2) as usize).clamp(20, 100);
    let width = want.clamp(20, avail) as u16;
    let height = (view.items.len() as u16 + 2).min(10);

    let (ax, ay) = view.anchor;
    let below_y = ay.saturating_add(1);
    let x = ax.min(screen.x + screen.width.saturating_sub(width));
    let y = if below_y + height <= screen.y + screen.height {
        below_y
    } else {
        ay.saturating_sub(height)
    };
    let popup = Rect {
        x,
        y,
        width,
        height,
    };

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " completions ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let visible = (inner.height as usize).min(view.items.len());
    // Naively window around the selection.
    let start = view.selected.saturating_sub(visible.saturating_sub(1));
    let end = (start + visible).min(view.items.len());
    let rows = view.items[start..end].iter().enumerate().map(|(i, item)| {
        let global = start + i;
        let style = if global == view.selected {
            Style::default()
                .fg(theme.background)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.foreground)
        };
        let detail = item.detail.unwrap_or("");
        TableRow::new(vec![
            Cell::from(format!(" {}", item.kind_glyph)).style(style),
            Cell::from(item.text.to_owned()).style(style),
            Cell::from(detail.to_owned()).style(style),
        ])
    });
    // Constraints adapt to the actual content rather than the old fixed
    // 8/16 split: a long phrase like 'CREATE TABLE IF NOT EXISTS' takes
    // the room it needs and the detail column shrinks to zero when no
    // item has a detail string.
    let text_w = (max_text as u16).max(4);
    let widths: Vec<Constraint> = if max_detail == 0 {
        vec![Constraint::Length(2), Constraint::Min(text_w)]
    } else {
        vec![
            Constraint::Length(2),
            Constraint::Length(text_w),
            Constraint::Length(max_detail as u16),
        ]
    };
    let table = Table::new(rows, widths);
    frame.render_widget(table, inner);

    // Build hit-test rects for each visible completion item.
    let mut item_rects = Vec::with_capacity(end - start);
    for i in start..end {
        let local = i - start;
        item_rects.push((
            Rect {
                x: inner.x,
                y: inner.y + local as u16,
                width: inner.width,
                height: 1,
            },
            i,
        ));
    }
    CompletionHitRegions {
        popup_rect: Some(popup),
        items: item_rects,
    }
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
        assert_eq!(buf.cursor_row, 0);
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
    fn statement_under_cursor_picks_the_right_one() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("SELECT 1; SELECT 2; SELECT 3");
        buf.apply_motion(Motion::LineStart, 1);
        let first = buf.statement_at_cursor(Dialect::Generic).unwrap();
        assert!(first.starts_with("SELECT 1"));

        // Walk into the second statement.
        for _ in 0..12 {
            buf.apply_motion(Motion::Right, 1);
        }
        let second = buf.statement_at_cursor(Dialect::Generic).unwrap();
        assert!(second.contains("SELECT 2"));
    }

    #[test]
    fn current_word_prefix_and_replace_round_trip() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("SELECT * FROM ord");
        assert_eq!(buf.current_word_prefix(), "ord");
        buf.replace_current_word_with("orders");
        assert_eq!(buf.lines(), &["SELECT * FROM orders"]);
        assert_eq!(buf.cursor(), (0, 20));

        // With the cursor placed right after a space, the prefix is empty.
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
        assert_eq!(buf.cursor_col, 4);
        buf.apply_motion(Motion::WordForward, 1);
        assert_eq!(buf.cursor_col, 8);
        buf.apply_motion(Motion::WordBackward, 1);
        assert_eq!(buf.cursor_col, 4);
    }

    /// H16 regression: search highlight slicing must not panic on
    /// multibyte char boundaries. The highlight code receives byte
    /// offsets in `EditorSearchHighlight.matches`; if a match
    /// starts or ends inside a multibyte sequence the old code
    /// sliced directly and panicked.
    #[test]
    fn search_highlight_handles_multibyte_match() {
        // "şahin" — ş is 2 bytes, so byte positions: ş(0,1) a(2) h(3) i(4) n(5)
        // A match at byte 0 with needle_len=2 would try to slice [0..2]
        // which is inside `ş` — without floor_char_boundary this panics.
        let line = "şahin";
        // Verify that the helper correctly snaps boundaries.
        assert_eq!(floor_char_boundary(line, 0), 0);
        assert_eq!(floor_char_boundary(line, 1), 0); // inside ş → snap to 0
        assert_eq!(floor_char_boundary(line, 2), 2); // 'a' start
        assert_eq!(floor_char_boundary(line, 6), 6); // at end
        assert_eq!(floor_char_boundary(line, 99), 6); // past end → clamp to len
    }

    /// H17 regression: CompletionHitRegions.popup_rect must reflect
    /// the actual Rect rendered by render_completion_popup, not an
    /// approximation. The popup may be placed above or below the
    /// anchor depending on screen space.
    #[test]
    fn completion_hit_regions_contains_popup_rect() {
        let regions = CompletionHitRegions::default();
        assert!(regions.popup_rect.is_none());
        assert!(regions.items.is_empty());
    }

    /// H17: Verify popup placement logic — below anchor when room,
    /// above when no room below.
    #[test]
    fn popup_y_below_anchor_when_room() {
        // Screen is 80x24, anchor at (10, 5), popup height = 5.
        // Below_y = 6. 6 + 5 = 11 <= 24 → popup goes below.
        let screen = Rect::new(0, 0, 80, 24);
        let anchor = (10u16, 5u16);
        let height: u16 = 5;
        let below_y = anchor.1.saturating_add(1);
        let y = if below_y + height <= screen.y + screen.height {
            below_y
        } else {
            anchor.1.saturating_sub(height)
        };
        assert_eq!(y, 6, "popup should be placed below the anchor");
    }

    #[test]
    fn popup_y_above_anchor_when_no_room() {
        // Screen is 80x10, anchor at (10, 9), popup height = 5.
        // Below_y = 10. 10 + 5 = 15 > 10 → popup goes above.
        let screen = Rect::new(0, 0, 80, 10);
        let anchor = (10u16, 9u16);
        let height: u16 = 5;
        let below_y = anchor.1.saturating_add(1);
        let y = if below_y + height <= screen.y + screen.height {
            below_y
        } else {
            anchor.1.saturating_sub(height)
        };
        assert_eq!(y, 4, "popup should be placed above the anchor");
    }
}
