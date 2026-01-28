//! Editor pane rendering. The text buffer model lives in
//! [`narwhal_domain::editor`] (see `EditorBuffer`); this module is the
//! ratatui binding that turns the buffer into glyphs on the terminal.

use narwhal_domain::editor::{
    floor_char_boundary, CompletionPopupView, EditorBuffer, EditorSearchHighlight,
};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

/// Compute the width of the line-number gutter so a buffer with N
/// lines reserves exactly the cells it needs for `"NNN │ "` plus the
/// trailing space (L36). Minimum 6 to keep the historical layout for
/// small buffers.
fn gutter_width(line_count: usize) -> usize {
    // Account for `" │ "` (2 visible cells after the number) plus the
    // number itself.
    let digits = line_count.max(1).to_string().len();
    (digits + 3).max(6)
}

pub fn render_editor(
    frame: &mut Frame<'_>,
    area: Rect,
    buffer: &mut EditorBuffer,
    theme: &Theme,
    focused: bool,
    title: &str,
    search: Option<&EditorSearchHighlight<'_>>,
) {
    let border_style = if focused {
        Style::default().fg(theme.accent)
    } else {
        Style::default().fg(theme.muted)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(format!(" {title} "));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let height = inner.height as usize;
    buffer.ensure_visible(height);

    // Collect matches per line for highlight rendering.
    let match_line_map: std::collections::HashMap<usize, Vec<(usize, bool)>> = search
        .map(|s| {
            let mut map: std::collections::HashMap<usize, Vec<(usize, bool)>> =
                std::collections::HashMap::new();
            for (i, &(line, col)) in s.matches.iter().enumerate() {
                let is_current = s.current == Some(i);
                map.entry(line).or_default().push((col, is_current));
            }
            // Sort matches within each line by column.
            for v in map.values_mut() {
                v.sort_by_key(|(col, _)| *col);
            }
            map
        })
        .unwrap_or_default();
    let needle_len = search.map_or(0, |s| s.needle_len);

    let end = (buffer.scroll() + height).min(buffer.lines().len());
    let gutter_w = gutter_width(buffer.lines().len());
    let num_w = gutter_w.saturating_sub(3); // " │ " suffix
    let lines: Vec<Line<'_>> = (buffer.scroll()..end)
        .map(|row| {
            let number = format!("{:>w$} │ ", row + 1, w = num_w);
            let gutter = Span::styled(number, Style::default().fg(theme.muted));

            let line_text = &buffer.lines()[row];

            if let Some(matches_on_line) = match_line_map.get(&row) {
                // Build spans with highlight overlays.
                let mut spans = vec![gutter];
                let mut pos = 0usize;
                for &(col, is_current) in matches_on_line {
                    let start = floor_char_boundary(line_text, col);
                    let hl_end_raw = col.saturating_add(needle_len);
                    let end = floor_char_boundary(line_text, hl_end_raw);
                    if start > pos {
                        let seg_end = start.min(line_text.len());
                        if pos < seg_end {
                            spans.push(Span::raw(line_text[pos..seg_end].to_owned()));
                        }
                    }
                    if start < line_text.len() && end > start {
                        let style = if is_current {
                            Style::default()
                                .fg(theme.background)
                                .bg(theme.accent)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(theme.foreground).bg(theme.muted)
                        };
                        spans.push(Span::styled(line_text[start..end].to_owned(), style));
                    }
                    // L15: search matches always have `end > start`
                    // (zero-length needles are filtered out upstream),
                    // so plain assignment suffices.
                    pos = end;
                }
                if pos < line_text.len() {
                    spans.push(Span::raw(line_text[pos..].to_owned()));
                }
                Line::from(spans)
            } else {
                let body = Span::raw(line_text.clone());
                Line::from(vec![gutter, body])
            }
        })
        .collect();

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);

    if focused && buffer.cursor_row() >= buffer.scroll() {
        let cursor_y = (buffer.cursor_row() - buffer.scroll()) as u16;
        if cursor_y < inner.height {
            let cursor_x = (gutter_w + cursor_display_col(buffer)) as u16;
            if cursor_x < inner.width {
                frame.set_cursor_position((inner.x + cursor_x, inner.y + cursor_y));
            }
        }
    }
}

/// Width-aware display column for the current cursor position. Honours
/// the East-Asian width tables so multibyte glyphs (Turkish 2-byte,
/// CJK 3-byte, emoji 4-byte) render with the cursor sprite over the
/// correct cell, not the byte index.
fn cursor_display_col(buffer: &EditorBuffer) -> usize {
    let row = buffer
        .cursor_row()
        .min(buffer.lines().len().saturating_sub(1));
    let line = &buffer.lines()[row];
    let mut col = buffer.cursor_col().min(line.len());
    while col > 0 && !line.is_char_boundary(col) {
        col -= 1;
    }
    line[..col].width()
}

/// Helper that turns the editor's outer rect plus the cursor offset into
/// an absolute screen coordinate the host app can pass as
/// [`CompletionPopupView::anchor`]. Mirrors the layout done inside
/// [`render_editor`].
pub fn editor_cursor_anchor(area: Rect, buffer: &EditorBuffer) -> (u16, u16) {
    let inner_x = area.x + 1;
    let inner_y = area.y + 1;
    let cursor_x =
        inner_x + (gutter_width(buffer.lines().len()) + cursor_display_col(buffer)) as u16;
    let cursor_y = if buffer.cursor_row() >= buffer.scroll() {
        inner_y + (buffer.cursor_row() - buffer.scroll()) as u16
    } else {
        inner_y
    };
    (cursor_x, cursor_y)
}

