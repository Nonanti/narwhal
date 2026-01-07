//! Tabular result viewer.

use narwhal_core::{ColumnHeader, ForeignKey, Index, Row, TableSchema, UniqueConstraint, Value};
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Paragraph, Row as TableRow, Table, TableState, Wrap,
};
use ratatui::Frame;
use std::cmp::Ordering;
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

/// Sort direction for a result column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SortDir {
    Asc,
    Desc,
}

/// Compare two optional [`Value`] references for sorting purposes.
///
/// Ordering rules:
/// - `None` (missing column) sorts last regardless of direction.
/// - `Null` sorts last in Asc, first in Desc.
/// - Same-type values compare naturally (Int numerically, String
///   lexicographically, etc.).
/// - Different types sort by a stable type-order: Int < Float < Bool <
///   String < Bytes < Date < Time < DateTime < Timestamp < Uuid < Json <
///   Unknown.
pub fn compare_values(a: Option<&Value>, b: Option<&Value>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, _) => Ordering::Greater,
        (_, None) => Ordering::Less,
        (Some(Value::Null), Some(Value::Null)) => Ordering::Equal,
        (Some(Value::Null), _) => Ordering::Greater,
        (_, Some(Value::Null)) => Ordering::Less,
        (Some(va), Some(vb)) => {
            let ta = type_rank(va);
            let tb = type_rank(vb);
            match ta.cmp(&tb) {
                Ordering::Equal => compare_same_type(va, vb),
                other => other,
            }
        }
    }
}

fn type_rank(v: &Value) -> u8 {
    match v {
        Value::Int(_) => 0,
        Value::Float(_) => 1,
        Value::Bool(_) => 2,
        Value::String(_) => 3,
        Value::Bytes(_) => 4,
        Value::Date(_) => 5,
        Value::Time(_) => 6,
        Value::DateTime(_) => 7,
        Value::Timestamp(_) => 8,
        Value::Uuid(_) => 9,
        Value::Json(_) => 10,
        Value::Unknown(_) => 11,
        Value::Null => 12, // unreachable in practice but included for completeness
        // Future variants get sorted after Null until ranked explicitly.
        _ => 13,
    }
}

fn compare_same_type(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        (Value::Bytes(x), Value::Bytes(y)) => x.cmp(y),
        (Value::Date(x), Value::Date(y)) => x.cmp(y),
        (Value::Time(x), Value::Time(y)) => x.cmp(y),
        (Value::DateTime(x), Value::DateTime(y)) => x.cmp(y),
        (Value::Timestamp(x), Value::Timestamp(y)) => x.cmp(y),
        (Value::Uuid(x), Value::Uuid(y)) => x.cmp(y),
        (Value::Json(x), Value::Json(y)) => x.to_string().cmp(&y.to_string()),
        (Value::Unknown(x), Value::Unknown(y)) => x.cmp(y),
        _ => Ordering::Equal,
    }
}

#[derive(Debug, Default)]
pub struct ResultView {
    /// Ratatui table state — `pub(crate)` so a future ratatui major
    /// upgrade doesn't ripple a `TableState` API change into every
    /// downstream caller. Use [`ResultView::selected`] /
    /// [`ResultView::select`] / [`ResultView::scroll_offset`]
    /// instead of touching it directly (M22).
    pub(crate) state: TableState,
    pub column_index: usize,
    pub popup: Option<CellPopup>,
    /// When `Some`, the cell editor is drawn on top of the result grid in
    /// place of the read-only popup. Only one of `popup` and `edit` is
    /// rendered at a time; the host app enforces this.
    pub edit: Option<CellEditView>,
    /// Active sort: `(column_index, direction)`.
    pub sort: Option<(usize, SortDir)>,
    /// Active filter text. Rows that don't contain this
    /// case-insensitive substring in any column are hidden.
    pub filter: String,
    /// When `true`, the filter input prompt is open for editing.
    pub filter_prompt_open: bool,
    /// Cached visible row indices computed by the last render.
    /// `visible_indices[i]` is the original row index of the i-th
    /// rendered row.
    pub visible_indices: Vec<usize>,
}

