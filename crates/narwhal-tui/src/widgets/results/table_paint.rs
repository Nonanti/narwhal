//! The main result table painter.

use narwhal_core::{ColumnHeader, Row};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Row as TableRow, Table, TableState};

use ratatui::widgets::Paragraph;

use super::cells::{compute_column_widths, render_for_grid};
use super::model::{ResultHitRegions, SearchHighlight};
use super::popups::{draw_cell_edit, draw_cell_popup};
use crate::theme::Theme;
use narwhal_domain::result::{ResultView, SortDir};

pub(super) fn draw_table(
    frame: &mut Frame<'_>,
    area: Rect,
    columns: &[ColumnHeader],
    rows: &[Row],
    search: Option<&SearchHighlight<'_>>,
    theme: &Theme,
    view: &mut ResultView,
) -> ResultHitRegions {
    // Reserve one row at the bottom for the filter prompt when open.
    let filter_prompt_open = view.filter_prompt_open;
    let table_area = if filter_prompt_open {
        Rect {
            height: area.height.saturating_sub(1),
            ..area
        }
    } else {
        area
    };

    // Derive visible row indices (filter then sort).
    let visible = view.visible_rows(columns, rows);
    // Cache for the host app to look up original row indices.
    view.visible_indices = visible.clone();

    let widths = compute_column_widths(columns, rows);
    let header_cells: Vec<Cell<'_>> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            // Sort indicator: ▲ for Asc, ▼ for Desc.
            let suffix = match view.sort {
                Some((col, SortDir::Asc)) if col == i => " \u{25b2}",
                Some((col, SortDir::Desc)) if col == i => " \u{25bc}",
                _ => "",
            };
            let label = format!("{} ({}){suffix}", c.name, c.data_type);
            let mut style = Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD);
            if i == view.column_index {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            Cell::from(label).style(style)
        })
        .collect();
    let header = TableRow::new(header_cells).bottom_margin(1);
    let constraints: Vec<Constraint> = widths
        .iter()
        .map(|w| Constraint::Length(*w as u16))
        .collect();
    let body_rows: Vec<TableRow<'_>> = visible
        .iter()
        .map(|&idx| {
            let row = &rows[idx];
            let cells = row
                .0
                .iter()
                .map(|v| Cell::from(render_for_grid(&v.render())));
            let mut r = TableRow::new(cells);
            if let Some(search) = search {
                if search.matches.contains(&idx) {
                    let is_current =
                        search.current.and_then(|c| search.matches.get(c)) == Some(&idx);
                    let bg = if is_current {
                        theme.accent
                    } else {
                        theme.muted
                    };
                    r = r.style(Style::default().bg(bg));
                }
            }
            r
        })
        .collect();
    let table = Table::new(body_rows, constraints)
        .header(header)
        .highlight_style(Style::default().bg(theme.muted))
        .column_spacing(1);

    // Materialise a ratatui `TableState` from the pure domain fields,
    // hand it to the renderer (which may scroll it), then copy the
    // possibly-updated values back into the view. `TableState`
    // doesn't carry any other persistent state we care about.
    let mut table_state = TableState::default();
    table_state.select(view.selected);
    *table_state.offset_mut() = view.scroll_offset;
    frame.render_stateful_widget(table, table_area, &mut table_state);
    view.selected = table_state.selected();
    view.scroll_offset = table_state.offset();

    // Render the filter prompt row at the bottom of the result area.
    if filter_prompt_open {
        let prompt_y = area.y + area.height.saturating_sub(1);
        let prompt_line = if view.filter.is_empty() {
            Line::from(vec![
                Span::styled(" / ", Style::default().fg(theme.accent)),
                Span::styled("\u{2588}", Style::default().fg(theme.foreground)),
            ])
        } else {
            Line::from(vec![
                Span::styled(" / ", Style::default().fg(theme.accent)),
                Span::raw(&view.filter),
                Span::styled("\u{2588}", Style::default().fg(theme.foreground)),
            ])
        };
        frame.render_widget(
            Paragraph::new(prompt_line),
            Rect {
                x: area.x,
                y: prompt_y,
                width: area.width,
                height: 1,
            },
        );
    }

    // Compute hit-test regions for the header row and visible data rows.
    let header_height = 2u16; // header line + bottom_margin
    let mut header_rects = Vec::with_capacity(columns.len());
    let mut x_offset = table_area.x;
    for (i, w) in widths.iter().enumerate() {
        let w16 = *w as u16;
        header_rects.push((
            Rect {
                x: x_offset,
                y: table_area.y,
                width: w16,
                height: header_height,
            },
            i,
        ));
        x_offset += w16 + 1; // +1 for column_spacing
    }

    let scroll_offset = view.scroll_offset;
    let data_y_start = table_area.y + header_height;
    let visible_height = table_area.height.saturating_sub(header_height) as usize;
    let mut row_rects = Vec::new();
    for visible_idx in 0..visible_height {
        let absolute_idx = scroll_offset + visible_idx;
        if absolute_idx >= visible.len() {
            break;
        }
        let data_idx = visible[absolute_idx];
        row_rects.push((
            Rect {
                x: table_area.x,
                y: data_y_start + visible_idx as u16,
                width: table_area.width,
                height: 1,
            },
            data_idx,
        ));
    }

    if let Some(popup) = view.popup.as_ref() {
        draw_cell_popup(frame, table_area, popup, theme);
    }
    if let Some(edit) = view.edit.as_ref() {
        draw_cell_edit(frame, table_area, edit, theme);
    }

    ResultHitRegions {
        headers: header_rects,
        rows: row_rects,
        tabs: Vec::new(),
    }
}
