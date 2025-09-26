//! Line-oriented text buffer for the SQL editor pane.
//!
//! The buffer is intentionally simple: a `Vec<String>` of lines plus a
//! cursor and a viewport offset. It pairs with [`narwhal_vim`] to interpret
//! modal keystrokes and with [`narwhal_sql`] to extract the statement under
//! the cursor for execution.

use narwhal_sql::{split_with, Dialect};
use narwhal_vim::Motion;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::theme::Theme;

const GUTTER_WIDTH: usize = 6; // "NNN │ "

#[derive(Debug, Clone)]
pub struct EditorBuffer {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
    scroll: usize,
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
        }
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_row, self.cursor_col)
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn entire_text(&self) -> String {
        self.lines.join("\n")
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
        if self.cursor_col > 0 {
            let cursor_col = self.cursor_col;
            let line = self.current_line_mut();
            // Walk back one char boundary to support UTF-8.
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
            }
        }
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

    fn cursor_byte_offset(&self) -> usize {
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

pub fn render_editor(
    frame: &mut Frame<'_>,
    area: Rect,
    buffer: &mut EditorBuffer,
    theme: &Theme,
    focused: bool,
    title: &str,
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

    let end = (buffer.scroll + height).min(buffer.lines.len());
    let lines: Vec<Line<'_>> = (buffer.scroll..end)
        .map(|row| {
            let number = format!("{:>3} │ ", row + 1);
            let gutter = Span::styled(number, Style::default().fg(theme.muted));
            let body = Span::raw(buffer.lines[row].clone());
            Line::from(vec![gutter, body])
        })
        .collect();

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);

    if focused && buffer.cursor_row >= buffer.scroll {
        let cursor_y = (buffer.cursor_row - buffer.scroll) as u16;
        if cursor_y < inner.height {
            let cursor_x = (GUTTER_WIDTH + buffer.cursor_col) as u16;
            if cursor_x < inner.width {
                frame.set_cursor_position((inner.x + cursor_x, inner.y + cursor_y));
            }
        }
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
}