/// Editor-style popup used by inline cell edits.
#[derive(Debug, Clone)]
pub struct CellEditView {
    pub column_name: String,
    pub column_type: String,
    pub row_index: usize,
    /// Current buffer the user is editing.
    pub buffer: String,
    /// Optional error message rendered below the input (e.g. UPDATE
    /// rejected by the engine).
    pub error: Option<String>,
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

    /// Returns the index of the selected row, or `None` when no row is
    /// selected. Mirrors `ratatui::widgets::TableState::selected`.
    pub fn selected(&self) -> Option<usize> {
        self.state.selected()
    }

    /// Select the row at `index`, or pass `None` to clear the
    /// selection.
    pub fn select(&mut self, index: Option<usize>) {
        self.state.select(index);
    }

    /// Vertical scroll offset of the underlying ratatui table.
    pub fn scroll_offset(&self) -> usize {
        self.state.offset()
    }

    /// Set the vertical scroll offset of the underlying ratatui table.
    pub fn set_scroll_offset(&mut self, offset: usize) {
        *self.state.offset_mut() = offset;
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
        self.sort = None;
        self.filter.clear();
        self.filter_prompt_open = false;
        self.visible_indices.clear();
    }

    /// Derive the visible row indices after applying filter then sort.
    /// Filter applies first; sort applies to the filtered subset.
    /// Sort is stable across ties.
    pub fn visible_rows(&self, columns: &[ColumnHeader], rows: &[Row]) -> Vec<usize> {
        let mut indices: Vec<usize> = (0..rows.len()).collect();

        // Filter: keep rows where any cell contains the needle
        // (case-insensitive).
        if !self.filter.is_empty() {
            let needle = self.filter.to_lowercase();
            indices.retain(|&i| {
                rows[i]
                    .0
                    .iter()
                    .any(|v| v.render().to_lowercase().contains(&needle))
            });
        }

        // Sort: stable sort on the filtered subset.
        if let Some((col, dir)) = self.sort {
            let col_clamped = if col < columns.len() {
                col
            } else {
                return indices;
            };
            indices.sort_by(|&a, &b| {
                let av = rows[a].0.get(col_clamped);
                let bv = rows[b].0.get(col_clamped);
                let ord = compare_values(av, bv);
                match dir {
                    SortDir::Asc => ord,
                    SortDir::Desc => ord.reverse(),
                }
            });
        }

        indices
    }
}

