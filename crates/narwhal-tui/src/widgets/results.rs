//! Tabular result viewer.

use narwhal_core::{ColumnHeader, ForeignKey, Index, Row, TableSchema, UniqueConstraint};
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Paragraph, Row as TableRow, Table, TableState, Wrap,
};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

#[derive(Debug, Default)]
pub struct ResultView {
    pub state: TableState,
    pub column_index: usize,
    pub popup: Option<CellPopup>,
}

/// Highlight information for [`ResultDisplay::Rows`] when search is active.
pub struct SearchHighlight<'a> {
    pub matches: &'a [usize],
    pub current: Option<usize>,
}

/// Modal description of one cell, shown over the result grid when the user
/// requests detail with Enter.
#[derive(Debug, Clone)]
pub struct CellPopup {
    pub column_name: String,
    pub column_type: String,
    pub value_text: String,
    pub row_index: usize,
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

    pub fn move_left(&mut self) {
        self.column_index = self.column_index.saturating_sub(1);
    }

    pub fn move_right(&mut self, total_cols: usize) {
        if total_cols == 0 {
            return;
        }
        if self.column_index + 1 < total_cols {
            self.column_index += 1;
        }
    }

    pub fn reset(&mut self) {
        self.state.select(None);
        self.column_index = 0;
        self.popup = None;
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
        search: Option<&'a SearchHighlight<'a>>,
    },
    Explain {
        lines: &'a [ExplainPlanLine],
        planning_time_ms: Option<f64>,
        execution_time_ms: Option<f64>,
    },
    TableDetail {
        schema: &'a TableSchema,
    },
    Error {
        message: &'a str,
        elapsed_ms: u64,
    },
}

/// One rendered line of a query plan. Independent of the parser so the
/// widget crate does not need a dependency on `serde_json`.
#[derive(Debug, Clone)]
pub struct ExplainPlanLine {
    pub depth: usize,
    pub text: String,
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
                draw_table(frame, inner, columns, rows, None, theme, view);
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
        ResultDisplay::Rows {
            columns,
            rows,
            search,
            ..
        } => {
            draw_table(frame, inner, columns, rows, *search, theme, view);
        }
        ResultDisplay::TableDetail { schema } => {
            draw_table_detail(frame, inner, schema, theme);
        }
        ResultDisplay::Explain {
            lines,
            planning_time_ms,
            execution_time_ms,
        } => {
            draw_explain(
                frame,
                inner,
                lines,
                *planning_time_ms,
                *execution_time_ms,
                theme,
            );
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
        ResultDisplay::Explain {
            execution_time_ms, ..
        } => match execution_time_ms {
            Some(ms) => format!(" results · explain · {ms:.3} ms "),
            None => " results · explain ".to_owned(),
        },
        ResultDisplay::TableDetail { schema } => {
            let qualifier = if schema.table.schema.is_empty() {
                String::new()
            } else {
                format!("{}.", schema.table.schema)
            };
            format!(
                " {}{} · {} cols · {} idx · {} fk ",
                qualifier,
                schema.table.name,
                schema.columns.len(),
                schema.indexes.len(),
                schema.foreign_keys.len()
            )
        }
        ResultDisplay::Error { elapsed_ms, .. } => format!(" results · error · {elapsed_ms} ms "),
    }
}

