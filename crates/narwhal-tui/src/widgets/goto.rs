//! `:goto` fuzzy schema navigator overlay (v1.1 #1).
//!
//! Centred picker. Top line is the query input; below is a scrolling
//! list of `connection.schema.table` strings ranked by fuzzy score.
//! Selection is highlighted with the theme accent.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::theme::Theme;

/// One row in the picker. Borrowed against the host's owned modal
/// state so we don't allocate per render.
pub struct GotoRowView<'a> {
    /// `connection.schema.table` (or qualified.column once that
    /// kind is wired). Rendered as the row label.
    pub qualified: &'a str,
    /// One-letter badge to the right \u{2014} 'T' table, 'V' view, 'M'
    /// materialised, 'S' system. Empty string suppresses the badge.
    pub badge: &'a str,
}

/// Borrowed view of the picker state.
pub struct GotoModalView<'a> {
    pub query: &'a str,
    /// Visible rows for this frame (already sliced to the modal
    /// viewport by the caller).
    pub rows: Vec<GotoRowView<'a>>,
    /// Selected row \u{2014} index *into* `rows`, not into the full match
    /// list. The caller is responsible for keeping the selection on
    /// screen.
    pub selected: usize,
    /// Total matches (informational \u{2014} e.g. "3 / 248").
    pub total: usize,
}

pub fn render_goto_modal(
    frame: &mut Frame<'_>,
    area: Rect,
    view: &GotoModalView<'_>,
    theme: &Theme,
) {
    let width = (area.width * 7 / 10).clamp(40, 100);
    let height = (area.height * 8 / 10).clamp(10, 30);
    if area.width < 30 || area.height < 8 {
        return;
    }
    let popup = centred(area, width, height);
    frame.render_widget(Clear, popup);

    let title = format!(
        " goto \u{00b7} {} match{} \u{00b7} esc cancels ",
        view.total,
        if view.total == 1 { "" } else { "es" }
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            title,
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // Split inner: query line (top) + thin separator + result list.
    if inner.height < 3 {
        return;
    }
    let query_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    let sep_area = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: 1,
    };
    let list_area = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width,
        height: inner.height - 2,
    };

    let prompt = Line::from(vec![
        Span::styled(
            " > ",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(view.query, Style::default().fg(theme.foreground)),
        Span::styled("\u{2588}", Style::default().fg(theme.accent)),
    ]);
    frame.render_widget(Paragraph::new(prompt), query_area);

    let sep_line: String = "\u{2500}".repeat(sep_area.width as usize);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            sep_line,
            Style::default().fg(theme.accent),
        ))),
        sep_area,
    );

    let mut lines: Vec<Line<'_>> = Vec::with_capacity(view.rows.len());
    for (i, row) in view.rows.iter().enumerate() {
        let selected = i == view.selected;
        let row_style = if selected {
            Style::default()
                .bg(theme.accent)
                .fg(ratatui::style::Color::Black)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.foreground)
        };
        let badge_style = if selected {
            row_style
        } else {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        };
        let prefix = if selected { "\u{25b8} " } else { "  " };
        let label = format!("{prefix}{}", row.qualified);
        let mut spans: Vec<Span<'_>> = vec![Span::styled(label, row_style)];
        if !row.badge.is_empty() {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(format!("[{}]", row.badge), badge_style));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), list_area);
}

fn centred(area: Rect, width: u16, height: u16) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}