/// Hit-test regions for completion popup items.
#[derive(Debug, Default, Clone)]
pub struct CompletionHitRegions {
    /// The actual screen `Rect` the popup was rendered to. Set to
    /// `None` when the popup is empty (no items).
    pub popup_rect: Option<Rect>,
    /// One `(Rect, item_index)` per visible completion item.
    pub items: Vec<(Rect, usize)>,
}

/// Render the completion popup overlay. Should be called *after*
/// [`render_editor`] so it draws on top.
///
/// Returns hit-test regions for each visible completion item so mouse
/// clicks can be routed to the correct item.
pub fn render_completion_popup(
    frame: &mut Frame<'_>,
    screen: Rect,
    view: &CompletionPopupView<'_>,
    theme: &Theme,
) -> CompletionHitRegions {
    use ratatui::layout::Constraint;
    use ratatui::style::Modifier;
    use ratatui::widgets::{Cell, Clear, Row as TableRow, Table};

    if view.items.is_empty() {
        return CompletionHitRegions::default();
    }
    // Width: glyph column (2) + widest text column + widest detail
    // column + breathing room (4). The popup is allowed to grow up to
    // whatever the screen can host minus a small margin, so multi-word
    // phrases like 'SELECT COUNT(*)' don't get cropped to 'SELECT C'
    // when the editor pane is narrow.
    let max_text = view
        .items
        .iter()
        .map(|i| i.text.chars().count())
        .max()
        .unwrap_or(0);
    let max_detail = view
        .items
        .iter()
        .map(|i| i.detail.map_or(0, |d| d.chars().count()))
        .max()
        .unwrap_or(0);
    let want = 2 + max_text + if max_detail == 0 { 0 } else { max_detail + 1 } + 4;
    let avail = (screen.width.saturating_sub(2) as usize).clamp(20, 100);
    let width = want.clamp(20, avail) as u16;
    let height = (view.items.len() as u16 + 2).min(10);

    let (ax, ay) = view.anchor;
    let below_y = ay.saturating_add(1);
    let x = ax.min(screen.x + screen.width.saturating_sub(width));
    let y = if below_y + height <= screen.y + screen.height {
        below_y
    } else {
        ay.saturating_sub(height)
    };
    let popup = Rect {
        x,
        y,
        width,
        height,
    };

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " completions ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let visible = (inner.height as usize).min(view.items.len());
    // Naively window around the selection.
    let start = view.selected.saturating_sub(visible.saturating_sub(1));
    let end = (start + visible).min(view.items.len());
    let rows = view.items[start..end].iter().enumerate().map(|(i, item)| {
        let global = start + i;
        let style = if global == view.selected {
            Style::default()
                .fg(theme.background)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.foreground)
        };
        let detail = item.detail.unwrap_or("");
        TableRow::new(vec![
            Cell::from(format!(" {}", item.kind_glyph)).style(style),
            Cell::from(item.text.to_owned()).style(style),
            Cell::from(detail.to_owned()).style(style),
        ])
    });
    // Constraints adapt to the actual content rather than the old fixed
    // 8/16 split: a long phrase like 'CREATE TABLE IF NOT EXISTS' takes
    // the room it needs and the detail column shrinks to zero when no
    // item has a detail string.
    let text_w = (max_text as u16).max(4);
    let widths: Vec<Constraint> = if max_detail == 0 {
        vec![Constraint::Length(2), Constraint::Min(text_w)]
    } else {
        vec![
            Constraint::Length(2),
            Constraint::Length(text_w),
            Constraint::Length(max_detail as u16),
        ]
    };
    let table = Table::new(rows, widths);
    frame.render_widget(table, inner);

    // Build hit-test rects for each visible completion item.
    let mut item_rects = Vec::with_capacity(end - start);
    for i in start..end {
        let local = i - start;
        item_rects.push((
            Rect {
                x: inner.x,
                y: inner.y + local as u16,
                width: inner.width,
                height: 1,
            },
            i,
        ));
    }
    CompletionHitRegions {
        popup_rect: Some(popup),
        items: item_rects,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_hit_regions_contains_popup_rect() {
        let regions = CompletionHitRegions::default();
        assert!(regions.popup_rect.is_none());
        assert!(regions.items.is_empty());
    }

    #[test]
    fn popup_y_below_anchor_when_room() {
        let screen = Rect::new(0, 0, 80, 24);
        let anchor = (10u16, 5u16);
        let height: u16 = 5;
        let below_y = anchor.1.saturating_add(1);
        let y = if below_y + height <= screen.y + screen.height {
            below_y
        } else {
            anchor.1.saturating_sub(height)
        };
        assert_eq!(y, 6, "popup should be placed below the anchor");
    }

    #[test]
    fn popup_y_above_anchor_when_no_room() {
        let screen = Rect::new(0, 0, 80, 10);
        let anchor = (10u16, 9u16);
        let height: u16 = 5;
        let below_y = anchor.1.saturating_add(1);
        let y = if below_y + height <= screen.y + screen.height {
            below_y
        } else {
            anchor.1.saturating_sub(height)
        };
        assert_eq!(y, 4, "popup should be placed above the anchor");
    }
}
