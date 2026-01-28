//! Modal popups rendered above the result grid: in-place cell
//! edit, read-only cell detail, EXPLAIN plan.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::cells::sanitize_for_display;
use super::model::{CellEditView, CellPopup, ExplainPlanLine};
use crate::theme::Theme;

use crate::widgets::centred_rect;

pub(super) fn draw_cell_edit(frame: &mut Frame<'_>, area: Rect, edit: &CellEditView, theme: &Theme) {
    let width = area
        .width
        .saturating_sub(8)
        .min(crate::constants::CELL_POPUP_MAX_WIDTH);
    let height = area.height.saturating_sub(4).min(12);
    if width < 20 || height < 5 {
        return;
    }
    let popup_area = centred_rect(area, width, height);
    frame.render_widget(Clear, popup_area);
    let title = format!(
        " edit cell · row {} · {} ({}) · Enter saves · Esc cancels ",
        edit.row_index + 1,
        edit.column_name,
        edit.column_type
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let mut lines: Vec<Line<'_>> = Vec::new();
    // Render the editable buffer with a trailing block cursor so the user
    // can see where the next keystroke goes.
    lines.push(Line::from(vec![
        Span::raw(edit.buffer.clone()),
        Span::styled("▏", Style::default().fg(theme.accent)),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  type NULL (any case) to set the cell to NULL",
        Style::default().fg(theme.muted),
    )));
    if let Some(error) = &edit.error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  error: {error}"),
            Style::default().fg(theme.error),
        )));
    }
    let paragraph = Paragraph::new(lines)
        .style(Style::default().fg(theme.foreground))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

pub(super) fn draw_cell_popup(frame: &mut Frame<'_>, area: Rect, popup: &CellPopup, theme: &Theme) {
    let width = area
        .width
        .saturating_sub(8)
        .min(crate::constants::CELL_POPUP_MAX_WIDTH);
    let height = area.height.saturating_sub(4).min(20);
    if width < 20 || height < 5 {
        return;
    }
    let popup_area = centred_rect(area, width, height);
    frame.render_widget(Clear, popup_area);
    let title = format!(
        " cell · row {} · {} ({}) ",
        popup.row_index + 1,
        popup.column_name,
        popup.column_type
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);
    let sanitized_value = sanitize_for_display(&popup.value_text);
    let paragraph = Paragraph::new(sanitized_value.as_ref())
        .style(Style::default().fg(theme.foreground))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}


pub(super) fn draw_explain(
    frame: &mut Frame<'_>,
    area: Rect,
    lines: &[ExplainPlanLine],
    planning_time_ms: Option<f64>,
    execution_time_ms: Option<f64>,
    theme: &Theme,
) {
    let mut rendered: Vec<Line<'_>> = Vec::with_capacity(lines.len() + 2);
    if let (Some(p), Some(e)) = (planning_time_ms, execution_time_ms) {
        rendered.push(Line::from(Span::styled(
            format!("  planning {p:.3} ms · execution {e:.3} ms"),
            Style::default().fg(theme.muted),
        )));
        rendered.push(Line::from(""));
    }
    for line in lines {
        let indent = "  ".repeat(line.depth);
        let glyph = if line.depth == 0 { "▸" } else { "└" };
        let style = if line.depth == 0 {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.foreground)
        };
        rendered.push(Line::from(vec![
            Span::raw(format!("  {indent}{glyph} ")),
            Span::styled(line.text.clone(), style),
        ]));
    }
    let paragraph = Paragraph::new(rendered);
    frame.render_widget(paragraph, area);
}

