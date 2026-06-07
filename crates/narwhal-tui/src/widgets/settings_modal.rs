//! In-app settings modal renderer.
//!
//! The modal is laid out as a centered overlay covering ~80% of the
//! screen, split into a left-hand section list (Editor / Theme /
//! Display / Keybindings) and a right-hand field grid that shows
//! the currently-selected section's knobs. A footer carries the
//! shortcut legend and the dirty / message hint owned by the host.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::theme::Theme;

/// Borrowed view of the settings modal state. The host fills this
/// per render; the renderer never touches the live `Settings`.
pub struct SettingsModalView<'a> {
    /// Section labels in display order.
    pub sections: &'a [&'a str],
    /// Field rows for the currently-active section. Each row is a
    /// `(label, value)` pair already stringified by the host.
    pub fields: &'a [(&'a str, String)],
    pub selected_section: usize,
    pub selected_field: usize,
    pub dirty: bool,
    pub footer: &'a str,
}

/// Render the modal centered inside `screen`.
pub fn render_settings_modal(
    frame: &mut Frame<'_>,
    screen: Rect,
    view: &SettingsModalView<'_>,
    theme: &Theme,
) {
    // 80% width, 70% height, clamped to leave at least a 2-cell margin.
    let width = (f32::from(screen.width) * 0.8) as u16;
    let height = (f32::from(screen.height) * 0.7) as u16;
    let area = Rect {
        x: screen.x + (screen.width.saturating_sub(width)) / 2,
        y: screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, area);

    let title = if view.dirty { " settings * " } else { " settings " };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Top tab bar for sections + bottom footer; the middle splits
    // into a 25/75 section list / field area.
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(inner);

    render_tabs(frame, vertical[0], view, theme);

    let middle = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(20), Constraint::Min(1)])
        .split(vertical[1]);
    render_section_list(frame, middle[0], view, theme);
    render_field_grid(frame, middle[1], view, theme);

    render_footer(frame, vertical[2], view, theme);
}

fn render_tabs(frame: &mut Frame<'_>, area: Rect, view: &SettingsModalView<'_>, theme: &Theme) {
    let spans: Vec<Span<'_>> = view
        .sections
        .iter()
        .enumerate()
        .flat_map(|(idx, label)| {
            let style = if idx == view.selected_section {
                Style::default()
                    .bg(theme.accent)
                    .fg(theme.background)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.foreground)
            };
            vec![
                Span::styled(format!(" {label} "), style),
                Span::raw(" "),
            ]
        })
        .collect();
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_section_list(
    frame: &mut Frame<'_>,
    area: Rect,
    view: &SettingsModalView<'_>,
    theme: &Theme,
) {
    let lines: Vec<Line<'_>> = view
        .sections
        .iter()
        .enumerate()
        .map(|(idx, label)| {
            let style = if idx == view.selected_section {
                Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
            };
            Line::from(Span::styled(format!("  {label}"), style))
        })
        .collect();
    let block = Block::default().borders(Borders::RIGHT).border_style(Style::default().fg(theme.muted));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_field_grid(
    frame: &mut Frame<'_>,
    area: Rect,
    view: &SettingsModalView<'_>,
    theme: &Theme,
) {
    let lines: Vec<Line<'_>> = view
        .fields
        .iter()
        .enumerate()
        .map(|(idx, (label, value))| {
            let active = idx == view.selected_field;
            let label_style = if active {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.foreground)
            };
            let value_style = if active {
                Style::default()
                    .bg(theme.accent)
                    .fg(theme.background)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
            };
            Line::from(vec![
                Span::styled(format!("  {label:<24}"), label_style),
                Span::styled(format!(" {value} "), value_style),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, view: &SettingsModalView<'_>, theme: &Theme) {
    let style = if view.dirty {
        Style::default().fg(theme.warning).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.muted)
    };
    let p = Paragraph::new(view.footer).style(style);
    frame.render_widget(p, area);
}
