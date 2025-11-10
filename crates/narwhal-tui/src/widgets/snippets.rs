//! Snippets modal renderer (`:snippets`).
//!
//! Renders a centred modal overlay listing saved snippet names.
//! Mirrors the layout of the 06-05 history modal: centred Rect, Block
//! border with title, one name per row.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::theme::Theme;
use crate::widgets::help::centred;

/// View model passed from AppCore to the render path.
pub struct SnippetsModalState<'a> {
    /// Snippet names to display.
    pub entries: Vec<&'a str>,
    /// Index of the currently selected entry.
    pub selected: usize,
}

/// Render the snippets modal on top of the current frame.
///
/// The modal occupies a centred rectangle (40% width × 60% height,
/// capped at 50×20) and displays one snippet name per row.
/// The selected row is rendered in reverse video.
pub fn render_snippets_modal(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &SnippetsModalState<'_>,
    theme: &Theme,
) {
    let max_width: u16 = 50;
    let max_height: u16 = 20;
    let width = (area.width * 4 / 10).min(max_width);
    let height = (area.height * 6 / 10).min(max_height);
    if width < 20 || height < 6 {
        return;
    }
    let popup = centred(area, width, height);
    frame.render_widget(Clear, popup);

    let total = state.entries.len();
    let title = format!(" snippets · {total} total ");

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

    let normal_style = Style::default().fg(theme.foreground);
    let selected_style = Style::default()
        .bg(theme.accent)
        .fg(ratatui::style::Color::Black);

    let mut lines: Vec<Line<'_>> = Vec::new();

    for (i, name) in state.entries.iter().enumerate() {
        let style = if i == state.selected {
            selected_style
        } else {
            normal_style
        };
        lines.push(Line::from(Span::styled(format!(" {name}"), style)));
    }

    // Pad remaining lines so the selection highlight fills the width.
    let body_height = inner.height as usize;
    while lines.len() < body_height {
        lines.push(Line::from(""));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}
