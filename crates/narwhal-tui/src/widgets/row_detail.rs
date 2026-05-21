//! Row detail / form-view modal.
//!
//! Renders every column of the focused row as a `column_name (TYPE) → value`
//! list inside a centred overlay. Multi-line cell values get full
//! `Paragraph` wrap since the modal has the room — no glyph projection.

use narwhal_core::{ColumnHeader, Value};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::theme::Theme;
use crate::widgets::results::sanitize_for_display;

/// View model passed to [`render_row_detail`] each frame.
pub struct RowDetailView<'a> {
    pub columns: &'a [ColumnHeader],
    pub values: &'a [Value],
    pub selected_column: usize,
    pub scroll_offset: u16,
    /// The original row index in the result set (1-based for display).
    pub row_index: usize,
}

/// Render the row-detail modal on top of the result pane area.
///
/// Layout: centred `Rect`, max 80×30 or 70 % of the screen (whichever
/// smaller). A `Clear` widget masks the result pane underneath.
pub fn render_row_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    view: &RowDetailView<'_>,
    theme: &Theme,
) {
    let (max_w, max_h) = crate::constants::ROW_DETAIL_MODAL_MAX;
    let pct_w = (area.width as u32 * 70 / 100) as u16;
    let pct_h = (area.height as u32 * 70 / 100) as u16;
    let width = max_w.min(pct_w).min(area.width);
    let height = max_h.min(pct_h).min(area.height);

    let popup_area = centred_rect(area, width, height);
    frame.render_widget(Clear, popup_area);

    let title = format!(" row {} · esc closes ", view.row_index + 1);
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

    let mut lines: Vec<Line<'_>> = Vec::with_capacity(view.columns.len() * 3);

    for (i, col) in view.columns.iter().enumerate() {
        let is_selected = i == view.selected_column;
        let value = view.values.get(i);

        // Column header line: "column_name (TYPE)"
        let header_style = if is_selected {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.foreground)
        };
        let type_style = if is_selected {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        let highlight_bg = if is_selected {
            Style::default().bg(theme.muted)
        } else {
            Style::default()
        };

        let header_text = format!("  {} (", col.name);
        let type_text = col.data_type.clone();
        let close_paren = ")";

        lines.push(Line::from(vec![
            Span::styled(header_text, header_style.patch(highlight_bg)),
            Span::styled(type_text, type_style.patch(highlight_bg)),
            Span::styled(close_paren, header_style.patch(highlight_bg)),
        ]));

        // Value line
        let raw_value_text = match value {
            Some(Value::Null) => "<null>".to_owned(),
            Some(v) => v.render(),
            None => String::new(),
        };
        let value_text = sanitize_for_display(&raw_value_text);
        let value_style = match value {
            Some(Value::Null) => Style::default().fg(theme.muted).patch(highlight_bg),
            _ if is_selected => Style::default().fg(theme.foreground).patch(highlight_bg),
            _ => Style::default().fg(theme.foreground),
        };

        // Indent the value under its column header. Use a Paragraph for
        // the value so multi-line content wraps naturally.
        // We add each line of the wrapped value separately.
        let inner_width = inner.width.saturating_sub(4) as usize; // 4 for "    " indent
        if inner_width == 0 {
            lines.push(Line::from(Span::styled(
                format!("    {value_text}"),
                value_style,
            )));
        } else {
            let wrapped = wrap_text(&value_text, inner_width);
            for line in &wrapped {
                lines.push(Line::from(Span::styled(format!("    {line}"), value_style)));
            }
        }

        // Blank line between entries
        if i + 1 < view.columns.len() {
            lines.push(Line::from(""));
        }
    }

    let paragraph = Paragraph::new(lines)
        .scroll((view.scroll_offset, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

/// Simple word-aware text wrapping. Returns a list of lines, each no
/// longer than `max_width` characters (approximately). Falls back to
/// character-level wrapping for words that exceed `max_width`.
fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![text.to_owned()];
    }
    let mut result = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            result.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in line.split_whitespace() {
            if current.is_empty() {
                if word.len() <= max_width {
                    current = word.to_owned();
                } else {
                    // Word longer than max_width — break it up.
                    for chunk in word.as_bytes().chunks(max_width) {
                        let s = String::from_utf8_lossy(chunk).into_owned();
                        result.push(s);
                    }
                    current.clear();
                }
            } else if current.len() + 1 + word.len() <= max_width {
                current.push(' ');
                current.push_str(word);
            } else {
                result.push(std::mem::take(&mut current));
                if word.len() <= max_width {
                    current = word.to_owned();
                } else {
                    for chunk in word.as_bytes().chunks(max_width) {
                        let s = String::from_utf8_lossy(chunk).into_owned();
                        result.push(s);
                    }
                    current.clear();
                }
            }
        }
        if !current.is_empty() {
            result.push(current);
        }
    }
    if result.is_empty() {
        result.push(String::new());
    }
    result
}

fn centred_rect(area: Rect, width: u16, height: u16) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}
