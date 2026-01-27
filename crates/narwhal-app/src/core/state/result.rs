//! Result-pane state: query outcome, supporting popups, the
//! multi-statement bundle.

use std::time::Instant;

use narwhal_core::{Column, ColumnHeader, Row, TableSchema};
use narwhal_tui::{ExplainPlanLine, ResultView};
use narwhal_vim::SearchDirection;
use uuid::Uuid;

use crate::completion::Completion;

/// What the result pane is currently showing.
#[derive(Debug, Default)]
pub enum ResultState {
    #[default]
    Empty,
    Running {
        sql: String,
        index: usize,
        total: usize,
        columns: Vec<ColumnHeader>,
        rows: Vec<Row>,
        streaming: bool,
        /// Moment the stream task was spawned. Used to compute elapsed
        /// time in the streaming title bar.
        started_at: Instant,
        /// Instant of the last redraw triggered by a chunk. Throttles
        /// renders to ≤10 Hz so a fast-arriving stream does not drown
        /// the redraw loop.
        last_render: Instant,
    },
    Affected {
        rows: u64,
        elapsed_ms: u64,
        index: usize,
        total: usize,
    },
    Rows {
        columns: Vec<ColumnHeader>,
        rows: Vec<Row>,
        elapsed_ms: u64,
        streamed: bool,
        index: usize,
        total: usize,
        /// Origin metadata for cell editing. `None` for ad-hoc statements;
        /// `Some` when the rows came from a sidebar table preview, so we
        /// know the schema/table/PK columns to build an UPDATE.
        source: Option<RowSource>,
        /// Best-effort table name extracted from the SQL (single-table
        /// `SELECT * FROM x`). Used by `:export insert` to generate
        /// INSERT statements. `None` for multi-table queries, computed
        /// expressions, etc.
        source_table: Option<crate::export::QualifiedName>,
    },
    Explain {
        lines: Vec<ExplainPlanLine>,
        planning_time_ms: Option<f64>,
        execution_time_ms: Option<f64>,
    },
    TableDetail {
        schema: TableSchema,
    },
    /// Stream was cancelled by the user (F4 / Ctrl-C). Shows
    /// how many rows were received before cancellation.
    Cancelled {
        rows_so_far: usize,
        elapsed_ms: u64,
    },
    Error {
        message: String,
        elapsed_ms: u64,
    },
}

/// Where a [`ResultState::Rows`] set originated. Populated only when the
/// rows came from a preview (sidebar `o` or analogous flow) so that the
/// edit path knows the table and primary key columns to target.
#[derive(Debug, Clone)]
pub struct RowSource {
    pub schema: String,
    pub table: String,
    pub columns: Vec<Column>,
    /// Offset of the first row in this page, relative to the unbounded
    /// `SELECT * FROM <table>`. Used by `:next` / `:prev`.
    pub offset: usize,
    /// Page size that produced this page. `:page-size` updates this for
    /// subsequent previews.
    pub limit: usize,
}

/// In-flight completion popup.
#[derive(Debug, Clone)]
pub struct CompletionState {
    /// Candidate list, already filtered and sorted.
    pub items: Vec<Completion>,
    /// Currently highlighted index.
    pub selected: usize,
    /// The prefix that produced [`Self::items`]. Used to detect when
    /// the user keeps typing and the popup needs to refilter.
    pub prefix: String,
}

/// In-flight cell edit. `buffer` is what the user is currently typing;
/// `original` is the cell's textual representation when the edit opened
/// (used for the cancel path and as the default in the popup).
#[derive(Debug, Clone)]
pub struct CellEdit {
    pub column_name: String,
    pub column_type: String,
    pub row_index: usize,
    pub column_index: usize,
    pub original: String,
    pub buffer: String,
}

/// Search state attached to a result pane.
#[derive(Debug, Default)]
pub struct ResultSearch {
    pub query: String,
    pub matches: Vec<usize>,
    pub current: Option<usize>,
    /// `true` while the user is typing the pattern; `false` after Enter.
    pub editing: bool,
}

