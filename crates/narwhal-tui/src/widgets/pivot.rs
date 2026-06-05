//! T2-T4-D: pivot table widget.
//!
//! Pure projection of a [`PivotTableView`] into a ratatui [`Table`].
//! Owns no state; the host derives a fresh [`PivotTableView`] on every
//! frame and hands it in.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::theme::Theme;

/// Pure-data view passed into [`render_pivot`]. Mirrors
/// `narwhal_pivot::PivotTable` so the TUI crate doesn't depend on
/// the pivot crate directly; the host transposes between the two.
#[derive(Debug, Clone)]
pub struct PivotTableView<'a> {
    pub title: &'a str,
    /// Aggregator label, e.g. `"sum"`. Surfaces in the title bar.
    pub agg_label: &'a str,
    /// Headers for the row-dimension columns (left side).
    pub row_dim_headers: &'a [String],
    /// Column-dim values across the top, after the row-dim columns.
    /// Empty when the pivot has no column dimension.
    pub col_headers: &'a [String],
    /// Rendered rows; each row carries `row_dim_headers.len()` row-key
    /// cells followed by `col_headers.len().max(1)` value cells.
    pub rows: &'a [Vec<String>],
}

/// Placeholder rendered when pivot derivation fails (no rows, bad
/// config, etc.). Title + message are owned by the host so the widget
/// only borrows.
#[derive(Debug, Clone)]
pub struct PivotPlaceholder<'a> {
    pub title: &'a str,
    pub message: &'a str,
}

/// Render the pivot table inside `area`.
pub fn render_pivot(frame: &mut Frame<'_>, area: Rect, view: &PivotTableView<'_>, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.muted))
        .title(Span::styled(
            format!(" pivot · {} · {} ", view.title, view.agg_label),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 8 || inner.height < 2 {
        return;
    }

    let total_cols = view.row_dim_headers.len() + view.col_headers.len().max(1);
    let widths: Vec<Constraint> = (0..total_cols)
        .map(|_| Constraint::Length(column_width(view).min(inner.width / total_cols.max(1) as u16)))
        .collect();

    // Build the header row: row-dim names then col-dim values.
    let mut header_cells: Vec<Cell<'_>> = view
        .row_dim_headers
        .iter()
        .map(|h| Cell::from(h.as_str()).style(Style::default().add_modifier(Modifier::BOLD)))
        .collect();
    if view.col_headers.is_empty() {
        header_cells
            .push(Cell::from(view.agg_label).style(Style::default().add_modifier(Modifier::BOLD)));
    } else {
        for col in view.col_headers {
            header_cells.push(
                Cell::from(col.as_str()).style(Style::default().add_modifier(Modifier::BOLD)),
            );
        }
    }
    let header = Row::new(header_cells).style(Style::default().fg(theme.foreground));

    let body: Vec<Row<'_>> = view
        .rows
        .iter()
        .map(|row| {
            Row::new(
                row.iter()
                    .map(|cell| Cell::from(cell.as_str()))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();

    let table = Table::new(body, widths)
        .header(header)
        .column_spacing(1)
        .style(Style::default().fg(theme.foreground));
    frame.render_widget(table, inner);
}

/// Render the placeholder pane (no pivot data yet, or derivation error).
pub fn render_pivot_placeholder(
    frame: &mut Frame<'_>,
    area: Rect,
    placeholder: &PivotPlaceholder<'_>,
    theme: &Theme,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.muted))
        .title(Span::styled(
            format!(" pivot · {} ", placeholder.title),
            Style::default().fg(theme.muted),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let para = Paragraph::new(Line::from(Span::styled(
        format!("  {}", placeholder.message),
        Style::default().fg(theme.muted),
    )));
    frame.render_widget(para, inner);
}

/// Rough per-column width: the longest rendered string in any column,
/// capped at 24 cells. Bigger values force horizontal truncation.
fn column_width(view: &PivotTableView<'_>) -> u16 {
    let mut max = 4u16;
    for h in view.row_dim_headers {
        max = max.max(h.chars().count() as u16);
    }
    for h in view.col_headers {
        max = max.max(h.chars().count() as u16);
    }
    for row in view.rows {
        for cell in row {
            max = max.max(cell.chars().count() as u16);
        }
    }
    max.clamp(4, 24)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn pivot_renders_within_area() {
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).expect("backend");
        let theme = Theme::default();
        let row_dim = vec!["country".to_owned()];
        let col_headers = vec!["2024".to_owned(), "2025".to_owned()];
        let rows = vec![
            vec!["tr".to_owned(), "10".to_owned(), "20".to_owned()],
            vec!["de".to_owned(), "—".to_owned(), "30".to_owned()],
        ];
        let view = PivotTableView {
            title: "country × year",
            agg_label: "sum(revenue)",
            row_dim_headers: &row_dim,
            col_headers: &col_headers,
            rows: &rows,
        };
        terminal
            .draw(|frame| render_pivot(frame, frame.area(), &view, &theme))
            .expect("draw");
    }

    #[test]
    fn pivot_renders_without_col_dim() {
        let backend = TestBackend::new(40, 8);
        let mut terminal = Terminal::new(backend).expect("backend");
        let theme = Theme::default();
        let row_dim = vec!["country".to_owned()];
        let col_headers: Vec<String> = Vec::new();
        let rows = vec![
            vec!["tr".to_owned(), "2".to_owned()],
            vec!["de".to_owned(), "1".to_owned()],
        ];
        let view = PivotTableView {
            title: "count",
            agg_label: "count",
            row_dim_headers: &row_dim,
            col_headers: &col_headers,
            rows: &rows,
        };
        terminal
            .draw(|frame| render_pivot(frame, frame.area(), &view, &theme))
            .expect("draw");
    }

    #[test]
    fn placeholder_renders_without_panic() {
        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend).expect("backend");
        let theme = Theme::default();
        terminal
            .draw(|frame| {
                render_pivot_placeholder(
                    frame,
                    frame.area(),
                    &PivotPlaceholder {
                        title: "config",
                        message: "no data yet",
                    },
                    &theme,
                );
            })
            .expect("draw");
    }

    #[test]
    fn pivot_in_tiny_area_does_not_panic() {
        let backend = TestBackend::new(6, 2);
        let mut terminal = Terminal::new(backend).expect("backend");
        let theme = Theme::default();
        let row_dim = vec!["k".to_owned()];
        let col_headers: Vec<String> = Vec::new();
        let rows = vec![vec!["a".to_owned(), "1".to_owned()]];
        let view = PivotTableView {
            title: "tiny",
            agg_label: "count",
            row_dim_headers: &row_dim,
            col_headers: &col_headers,
            rows: &rows,
        };
        terminal
            .draw(|frame| render_pivot(frame, frame.area(), &view, &theme))
            .expect("draw");
    }
}
