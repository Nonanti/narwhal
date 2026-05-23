//! Result-view state types. Most carry both data and a sliver of
//! ratatui state (`TableState`); a future split moves the pure
//! data half into narwhal-domain. For now the whole bundle lives
//! together.

use narwhal_core::{ColumnHeader, Row, TableSchema};
use ratatui::layout::Rect;
use ratatui::widgets::TableState;

use super::sort::{compare_values, SortDir};

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
    pub const fn selected(&self) -> Option<usize> {
        self.state.selected()
    }

    /// Select the row at `index`, or pass `None` to clear the
    /// selection.
    pub fn select(&mut self, index: Option<usize>) {
        self.state.select(index);
    }

    /// Vertical scroll offset of the underlying ratatui table.
    pub const fn scroll_offset(&self) -> usize {
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
        let next = self.state.selected().map_or(0, |i| i + 1);
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

/// View model passed to `render_results` each frame.
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
/// Returned by `render_results` so the host app can route mouse events.
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
