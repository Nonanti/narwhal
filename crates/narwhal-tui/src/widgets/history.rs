//! History modal renderer (Ctrl+R).
//!
//! Renders a centred modal overlay listing recent journal entries.
//! The caller provides a [`HistoryModalState`] with entries, filter
//! string, and selected index; rendering is pure — no I/O.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::theme::Theme;
use crate::widgets::help::centred;
use unicode_width::UnicodeWidthStr;

/// View model passed from AppCore to the render path. Owns only the
/// data needed for display; the full `HistoryState` stays in the core.
pub struct HistoryModalState<'a> {
    /// All loaded entries (used to compute `total`).
    pub total: usize,
    /// The filtered subset to render.
    pub visible: Vec<HistoryRow<'a>>,
    /// Current filter string.
    pub filter: &'a str,
    /// Index into the `visible` list.
    pub selected: usize,
}

/// One row in the history modal.
pub struct HistoryRow<'a> {
    /// Formatted timestamp: `YYYY-MM-DD HH:MM:SS`.
    pub timestamp: &'a str,
    /// Connection name (or `"<local>"` if absent).
    pub connection: &'a str,
    /// Single-line SQL preview (pre-truncated by the caller).
    pub sql: &'a str,
}

/// Render the history modal on top of the current frame.
///
/// The modal occupies a centred rectangle (60% width × 70% height,
/// capped at 80×24) and displays a three-column table:
/// timestamp | connection | SQL preview.
///
/// The selected row is rendered in reverse video.
pub fn render_history_modal(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &HistoryModalState<'_>,
    theme: &Theme,
) {
    let max_width: u16 = 80;
    let max_height: u16 = 24;
    let width = (area.width * 6 / 10).min(max_width);
    let height = (area.height * 7 / 10).min(max_height);
    if width < 30 || height < 6 {
        return;
    }
    let popup = centred(area, width, height);
    frame.render_widget(Clear, popup);

    let total = state.total;
    let visible_count = state.visible.len();
    let title = format!(
        " history · {visible_count}/{total}  filter: {}_ ",
        if state.filter.is_empty() {
            ""
        } else {
            state.filter
        }
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            &title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let timestamp_width: usize = 19;
    let connection_width: usize = 12;
    let sql_min_width: usize = 20;
    let inner_width = inner.width as usize;
    let sql_width = inner_width
        .saturating_sub(timestamp_width)
        .saturating_sub(connection_width)
        .saturating_sub(6) // padding/separators
        .max(sql_min_width);

    let mut lines: Vec<Line<'_>> = Vec::new();
    let header_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let normal_style = Style::default().fg(theme.foreground);
    let selected_style = Style::default()
        .bg(theme.accent)
        .fg(ratatui::style::Color::Black);

    // Column header
    lines.push(Line::from(vec![
        Span::styled(format!(" {:timestamp_width$}", "TIMESTAMP"), header_style),
        Span::styled(format!(" {:connection_width$}", "CONNECTION"), header_style),
        Span::styled(format!(" {:sql_width$}", "SQL"), header_style),
    ]));

    for (i, row) in state.visible.iter().enumerate() {
        let style = if i == state.selected {
            selected_style
        } else {
            normal_style
        };
        let sql_truncated = truncate_display(row.sql, sql_width);
        let ts = pad_to_width(row.timestamp, timestamp_width);
        let conn = pad_to_width(row.connection, connection_width);
        lines.push(Line::from(vec![
            Span::styled(format!(" {ts}"), style),
            Span::styled(format!(" {conn}"), style),
            Span::styled(format!(" {sql_truncated}"), style),
        ]));
    }

    // Pad remaining lines so the selection highlight fills the width.
    let body_height = inner.height.saturating_sub(1) as usize; // minus header
    while lines.len() < body_height + 1 {
        lines.push(Line::from(""));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Truncate a string so its **display width** does not exceed `max_width`
/// cells, appending `…` if truncated. Uses `unicode_width` so CJK,
/// emoji, and other wide characters are counted correctly.
fn truncate_display(s: &str, max_width: usize) -> String {
    if s.width() <= max_width {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = UnicodeWidthStr::width(ch.to_string().as_str());
        if w + cw + 1 > max_width {
            out.push('…');
            break;
        }
        out.push(ch);
        w += cw;
    }
    out
}

/// Pad a string with trailing spaces so its **display width** equals
/// `target_width` cells. Handles wide characters correctly by computing
/// the difference between display width and target.
fn pad_to_width(s: &str, target_width: usize) -> String {
    let display_w = s.width();
    let mut out = s.to_owned();
    let need = target_width.saturating_sub(display_w);
    out.extend(std::iter::repeat(' ').take(need));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_display_short() {
        assert_eq!(truncate_display("hello", 10), "hello");
    }

    #[test]
    fn truncate_display_exact() {
        assert_eq!(truncate_display("hello", 5), "hello");
    }

    #[test]
    fn truncate_display_long() {
        let result = truncate_display("hello world", 6);
        assert_eq!(result, "hello…");
    }

    #[test]
    fn truncate_display_respects_wide_chars() {
        // CJK character '中' has display width 2.
        // "中日" = 4 display cells. Truncating to 3 should yield "中…" (2+1=3).
        let result = truncate_display("中日", 3);
        assert_eq!(result, "中…");
    }

    #[test]
    fn pad_to_width_handles_wide_chars() {
        // '中' = 2 cells wide. Pad to 5 → need 3 spaces.
        let result = pad_to_width("中", 5);
        assert_eq!(result, "中   ");
    }

    #[test]
    fn pad_to_width_ascii() {
        let result = pad_to_width("abc", 6);
        assert_eq!(result, "abc   ");
    }
}