/// View model passed to [`render_results`] each frame.
///
/// `Display::Empty` is shown before the first run, `Running` while a
/// statement is in flight (rows may already be filling in for streamed
/// queries), `Affected` for non-SELECT completions, `Rows` for completed
/// SELECT-like queries (streamed or materialised), and `Error` when the
/// engine returned a failure.
#[non_exhaustive]
pub enum ResultDisplay<'a> {
    Empty,
    Running {
        sql: &'a str,
        index: usize,
        total: usize,
        columns: &'a [ColumnHeader],
        rows: &'a [Row],
        streaming: bool,
        started_at: std::time::Instant,
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
    Cancelled {
        rows_so_far: usize,
        elapsed_ms: u64,
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

/// Hit-test regions computed during the last render of the results pane.
/// Returned by [`render_results`] so the host app can route mouse events.
#[derive(Debug, Default, Clone)]
pub struct ResultHitRegions {
    /// One `(Rect, column_index)` per rendered column header cell.
    pub headers: Vec<(Rect, usize)>,
    /// One `(Rect, row_index)` per rendered data row.
    pub rows: Vec<(Rect, usize)>,
    /// One `(Rect, result_index)` per rendered result tab in the strip.
    /// Empty when there is only one result.
    pub tabs: Vec<(Rect, usize)>,
}

#[allow(clippy::too_many_arguments)]
pub fn render_results(
    frame: &mut Frame<'_>,
    area: Rect,
    display: &ResultDisplay<'_>,
    view: &mut ResultView,
    theme: &Theme,
    focused: bool,
    result_count: usize,
    active_result: usize,
) -> ResultHitRegions {
    let border_style = if focused {
        Style::default().fg(theme.accent)
    } else {
        Style::default().fg(theme.muted)
    };
    let title = build_title(display, view);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Render the result tab strip when there are multiple results.
    let tab_rects = if result_count > 1 {
        let strip_area = Rect { height: 1, ..inner };
        let mut rects = Vec::with_capacity(result_count);
        let mut x = strip_area.x;
        for i in 0..result_count {
            let label = format!(" result {}/{} ", i + 1, result_count);
            let label_width = label.len() as u16;
            let is_active = i == active_result;
            let style = if is_active {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
            };
            let tab_rect = Rect {
                x,
                y: strip_area.y,
                width: label_width.min(strip_area.width.saturating_sub(x - strip_area.x)),
                height: 1,
            };
            let span = Span::styled(label, style);
            frame.render_widget(Paragraph::new(span), tab_rect);
            if tab_rect.width > 0 {
                rects.push((tab_rect, i));
            }
            x += label_width;
            if x >= strip_area.x + strip_area.width {
                break;
            }
        }
        rects
    } else {
        Vec::new()
    };

    let content_area = if result_count > 1 {
        Rect {
            y: inner.y + 1,
            height: inner.height.saturating_sub(1),
            ..inner
        }
    } else {
        inner
    };

    let mut regions = match display {
        ResultDisplay::Empty => {
            let p = Paragraph::new(Span::styled(
                "  no results yet — F5 / Alt-Enter runs cursor statement, F6 runs whole buffer, Ctrl-Space completes",
                Style::default().fg(theme.muted),
            ))
            .wrap(Wrap { trim: false });
            frame.render_widget(p, content_area);
            ResultHitRegions::default()
        }
        ResultDisplay::Running {
            sql, columns, rows, ..
        } => {
            if columns.is_empty() {
                let p = Paragraph::new(vec![Line::from(Span::styled(
                    format!("  ⏳ running: {sql}"),
                    Style::default().fg(theme.muted),
                ))]);
                frame.render_widget(p, content_area);
                ResultHitRegions::default()
            } else {
                draw_table(frame, content_area, columns, rows, None, theme, view)
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
            frame.render_widget(p, content_area);
            ResultHitRegions::default()
        }
        ResultDisplay::Rows {
            columns,
            rows,
            search,
            ..
        } => draw_table(frame, content_area, columns, rows, *search, theme, view),
        ResultDisplay::TableDetail { schema } => {
            draw_table_detail(frame, content_area, schema, theme);
            ResultHitRegions::default()
        }
        ResultDisplay::Explain {
            lines,
            planning_time_ms,
            execution_time_ms,
        } => {
            draw_explain(
                frame,
                content_area,
                lines,
                *planning_time_ms,
                *execution_time_ms,
                theme,
            );
            ResultHitRegions::default()
        }
        ResultDisplay::Cancelled {
            rows_so_far,
            elapsed_ms,
        } => {
            let msg = format!(
                "  cancelled at {} rows · {} ms",
                format_count(*rows_so_far),
                elapsed_ms
            );
            let p = Paragraph::new(Span::styled(msg, Style::default().fg(theme.muted)));
            frame.render_widget(p, inner);
            ResultHitRegions::default()
        }
        ResultDisplay::Error {
            message,
            elapsed_ms,
        } => {
            let p = Paragraph::new(vec![Line::from(Span::styled(
                format!("  error ({elapsed_ms} ms): {message}"),
                Style::default().fg(theme.error),
            ))]);
            frame.render_widget(p, content_area);
            ResultHitRegions::default()
        }
    };
    regions.tabs = tab_rects;
    regions
}

fn format_count(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn format_elapsed(d: std::time::Duration) -> String {
    let total = d.as_secs_f64();
    if total < 60.0 {
        format!("{total:.1}s")
    } else {
        let mins = (total / 60.0).floor() as u64;
        let secs = (total % 60.0).floor() as u64;
        format!("{mins:02}:{secs:02}")
    }
}

fn build_title(display: &ResultDisplay<'_>, view: &ResultView) -> String {
    let base = match display {
        ResultDisplay::Empty => " results ".into(),
        ResultDisplay::Running {
            index: _,
            total: _,
            rows,
            streaming: true,
            started_at,
            ..
        } => {
            let count = format_count(rows.len());
            let elapsed = format_elapsed(started_at.elapsed());
            format!(" streaming · {count} rows · {elapsed} ")
        }
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
            if *streamed {
                let count = format_count(rows.len());
                format!(" results · {count} rows · {elapsed_ms}ms ")
            } else {
                let badge = "exec";
                format!(
                    " results · {index}/{total} · {} rows · {elapsed_ms} ms · {badge} ",
                    rows.len()
                )
            }
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
        ResultDisplay::Cancelled {
            rows_so_far,
            elapsed_ms,
        } => {
            let count = format_count(*rows_so_far);
            format!(" cancelled at {count} rows · {elapsed_ms}ms ")
        }
        ResultDisplay::Error { elapsed_ms, .. } => format!(" results · error · {elapsed_ms} ms "),
    };
    // Append filter tag when a filter is active but the prompt is closed.
    if !view.filter.is_empty() && !view.filter_prompt_open {
        format!("{base}[filter: {}] ", view.filter)
    } else {
        base
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

use crate::constants::{RESULT_MAX_COLUMN_WIDTH as MAX_COLUMN_WIDTH, RESULT_MIN_COLUMN_WIDTH as MIN_COLUMN_WIDTH};

fn compute_column_widths(columns: &[ColumnHeader], rows: &[Row]) -> Vec<usize> {
    columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let header_len = format!("{} ({})", c.name, c.data_type).width();
            let body_len = rows
                .iter()
                .map(|r| {
                    r.0.get(i)
                        .map(|v| render_for_grid(&v.render()).width())
                        .unwrap_or(0)
                })
                .max()
                .unwrap_or(0);
            header_len
                .max(body_len)
                .clamp(MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH)
        })
        .collect()
}

/// Single-line projection used in the result grid. Cell popup still shows
/// the raw value through a `Paragraph` widget so the user can read the
/// real text on demand — this just keeps grid rows one row tall.
///
/// Also sanitises dangerous Unicode glyphs (BIDI overrides, zero-width
/// characters, control chars) that could be used for visual spoofing
/// (Trojan Source attacks). Such characters are replaced with `·`.
fn render_for_grid(s: &str) -> String {
    let mut needs_sanitize = false;
    let mut needs_newline_replace = false;
    for ch in s.chars() {
        if is_dangerous_glyph(ch) {
            needs_sanitize = true;
            break;
        }
        if matches!(ch, '\n' | '\r' | '\t') {
            needs_newline_replace = true;
        }
    }
    if !needs_sanitize && !needs_newline_replace {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if is_dangerous_glyph(c) {
            out.push('·');
        } else {
            match c {
                '\r' => {
                    // Collapse CRLF into one glyph.
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    out.push('⏎');
                }
                '\n' => out.push('⏎'),
                '\t' => out.push('→'),
                other => out.push(other),
            }
        }
    }
    out
}

/// Returns true for Unicode characters that are dangerous to render
/// in a terminal grid: BIDI override controls, zero-width / directional
/// marks, and C0/C1 control characters (except \t, \n, \r which are
/// handled separately by `render_for_grid`).
fn is_dangerous_glyph(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}'  // BIDI override
        | '\u{2066}'..='\u{2069}' // BIDI isolate
        | '\u{200B}'..='\u{200F}' // zero-width, LRM/RLM
        | '\u{0000}'..='\u{0008}' // C0 controls (except TAB at 0x09)
        | '\u{000B}'..='\u{000C}' // VT, FF
        | '\u{000E}'..='\u{001F}' // SO..US, C1 range start
        | '\u{007F}'               // DEL
    )
}

/// Sanitise a string for display in any TUI context (cell popup,
/// row detail, history, sidebar, status). Replaces BIDI override
/// characters, zero-width / directional marks, and C0/C1 control
/// characters with `·`. Unlike `render_for_grid`, this does **not**
/// replace newlines / tabs — callers that need single-line projection
/// should use `render_for_grid` instead.
pub fn sanitize_for_display(s: &str) -> std::borrow::Cow<'_, str> {
    let needs = s.chars().any(is_dangerous_glyph);
    if !needs {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if is_dangerous_glyph(ch) {
            out.push('·');
        } else {
            out.push(ch);
        }
    }
    std::borrow::Cow::Owned(out)
}

fn draw_cell_edit(frame: &mut Frame<'_>, area: Rect, edit: &CellEditView, theme: &Theme) {
    let width = area.width.saturating_sub(8).min(crate::constants::CELL_POPUP_MAX_WIDTH);
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

fn draw_cell_popup(frame: &mut Frame<'_>, area: Rect, popup: &CellPopup, theme: &Theme) {
    let width = area.width.saturating_sub(8).min(crate::constants::CELL_POPUP_MAX_WIDTH);
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
    frame.render_stateful_widget(table, table_area, &mut view.state);

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

    let scroll_offset = view.state.offset();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn format_count_small() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(9999), "9999");
    }

    #[test]
    fn format_count_k_suffix() {
        assert_eq!(format_count(10_000), "10.0k");
        assert_eq!(format_count(12_345), "12.3k");
        assert_eq!(format_count(999_999), "1000.0k");
    }

    #[test]
    fn format_count_m_suffix() {
        assert_eq!(format_count(1_000_000), "1.0M");
        assert_eq!(format_count(1_234_567), "1.2M");
        assert_eq!(format_count(12_345_678), "12.3M");
    }

    #[test]
    fn format_elapsed_under_60s() {
        assert_eq!(format_elapsed(Duration::from_millis(0)), "0.0s");
        assert_eq!(format_elapsed(Duration::from_millis(100)), "0.1s");
        assert_eq!(format_elapsed(Duration::from_millis(2100)), "2.1s");
        assert_eq!(format_elapsed(Duration::from_millis(59999)), "60.0s");
    }

    #[test]
    fn format_elapsed_over_60s() {
        assert_eq!(format_elapsed(Duration::from_secs(60)), "01:00");
        assert_eq!(format_elapsed(Duration::from_secs(125)), "02:05");
        assert_eq!(format_elapsed(Duration::from_secs(3661)), "61:01");
    }

    #[test]
    fn sanitize_replaces_bidi_override() {
        // U+202E RIGHT-TO-LEFT OVERRIDE — Trojan Source attack vector.
        let input = "SELECT \u{202E}1;";
        let output = render_for_grid(input);
        assert_eq!(output, "SELECT ·1;");
    }

    #[test]
    fn sanitize_replaces_control_chars() {
        // NULL, BEL, BS, VT, FF, DEL
        let input = "a\u{0000}b\u{0007}c\u{0008}d\u{000B}e\u{000C}f\u{007F}g";
        let output = render_for_grid(input);
        assert_eq!(output, "a·b·c·d·e·f·g");
    }

    #[test]
    fn sanitize_preserves_normal_unicode() {
        let input = "Türkçe 中文 🦄";
        let output = render_for_grid(input);
        assert_eq!(output, input);
    }

    #[test]
    fn sanitize_handles_newline_and_bidi_combined() {
        let input = "a\n\u{202E}b";
        let output = render_for_grid(input);
        assert_eq!(output, "a⏎·b");
    }
}
