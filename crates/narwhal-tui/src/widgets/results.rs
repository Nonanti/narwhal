//! Tabular result viewer.

use narwhal_core::{ColumnHeader, Row};
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row as TableRow, Table, TableState};
use ratatui::Frame;

use crate::theme::Theme;

#[derive(Debug, Default)]
pub struct ResultView {
    pub state: TableState,
    pub column_offset: usize,
}

impl ResultView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn move_down(&mut self, total_rows: usize) {
        if total_rows == 0 {
            return;
        }
        let next = self.state.selected().map(|i| i + 1).unwrap_or(0);
        self.state.select(Some(next.min(total_rows - 1)));
    }

    pub fn move_up(&mut self) {
        if let Some(i) = self.state.selected() {
            self.state.select(Some(i.saturating_sub(1)));
        } else {
            self.state.select(Some(0));
        }
    }

    pub fn reset(&mut self) {
        self.state.select(None);
        self.column_offset = 0;
    }
}

/// View model passed to [`render_results`] each frame.
///
/// `Display::Empty` is shown before the first run, `Running` while a
/// statement is in flight (rows may already be filling in for streamed
/// queries), `Affected` for non-SELECT completions, `Rows` for completed
/// SELECT-like queries (streamed or materialised), and `Error` when the
/// engine returned a failure.
pub enum ResultDisplay<'a> {
    Empty,
    Running {
        sql: &'a str,
        index: usize,
        total: usize,
        columns: &'a [ColumnHeader],
        rows: &'a [Row],
    },
    Affected {
        rows: u64,
        elapsed_ms: u64,
        index: usize,
        total: usize,
    },
    Rows {
        columns: &'a [ColumnHeader],
        rows: &'a [Row],
        elapsed_ms: u64,
        streamed: bool,
        index: usize,
        total: usize,
    },
    Error {
        message: &'a str,
        elapsed_ms: u64,
    },
}

pub fn render_results(
    frame: &mut Frame<'_>,
    area: Rect,
    display: &ResultDisplay<'_>,
    view: &mut ResultView,
    theme: &Theme,
    focused: bool,
) {
    let border_style = if focused {
        Style::default().fg(theme.accent)
    } else {
        Style::default().fg(theme.muted)
    };
    let title = build_title(display);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    match display {
        ResultDisplay::Empty => {
            let p = Paragraph::new(Span::styled(
                "  no results yet — Ctrl-; to run, :run-all for whole buffer",
                Style::default().fg(theme.muted),
            ));
            frame.render_widget(p, inner);
        }
        ResultDisplay::Running {
            sql, columns, rows, ..
        } => {
            if columns.is_empty() {
                let p = Paragraph::new(vec![Line::from(Span::styled(
                    format!("  ⏳ running: {sql}"),
                    Style::default().fg(theme.muted),
                ))]);
                frame.render_widget(p, inner);
            } else {
                draw_table(frame, inner, columns, rows, theme, view);
            }
        }
        ResultDisplay::Affected {
            rows, elapsed_ms, ..
        } => {
            let msg = format!(
                "  {} row{} affected · {} ms",
                rows,
                if *rows == 1 { "" } else { "s" },
                elapsed_ms
            );
            let p = Paragraph::new(Span::styled(msg, Style::default().fg(theme.foreground)));
            frame.render_widget(p, inner);
        }
        ResultDisplay::Rows { columns, rows, .. } => {
            draw_table(frame, inner, columns, rows, theme, view);
        }
        ResultDisplay::Error {
            message,
            elapsed_ms,
        } => {
            let p = Paragraph::new(vec![Line::from(Span::styled(
                format!("  error ({elapsed_ms} ms): {message}"),
                Style::default().fg(theme.error),
            ))]);
            frame.render_widget(p, inner);
        }
    }
}

fn build_title(display: &ResultDisplay<'_>) -> String {
    match display {
        ResultDisplay::Empty => " results ".into(),
        ResultDisplay::Running {
            index, total, rows, ..
        } => format!(" results · running {index}/{total} · {} rows ", rows.len()),
        ResultDisplay::Affected {
            index,
            total,
            elapsed_ms,
            ..
        } => format!(" results · {index}/{total} · {elapsed_ms} ms "),
        ResultDisplay::Rows {
            index,
            total,
            rows,
            elapsed_ms,
            streamed,
            ..
        } => {
            let badge = if *streamed { "stream" } else { "exec" };
            format!(
                " results · {index}/{total} · {} rows · {elapsed_ms} ms · {badge} ",
                rows.len()
            )
        }
        ResultDisplay::Error { elapsed_ms, .. } => format!(" results · error · {elapsed_ms} ms "),
    }
}

fn draw_table(
    frame: &mut Frame<'_>,
    area: Rect,
    columns: &[ColumnHeader],
    rows: &[Row],
    theme: &Theme,
    view: &mut ResultView,
) {
    let header_cells: Vec<Cell<'_>> = columns
        .iter()
        .map(|c| {
            let label = format!("{} ({})", c.name, c.data_type);
            Cell::from(label).style(
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )
        })
        .collect();
    let header = TableRow::new(header_cells).bottom_margin(1);
    let widths: Vec<Constraint> = columns.iter().map(|_| Constraint::Length(18)).collect();
    let body_rows: Vec<TableRow<'_>> = rows
        .iter()
        .map(|row| TableRow::new(row.0.iter().map(|v| Cell::from(v.render()))))
        .collect();
    let table = Table::new(body_rows, widths)
        .header(header)
        .highlight_style(Style::default().bg(theme.muted))
        .column_spacing(1);
    frame.render_stateful_widget(table, area, &mut view.state);
}
