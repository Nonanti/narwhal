use narwhal_vim::Mode;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::theme::Theme;

/// Stateless view-model passed in by the application each frame.
pub struct RootLayout<'a> {
    pub mode: Mode,
    pub connection_label: &'a str,
    pub status_message: &'a str,
    pub theme: &'a Theme,
}

pub fn render_root(frame: &mut Frame<'_>, area: Rect, view: &RootLayout<'_>) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(28), Constraint::Min(1)])
        .split(outer[0]);

    render_sidebar(frame, body[0], view);
    render_main(frame, body[1], view);
    render_status_bar(frame, outer[1], view);
}

fn render_sidebar(frame: &mut Frame<'_>, area: Rect, view: &RootLayout<'_>) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" 🐋 narwhal ", view.theme.sidebar_title()));
    let placeholder = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            "  no connection",
            Style::default().fg(view.theme.muted),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  press :open to add one",
            Style::default().fg(view.theme.muted),
        )),
    ])
    .block(block);
    frame.render_widget(placeholder, area);
}

fn render_main(frame: &mut Frame<'_>, area: Rect, view: &RootLayout<'_>) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" editor ");
    let placeholder = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            "  -- skeleton --",
            Style::default().fg(view.theme.muted),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  i = insert, Esc = normal, :q = quit",
            Style::default().fg(view.theme.muted),
        )),
    ])
    .block(block);
    frame.render_widget(placeholder, area);
}

fn render_status_bar(frame: &mut Frame<'_>, area: Rect, view: &RootLayout<'_>) {
    let parts = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(area);

    let mode_label = format!(" {} ", view.mode.short_label());
    let mode = Paragraph::new(mode_label).style(view.theme.mode_indicator());
    frame.render_widget(mode, parts[0]);

    let sep = Paragraph::new(" ").style(view.theme.status_bar());
    frame.render_widget(sep, parts[1]);

    let text = format!(" {} │ {}", view.connection_label, view.status_message);
    let status = Paragraph::new(text).style(view.theme.status_bar());
    frame.render_widget(status, parts[2]);
}
