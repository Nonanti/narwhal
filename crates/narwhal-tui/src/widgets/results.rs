//! Tabular result viewer.

use narwhal_core::QueryResult;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
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

pub fn render_results(
    frame: &mut Frame<'_>,
    area: Rect,
    result: Option<&QueryResult>,
    error: Option<&str>,
    view: &mut ResultView,
    theme: &Theme,
    focused: bool,
) {
    let border_style = if focused {
        Style::default().fg(theme.accent)
    } else {
        Style::default().fg(theme.muted)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(" results ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if let Some(err) = error {
        let p = Paragraph::new(Span::styled(
            format!("  error: {err}"),
            Style::default().fg(theme.error),
        ));
        frame.render_widget(p, inner);
        return;
    }

    let Some(result) = result else {
        let p = Paragraph::new(Span::styled(
            "  no results yet — press Ctrl-Enter to run",
            Style::default().fg(theme.muted),
        ));
        frame.render_widget(p, inner);
        return;
    };

    if result.columns.is_empty() {
        let affected = result.rows_affected.unwrap_or(0);
        let msg = format!(
            "  {} row{} affected · {} ms",
            affected,
            if affected == 1 { "" } else { "s" },
            result.elapsed_ms
        );
        let p = Paragraph::new(Span::styled(msg, Style::default().fg(theme.foreground)));
        frame.render_widget(p, inner);
        return;
    }

    let header_cells: Vec<Cell<'_>> = result
        .columns
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
    let header = Row::new(header_cells).bottom_margin(1);

    let widths: Vec<Constraint> = result
        .columns
        .iter()
        .map(|_| Constraint::Length(18))
        .collect();

    let rows: Vec<Row<'_>> = result
        .rows
        .iter()
        .map(|row| Row::new(row.0.iter().map(|v| Cell::from(v.render()))))
        .collect();

    let table = Table::new(rows, widths)
        .header(header)
        .highlight_style(Style::default().bg(theme.muted))
        .column_spacing(1);
    frame.render_stateful_widget(table, inner, &mut view.state);
}
