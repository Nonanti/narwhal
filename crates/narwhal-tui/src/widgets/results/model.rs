//! Result-display render argument types. The state-bearing
//! `ResultView` now lives in `narwhal_domain::result`; this module
//! only owns the lifetimed render-only enums (`ResultDisplay<'a>`,
//! `SearchHighlight<'a>`) and the post-render hit-region payload.

use narwhal_core::{ColumnHeader, Row, TableSchema};
use ratatui::layout::Rect;

// `mod.rs` re-exports these so the `narwhal_tui::MetaTab` / etc.
// import paths keep working; here we only need them in scope for
// the field types of `ResultDisplay`.
use narwhal_domain::result::{ExplainPlanLine, MetaTab};

/// Highlight information for [`ResultDisplay::Rows`] when search is active.
pub struct SearchHighlight<'a> {
    pub matches: &'a [usize],
    pub current: Option<usize>,
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
        /// Active metadata sub-view. The renderer paints a tab strip
        /// across the top and only the matching block beneath; `Records`
        /// is short-circuited by the host before reaching us (it swaps
        /// the entire `ResultState` to `Rows`).
        active_tab: MetaTab,
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
