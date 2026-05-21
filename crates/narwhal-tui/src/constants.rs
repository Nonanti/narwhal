//! UI layout / sizing constants shared across the widget tree.
//!
//! Centralised so changing the sidebar width or any modal max-size is
//! a one-line edit instead of a multi-file grep (M23). Each constant
//! documents the widget(s) that previously inlined the magic number.

use std::time::Duration;

/// Width (in cells) of the sidebar pane in the root layout.
pub const SIDEBAR_WIDTH: u16 = 34;

/// Percentage split between the editor pane and the result pane in
/// the root layout. `(editor, results)`.
pub const EDITOR_RESULTS_SPLIT_PCT: (u16, u16) = (55, 45);

/// Hard cap for the help modal: `(max_width, max_height)` in cells.
pub const HELP_MODAL_MAX: (u16, u16) = (64, 50);

/// Hard cap for the history modal: `(max_width, max_height)` in cells.
pub const HISTORY_MODAL_MAX: (u16, u16) = (80, 24);

/// Hard cap for the snippets modal: `(max_width, max_height)` in cells.
pub const SNIPPETS_MODAL_MAX: (u16, u16) = (50, 20);

/// Hard cap for the row-detail modal: `(max_width, max_height)` in cells.
pub const ROW_DETAIL_MODAL_MAX: (u16, u16) = (80, 30);

/// Width range of the completion popup: `(min, max)` in cells.
pub const COMPLETION_WIDTH_RANGE: (u16, u16) = (20, 100);

/// Maximum visible height (in rows) of the completion popup.
pub const COMPLETION_MAX_HEIGHT: u16 = 10;

/// Maximum width (in cells) of the cell-value popup and the in-place
/// cell editor.
pub const CELL_POPUP_MAX_WIDTH: u16 = 80;

/// Minimum legible width (in cells) for a result-grid column.
pub const RESULT_MIN_COLUMN_WIDTH: usize = 6;

/// Maximum width (in cells) a result-grid column may auto-expand to.
pub const RESULT_MAX_COLUMN_WIDTH: usize = 40;


/// Render throttle for streaming queries — collapses bursty
/// `RowsAppended` updates into one frame redraw per interval.
pub const STREAM_RENDER_THROTTLE: Duration = Duration::from_millis(100);

/// Debounce window for the post-DDL schema-refresh timer. A migration
/// firing 50 DDL statements still only issues one refresh.
pub const SCHEMA_REFRESH_DEBOUNCE: Duration = Duration::from_millis(200);

/// Default `LIMIT` for the Ctrl+R history modal load.
pub const HISTORY_LOAD_LIMIT: usize = 200;