/// Editor search state, separate from the result pane search.
/// Per-tab so each editor pane carries its own needle and highlight state.
#[derive(Debug, Clone, Default)]
pub struct EditorSearchState {
    /// The literal substring needle.
    pub needle: String,
    /// Direction of the search that opened the prompt.
    pub direction: SearchDirection,
    /// Whether the search prompt is currently open for editing.
    pub prompt_open: bool,
    /// Cursor position saved when `/` or `?` was pressed, restored on Esc.
    pub saved_cursor: Option<(usize, usize)>,
    /// All match positions as `(line_idx, byte_col)` pairs.
    pub matches: Vec<(usize, usize)>,
    /// Index into `matches` for the current match (where the cursor sits).
    pub current: Option<usize>,
    /// Whether matches are highlighted in the editor.
    pub highlight: bool,
}

/// In-flight row detail modal. `R` (or Shift+Enter) opens it from the
/// result pane; Esc / `R` / Shift+Enter dismisses it.
#[derive(Debug, Clone)]
pub struct RowDetailState {
    /// Original row index in the full result set.
    pub row_index: usize,
    pub columns: Vec<ColumnHeader>,
    pub values: Vec<narwhal_core::Value>,
    pub selected_column: usize,
    pub scroll_offset: u16,
}

/// Bundle of per-statement results produced by a multi-statement batch.
///
/// When the dispatch pipeline produces N result sets the user can cycle
/// through them with `]r` / `[r` (or Ctrl-PgDown / Ctrl-PgUp); the
/// active tab's state — scroll, sort, filter — is preserved across
/// switches.
///
/// The common case (single result) has `states.len() == 1` and the
/// strip is not rendered; behaviour is byte-for-byte identical to the
/// pre-bundle world.
///
/// `states` and `views` are kept in parallel arrays so the render path
/// can borrow from `states` immutably while mutating `views` — they
/// live in separate allocations, satisfying the borrow checker.
#[derive(Debug)]
pub struct ResultBundle {
    /// One `ResultState` per statement in the batch.
    pub states: Vec<ResultState>,
    /// One `ResultView` per statement (scroll, sort, filter, etc.).
    pub views: Vec<ResultView>,
    /// Index of the currently-visible result.
    pub active: usize,
}

impl ResultBundle {
    /// Construct a single-result bundle. No tab strip renders.
    pub fn single(state: ResultState, view: ResultView) -> Self {
        Self {
            states: vec![state],
            views: vec![view],
            active: 0,
        }
    }

    /// Construct a multi-result bundle from parallel vectors.
    /// `active` starts at 0.
    pub fn multi(states: Vec<ResultState>, views: Vec<ResultView>) -> Self {
        assert!(
            !states.is_empty(),
            "ResultBundle must contain at least one entry"
        );
        assert_eq!(
            states.len(),
            views.len(),
            "states and views must have the same length"
        );
        Self {
            states,
            views,
            active: 0,
        }
    }

    /// Whether the bundle contains more than one result (and thus a
    /// tab strip should be rendered).
    pub fn is_multi(&self) -> bool {
        self.states.len() > 1
    }

    /// Total number of results in the bundle.
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// Whether the bundle has no results. (Always false in practice
    /// since we guarantee at least one entry, but required by clippy.)
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Read-only access to the active result view.
    pub fn active(&self) -> &ResultView {
        &self.views[self.active]
    }

    /// Mutable access to the active result view.
    pub fn active_mut(&mut self) -> &mut ResultView {
        &mut self.views[self.active]
    }

    /// Read-only access to the active result state.
    pub fn active_state(&self) -> &ResultState {
        &self.states[self.active]
    }

    /// Mutable access to the active result state.
    pub fn active_state_mut(&mut self) -> &mut ResultState {
        &mut self.states[self.active]
    }

    /// Advance to the next result (wraps).
    pub fn next(&mut self) {
        if self.states.len() > 1 {
            self.active = (self.active + 1) % self.states.len();
        }
    }

    /// Go to the previous result (wraps).
    pub fn prev(&mut self) {
        if self.states.len() > 1 {
            self.active = self.active.checked_sub(1).unwrap_or(self.states.len() - 1);
        }
    }

    /// Reset every `ResultView` in the bundle.
    pub fn reset_all(&mut self) {
        for view in &mut self.views {
            view.reset();
        }
    }
}

impl Default for ResultBundle {
    fn default() -> Self {
        Self::single(ResultState::Empty, ResultView::new())
    }
}

