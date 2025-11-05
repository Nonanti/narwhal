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
        let sql_truncated = truncate_str(row.sql, sql_width);
        let ts = row.timestamp;
        let conn = row.connection;
        lines.push(Line::from(vec![
            Span::styled(format!(" {ts:timestamp_width$}"), style),
            Span::styled(format!(" {conn:connection_width$}"), style),
            Span::styled(format!(" {sql_truncated:sql_width$}"), style),
        ]));
    }

    // Pad remaining lines so the selection highlight fills the width.
    let body_height = inner.height.saturating_sub(1) as usize; // minus header
    while lines.len() < body_height + 1 {
        lines.push(Line::from(""));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Truncate a string to `max` chars, appending `…` if truncated.
fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_owned()
    } else {
        let mut end = max.saturating_sub(1);
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_str_short() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_exact() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn truncate_str_long() {
        let result = truncate_str("hello world", 6);
        assert_eq!(result, "hello…");
    }
}
