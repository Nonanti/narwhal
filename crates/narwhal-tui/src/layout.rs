use narwhal_vim::Mode;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::theme::Theme;

/// Read-only view model rendered by [`render_root`].
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
        .title(Span::styled(" narwhal ", view.theme.sidebar_title()));
    let body = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            "  no connection",
            Style::default().fg(view.theme.muted),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  :open to attach one",
            Style::default().fg(view.theme.muted),
        )),
    ])
    .block(block);
    frame.render_widget(body, area);
}

fn render_main(frame: &mut Frame<'_>, area: Rect, view: &RootLayout<'_>) {
    let block = Block::default().borders(Borders::ALL).title(" editor ");
    let body = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            "  press i to insert, Esc to leave insert, :q to quit",
            Style::default().fg(view.theme.muted),
        )),
    ])
    .block(block);
    frame.render_widget(body, area);
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
    frame.render_widget(
        Paragraph::new(mode_label).style(view.theme.mode_indicator()),
        parts[0],
    );

    frame.render_widget(Paragraph::new(" ").style(view.theme.status_bar()), parts[1]);

    let text = format!(" {} │ {}", view.connection_label, view.status_message);
    frame.render_widget(
        Paragraph::new(text).style(view.theme.status_bar()),
        parts[2],
    );
}