fn draw_table_detail(frame: &mut Frame<'_>, area: Rect, schema: &TableSchema, theme: &Theme) {
    let mut lines: Vec<Line<'_>> = Vec::with_capacity(
        schema.columns.len() + schema.indexes.len() + schema.foreign_keys.len() + 8,
    );
    let bold_accent = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);

    lines.push(Line::from(Span::styled("  columns", bold_accent)));
    for col in &schema.columns {
        let mut tags = Vec::new();
        if col.primary_key {
            tags.push("PK");
        }
        if !col.nullable {
            tags.push("NOT NULL");
        }
        let tag_str = if tags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", tags.join(" "))
        };
        let default_str = match &col.default {
            Some(d) => format!("  default {d}"),
            None => String::new(),
        };
        lines.push(Line::from(format!(
            "    {:<24} {}{tag_str}{default_str}",
            col.name, col.data_type
        )));
    }

    if !schema.indexes.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("  indexes", bold_accent)));
        for idx in &schema.indexes {
            lines.push(Line::from(format_index_line(idx)));
        }
    }

    if !schema.foreign_keys.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("  foreign keys", bold_accent)));
        for fk in &schema.foreign_keys {
            lines.push(Line::from(format_foreign_key_line(fk)));
        }
    }

    if !schema.unique_constraints.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  unique constraints",
            bold_accent,
        )));
        for uq in &schema.unique_constraints {
            lines.push(Line::from(format_unique_line(uq)));
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn format_index_line(idx: &Index) -> String {
    let kind = if idx.primary {
        "PRIMARY"
    } else if idx.unique {
        "UNIQUE"
    } else {
        "INDEX"
    };
    format!("    {kind:<8} {}({})", idx.name, idx.columns.join(", "))
}

fn format_foreign_key_line(fk: &ForeignKey) -> String {
    let qualifier = match &fk.referenced_schema {
        Some(s) if !s.is_empty() => format!("{s}."),
        _ => String::new(),
    };
    let actions = {
        let mut bits = Vec::new();
        if let Some(a) = fk.on_update {
            bits.push(format!("ON UPDATE {}", a.as_sql()));
        }
        if let Some(a) = fk.on_delete {
            bits.push(format!("ON DELETE {}", a.as_sql()));
        }
        if bits.is_empty() {
            String::new()
        } else {
            format!("  {}", bits.join(" "))
        }
    };
    format!(
        "    {} ({}) -> {}{}({}){actions}",
        fk.name,
        fk.columns.join(", "),
        qualifier,
        fk.referenced_table,
        fk.referenced_columns.join(", ")
    )
}

fn format_unique_line(uq: &UniqueConstraint) -> String {
    format!("    {} ({})", uq.name, uq.columns.join(", "))
}

const MIN_COLUMN_WIDTH: usize = 6;
const MAX_COLUMN_WIDTH: usize = 40;

fn compute_column_widths(columns: &[ColumnHeader], rows: &[Row]) -> Vec<usize> {
    columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let header_len = format!("{} ({})", c.name, c.data_type).width();
            let body_len = rows
                .iter()
                .map(|r| r.0.get(i).map(|v| v.render().width()).unwrap_or(0))
                .max()
                .unwrap_or(0);
            header_len
                .max(body_len)
                .clamp(MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH)
        })
        .collect()
}

fn draw_cell_popup(frame: &mut Frame<'_>, area: Rect, popup: &CellPopup, theme: &Theme) {
    let width = area.width.saturating_sub(8).min(80);
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
    let paragraph = Paragraph::new(popup.value_text.as_str())
        .style(Style::default().fg(theme.foreground))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
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

fn draw_explain(
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

fn draw_table(
    frame: &mut Frame<'_>,
    area: Rect,
    columns: &[ColumnHeader],
    rows: &[Row],
    search: Option<&SearchHighlight<'_>>,
    theme: &Theme,
    view: &mut ResultView,
) {
    let widths = compute_column_widths(columns, rows);
    let header_cells: Vec<Cell<'_>> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let label = format!("{} ({})", c.name, c.data_type);
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
    let body_rows: Vec<TableRow<'_>> = rows
        .iter()
        .enumerate()
        .map(|(idx, row)| {
            let cells = row.0.iter().map(|v| Cell::from(v.render()));
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
    frame.render_stateful_widget(table, area, &mut view.state);

    if let Some(popup) = view.popup.as_ref() {
        draw_cell_popup(frame, area, popup, theme);
    }
}
