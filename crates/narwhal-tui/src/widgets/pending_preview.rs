//! Pending-changes preview modal (L36).
//!
//! Lists every staged mutation in queue order, one per line, with a
//! footer that lays out the commit / discard / close chords. The host
//! reconstructs the view from the live [`crate::widgets::pending_preview::PendingPreviewView`]
//! every frame; the modal owns no data of its own.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::theme::Theme;

/// Read-only snapshot of the queue at render time.
#[derive(Debug, Clone)]
pub struct PendingPreviewView<'a> {
    /// One entry per pending mutation, in queue order. Each is the
    /// `summary()` produced by `PendingMutation::summary` — already
    /// formatted to one line of human-readable SQL-ish text.
    pub mutations: &'a [String],
    /// First visible mutation index. Clamped at render time.
    pub scroll: u16,
}

/// Render the modal centred on `area`. Uses 70% of width and 60% of
/// height so the result pane stays partially visible beneath.
pub fn render_pending_preview(
    frame: &mut Frame<'_>,
    area: Rect,
    view: &PendingPreviewView<'_>,
    theme: &Theme,
) {
    let modal = centred_rect(70, 60, area);
    frame.render_widget(Clear, modal);

    let title = format!(" pending changes — {} queued ", view.mutations.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            title,
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let total = view.mutations.len() as u16;
    let scroll = view.scroll.min(total.saturating_sub(1));
    let items: Vec<ListItem<'_>> = view
        .mutations
        .iter()
        .enumerate()
        .skip(scroll as usize)
        .map(|(idx, summary)| {
            let prefix = format!(" {:>3}. ", idx + 1);
            ListItem::new(Line::from(vec![
                Span::styled(prefix, Style::default().fg(theme.muted)),
                Span::styled(summary.clone(), Style::default().fg(theme.foreground)),
            ]))
        })
        .collect();
    let list = List::new(items);
    frame.render_widget(list, chunks[0]);

    let hint = " Ctrl-S commit · Ctrl-X discard · j/k scroll · Esc/Ctrl-P close ";
    let hint_para = Paragraph::new(Line::from(Span::styled(
        hint,
        Style::default().fg(theme.muted),
    )));
    frame.render_widget(hint_para, chunks[1]);
}

fn centred_rect(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(vert[1])[1]
}
