//! Modal renderer for the connection wizard form.
//!
//! The widget is intentionally read-only with respect to the wizard state;
//! [`crate::theme::Theme`] supplies colours and the caller passes a frozen
//! [`WizardView`] each frame.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::theme::Theme;

/// Field labels and current values plus a hint flag for secret entries.
pub struct WizardFieldView<'a> {
    pub label: &'a str,
    pub value: &'a str,
    pub secret: bool,
}

pub struct WizardView<'a> {
    pub drivers: &'a [&'a str],
    pub driver_index: usize,
    pub fields: Vec<WizardFieldView<'a>>,
    /// 0 selects the driver row; 1..=N selects a field.
    pub focused: usize,
    pub error: Option<&'a str>,
}

pub fn render_wizard(frame: &mut Frame<'_>, area: Rect, view: &WizardView<'_>, theme: &Theme) {
    let width = area.width.saturating_sub(8).min(70);
    let height = area
        .height
        .saturating_sub(4)
        .min((view.fields.len() + 8) as u16);
    if width < 30 || height < 6 {
        return;
    }
    let popup = centred(area, width, height);
    frame.render_widget(Clear, popup);

    let title = " add connection · Tab/Shift-Tab move · Enter save · Esc cancel ";
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines: Vec<Line<'_>> = Vec::with_capacity(view.fields.len() + 4);

    // Driver row.
    let driver_focused = view.focused == 0;
    let driver_label_style = if driver_focused {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.muted)
    };
    let mut driver_spans: Vec<Span<'_>> = vec![Span::styled("  driver  ", driver_label_style)];
    for (i, d) in view.drivers.iter().enumerate() {
        let style = if i == view.driver_index {
            Style::default()
                .fg(theme.foreground)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.foreground)
        };
        driver_spans.push(Span::styled(format!(" {d} "), style));
        driver_spans.push(Span::raw(" "));
    }
    if driver_focused {
        driver_spans.push(Span::styled(
            "  (←/→ to cycle)",
            Style::default().fg(theme.muted),
        ));
    }
    lines.push(Line::from(driver_spans));
    lines.push(Line::from(""));

    // Fields.
    for (i, field) in view.fields.iter().enumerate() {
        let is_focused = view.focused == i + 1;
        let label_style = if is_focused {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        let display_value = if field.secret && !field.value.is_empty() {
            "•".repeat(field.value.chars().count())
        } else {
            field.value.to_owned()
        };
        let cursor_glyph = if is_focused { "▏" } else { " " };
        let value_style = Style::default().fg(theme.foreground);
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<10}", field.label), label_style),
            Span::raw("  "),
            Span::styled(display_value, value_style),
            Span::styled(cursor_glyph, value_style),
        ]));
    }

    if let Some(error) = view.error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  ! {error}"),
            Style::default().fg(theme.error),
        )));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

use super::centred_rect as centred;
