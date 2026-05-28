//! Pure result-pane mutators.
//!
//! Faz 1 Madde 3, Adım 7 (`results_actions` extraction).
//!
//! Each function here mutates a `ResultBundle` (and occasionally
//! sibling state on a `Tab`) without touching any IO surface. The
//! host's `AppCore` methods are thin shims that:
//!
//!   1. gather the relevant state slice (usually
//!      `tabs[active_tab].results` + a `bool` for streaming),
//!   2. call the function here,
//!   3. write the returned status message into `ui.status.message`.
//!
//! Functions return a `String` rather than mutating a status slot
//! directly so the host stays in control of which slot is written
//! (status / notification / silent), and so the unit tests in this
//! crate can assert on the message verbatim.

use narwhal_core::Value;

use super::{
    CellEdit, CellEditView, CellPopup, ResultBundle, ResultSearch, ResultState, RowDetailState,
    SortDir,
};

const STREAMING_BLOCKED: &str = "sort/filter unavailable while streaming";

/// `s` keybind: cycle the active column's sort
/// none → asc → desc → none.
///
/// Returns the status-line message describing the new state.
pub fn toggle_sort(bundle: &mut ResultBundle, is_streaming: bool) -> String {
    if is_streaming {
        return STREAMING_BLOCKED.into();
    }
    if !matches!(bundle.active_state(), ResultState::Rows { .. }) {
        return "no result to sort".into();
    }
    let col = bundle.active().column_index;
    let view = bundle.active_mut();
    view.sort = match view.sort {
        Some((c, SortDir::Asc)) if c == col => Some((col, SortDir::Desc)),
        Some((c, SortDir::Desc)) if c == col => None,
        _ => Some((col, SortDir::Asc)),
    };
    match view.sort {
        Some((c, SortDir::Asc)) => format!("sort: column {} ascending", c + 1),
        Some((c, SortDir::Desc)) => format!("sort: column {} descending", c + 1),
        None => "sort: cleared".into(),
    }
}

/// `:sort` command-palette setter. `column_1based`:
///   * `None` → clear active sort.
///   * `Some(n)` → toggle asc/desc/clear on 1-based column `n`.
///
/// Caller (the app shim) is responsible for mapping
/// `crate::commands::SortArg` to this `Option<usize>` so the domain
/// crate doesn't depend on the commands crate.
pub fn apply_sort_command(
    bundle: &mut ResultBundle,
    column_1based: Option<usize>,
    is_streaming: bool,
) -> String {
    if is_streaming {
        return STREAMING_BLOCKED.into();
    }
    if !matches!(bundle.active_state(), ResultState::Rows { .. }) {
        return "no result to sort".into();
    }
    let view = bundle.active_mut();
    match column_1based {
        None => {
            view.sort = None;
            "sort: cleared".into()
        }
        Some(n) => {
            let col = n.saturating_sub(1);
            view.sort = match view.sort {
                Some((c, SortDir::Asc)) if c == col => Some((col, SortDir::Desc)),
                Some((c, SortDir::Desc)) if c == col => None,
                _ => Some((col, SortDir::Asc)),
            };
            match view.sort {
                Some((c, SortDir::Asc)) => format!("sort: column {} ascending", c + 1),
                Some((c, SortDir::Desc)) => format!("sort: column {} descending", c + 1),
                None => "sort: cleared".into(),
            }
        }
    }
}

/// `:filter` command-palette setter.
///
///   * `None` → open the inline filter prompt for editing.
///   * `Some("")` → clear the active filter.
///   * `Some(expr)` → set the active filter verbatim.
pub fn apply_filter_command(
    bundle: &mut ResultBundle,
    spec: Option<String>,
    is_streaming: bool,
) -> String {
    if is_streaming {
        return STREAMING_BLOCKED.into();
    }
    if !matches!(bundle.active_state(), ResultState::Rows { .. }) {
        return "no result to filter".into();
    }
    let rv = bundle.active_mut();
    match spec {
        None => {
            rv.filter_prompt_open = true;
            "filter: type to filter, Enter accepts, Esc clears".into()
        }
        Some(expr) if expr.is_empty() => {
            rv.filter.clear();
            rv.filter_prompt_open = false;
            "filter: cleared".into()
        }
        Some(expr) => {
            rv.filter = expr.clone();
            rv.filter_prompt_open = false;
            format!("filter: {expr}")
        }
    }
}

/// `f` keybind: open the inline filter prompt. Idempotent — calling
/// it on an already-open prompt is a no-op modulo the status hint.
pub fn open_filter_prompt(bundle: &mut ResultBundle, is_streaming: bool) -> String {
    if is_streaming {
        return STREAMING_BLOCKED.into();
    }
    if !matches!(bundle.active_state(), ResultState::Rows { .. }) {
        return "no result to filter".into();
    }
    bundle.active_mut().filter_prompt_open = true;
    "filter: type to filter, Enter accepts, Esc clears".into()
}

/// Esc in the result pane: drop any active search and clear the
/// active filter. Returns the status message the host should display,
/// or an empty string when neither was active (in which case the
/// host should leave the status bar alone).
///
/// When both a search *and* a filter are active, the filter message
/// wins — it is the more recently-cleared state visible on screen.
/// Matches the historical behaviour where two assignments to
/// `status.message` happened in sequence inside the same handler.
pub fn handle_escape(search: &mut Option<ResultSearch>, bundle: &mut ResultBundle) -> &'static str {
    let had_search = search.take().is_some();
    let had_filter = !bundle.active().filter.is_empty();
    if had_filter {
        let rv = bundle.active_mut();
        rv.filter.clear();
        rv.filter_prompt_open = false;
        return "filter cleared";
    }
    if had_search {
        return "search cleared";
    }
    ""
}

/// Translate the active view's selected row index (which addresses
/// the post-filter / post-sort `visible_indices` list) back to the
/// original row index in the full result set. Returns `None` when
/// nothing is selected.
#[must_use]
pub fn selected_original_row(bundle: &ResultBundle) -> Option<usize> {
    let view = bundle.active();
    let vis_selected = view.selected()?;
    view.visible_indices.get(vis_selected).copied()
}

/// Mark the active cell-edit overlay with a failure message and
/// return the status-line text the host should display. The overlay
/// stays open so the user can fix the value and retry.
pub fn set_edit_error(bundle: &mut ResultBundle, message: &str) -> String {
    if let Some(view) = bundle.active_mut().edit.as_mut() {
        view.error = Some(message.to_owned());
    }
    format!("edit failed: {message}")
}

/// Format `raw` as pretty-printed JSON. When parsing fails, returns
/// `raw` unchanged plus the parser's error message; the JSON viewer
/// modal surfaces that as a muted footer so the user still gets
/// best-effort display for quasi-JSON text.
#[must_use]
pub fn prettify_json(raw: &str) -> (String, Option<String>) {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v) => match serde_json::to_string_pretty(&v) {
            Ok(s) => (s, None),
            Err(e) => (raw.to_owned(), Some(e.to_string())),
        },
        Err(e) => (raw.to_owned(), Some(e.to_string())),
    }
}

// ---------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------

/// `/` keybind: arm a fresh search prompt on the active tab. Returns
/// the status text the host should display, or `None` when there is
/// no result to search against.
pub fn start_search(
    tab_search: &mut Option<ResultSearch>,
    bundle: &ResultBundle,
) -> Option<String> {
    if !matches!(
        bundle.active_state(),
        ResultState::Rows { .. } | ResultState::Running { .. }
    ) {
        return Some("no result to search".into());
    }
    *tab_search = Some(ResultSearch {
        query: String::new(),
        matches: Vec::new(),
        current: None,
        editing: true,
    });
    Some("search: ".into())
}

/// Recompute the visible-row matches for the active search query.
/// Returns the status text the host should display, or `None` when
/// no status update is appropriate (no search armed).
///
/// Pure modulo the search slot and the resulting status message.
pub fn refresh_search_matches(
    tab_search: &mut Option<ResultSearch>,
    bundle: &ResultBundle,
) -> Option<String> {
    let needle = match tab_search.as_ref() {
        Some(s) if !s.query.is_empty() => s.query.to_lowercase(),
        Some(_) => {
            if let Some(s) = tab_search.as_mut() {
                s.matches.clear();
                s.current = None;
            }
            return Some("search: ".into());
        }
        None => return None,
    };
    let matches = match bundle.active_state() {
        ResultState::Rows { rows, .. } | ResultState::Running { rows, .. } => rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| {
                row.0
                    .iter()
                    .any(|v| v.render().to_lowercase().contains(&needle))
                    .then_some(i)
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let total = matches.len();
    let search = tab_search.as_mut()?;
    let query = search.query.clone();
    search.matches = matches;
    search.current = if total == 0 { None } else { Some(0) };
    Some(if total == 0 {
        format!("search: {query} \u{00b7} no matches")
    } else {
        format!("search: {query} \u{00b7} 1/{total}")
    })
}

/// `n` / `N` keybinds: cycle through search matches by `delta`
/// (positive forward, negative backward) and snap the result-pane
/// selection to the new match. Wraps. Returns the status text, or
/// `None` when there is no armed search / no matches.
pub fn advance_search(
    tab_search: &mut Option<ResultSearch>,
    bundle: &mut ResultBundle,
    delta: i32,
) -> Option<String> {
    let search = tab_search.as_mut()?;
    if search.matches.is_empty() {
        return None;
    }
    let len = search.matches.len() as i32;
    let current = search.current.unwrap_or(0) as i32;
    let next = (current + delta).rem_euclid(len) as usize;
    search.current = Some(next);
    let total = search.matches.len();
    let query = search.query.clone();
    let row_idx = search.matches.get(next).copied();
    let msg = format!("search: {query} \u{00b7} {}/{}", next + 1, total);
    if let Some(idx) = row_idx {
        bundle.active_mut().select(Some(idx));
    }
    Some(msg)
}

/// Snap the result-pane selection to the search's current match,
/// without changing the cursor inside `matches`. Used after
/// `refresh_search_matches` finds the first hit.
pub fn jump_to_current_match(tab_search: Option<&ResultSearch>, bundle: &mut ResultBundle) {
    let Some(idx) = tab_search.and_then(|s| s.current.and_then(|c| s.matches.get(c).copied()))
    else {
        return;
    };
    bundle.active_mut().select(Some(idx));
}

// ---------------------------------------------------------------------
// Cell popup / Row detail
// ---------------------------------------------------------------------

/// `Enter` on a row in the result pane: open the read-only cell
/// popup over the currently-focused cell. Returns the status text
/// the host should display, or `None` on success.
pub fn open_cell_popup(bundle: &mut ResultBundle) -> Option<String> {
    let Some(row_index) = selected_original_row(bundle) else {
        return Some("select a row first (j/k)".into());
    };
    let col_index = bundle.active().column_index;
    let (columns, rows) = match bundle.active_state() {
        ResultState::Rows { rows, columns, .. } | ResultState::Running { rows, columns, .. } => {
            (columns, rows)
        }
        _ => return None,
    };
    let column = columns.get(col_index)?;
    let row = rows.get(row_index)?;
    let value = row.0.get(col_index)?;
    let popup = CellPopup {
        column_name: column.name.clone(),
        column_type: column.data_type.clone(),
        value_text: value.render(),
        row_index,
    };
    bundle.active_mut().popup = Some(popup);
    None
}

/// `R` / `Shift+Enter` on a row: open the row detail modal. Skips
/// when another result-pane modal is already up (popup / cell edit /
/// existing row detail). Returns the status text, or `None` when the
/// modal opened successfully.
pub fn open_row_detail(
    bundle: &ResultBundle,
    row_detail: &mut Option<RowDetailState>,
    editing_is_open: bool,
) -> Option<String> {
    if row_detail.is_some() || bundle.active().popup.is_some() || editing_is_open {
        return None;
    }
    let Some(vis_selected) = bundle.active().selected() else {
        return Some("no row selected".into());
    };
    let (columns, rows) = match bundle.active_state() {
        ResultState::Rows { columns, rows, .. } | ResultState::Running { columns, rows, .. } => {
            (columns.clone(), rows.clone())
        }
        _ => return Some("no result to inspect".into()),
    };
    let visible = bundle.active().visible_rows(&columns, &rows);
    let Some(&row_idx) = visible.get(vis_selected) else {
        return Some("no row selected".into());
    };
    let row = rows.get(row_idx)?;
    *row_detail = Some(RowDetailState {
        row_index: row_idx,
        columns,
        values: row.0.clone(),
        selected_column: 0,
        scroll_offset: 0,
    });
    None
}

/// Navigation verbs accepted by the row-detail modal. The host
/// translates keyboard events to this enum and the modal mutates
/// itself accordingly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowDetailMotion {
    Up,
    Down,
    PageUp,
    PageDown,
    Top,
    Bottom,
    /// `Esc` / `R` / `Shift+Enter`: close the modal.
    Close,
}

// ---------------------------------------------------------------------
// Yank (clipboard text construction — IO stays in the host)
// ---------------------------------------------------------------------

/// Build the cell text the `y` keybind should write to the
/// clipboard. The host calls this, hands the returned string to its
/// clipboard implementation, then writes the status message itself.
///
/// `selected_row` is the original-row index from
/// [`selected_original_row`]; pass `Some(0)` if no row is highlighted
/// so the historical "first row" fallback survives.
pub fn prepare_yank_cell(
    bundle: &ResultBundle,
    selected_row: Option<usize>,
) -> Result<String, &'static str> {
    let view = bundle.active();
    let (rows, _columns) = match bundle.active_state() {
        ResultState::Rows { rows, columns, .. } | ResultState::Running { rows, columns, .. } => {
            (rows, columns)
        }
        _ => return Err("no cell to yank"),
    };
    let row_idx = selected_row.unwrap_or(0);
    let col_idx = view.column_index;
    let value = rows
        .get(row_idx)
        .and_then(|r| r.0.get(col_idx))
        .ok_or("no cell selected")?;
    Ok(render_cell_for_yank(value))
}

/// Build the row text the `Y` keybind should write to the clipboard,
/// plus the cell count for the status message. Cells are TAB-joined;
/// nulls render as empty strings (matching the historical handler).
pub fn prepare_yank_row(
    bundle: &ResultBundle,
    selected_row: Option<usize>,
) -> Result<(String, usize), &'static str> {
    let rows = match bundle.active_state() {
        ResultState::Rows { rows, .. } | ResultState::Running { rows, .. } => rows,
        _ => return Err("no row to yank"),
    };
    let row_idx = selected_row.unwrap_or(0);
    let row = rows.get(row_idx).ok_or("no row selected")?;
    let cells = row.0.len();
    let text = row
        .0
        .iter()
        .map(render_cell_for_yank)
        .collect::<Vec<_>>()
        .join("\t");
    Ok((text, cells))
}

fn render_cell_for_yank(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        other => other.render(),
    }
}

// ---------------------------------------------------------------------
// Cell edit
// ---------------------------------------------------------------------

/// Build the [`CellEdit`] + [`CellEditView`] pair the editor overlay
/// should mount, after walking every precondition (rows + columns are
/// non-empty, the source has a primary key, a row and column are
/// selected). The host installs the returned slot pair and writes the
/// "edit: Enter saves \u{00b7} Esc cancels" status itself.
///
/// The error variant carries the status text to display when one of
/// the preconditions fails, matching the historical handler verbatim.
#[allow(clippy::result_large_err)]
pub fn start_cell_edit(
    bundle: &ResultBundle,
    selected_row: Option<usize>,
) -> Result<(CellEdit, CellEditView), String> {
    let view = bundle.active();
    let (columns, rows, source) = match bundle.active_state() {
        ResultState::Rows {
            columns,
            rows,
            source: Some(source),
            ..
        } => (columns, rows, source),
        ResultState::Rows { source: None, .. } => {
            return Err("this result is read-only (no row source); preview a table to edit".into());
        }
        _ => return Err("no editable cell here".into()),
    };
    if columns.is_empty() || rows.is_empty() {
        return Err("no rows to edit".into());
    }
    if !source.columns.iter().any(|c| c.primary_key) {
        return Err(format!(
            "{}: no primary key, cell edits are disabled",
            source.table
        ));
    }
    let row_index = selected_row.unwrap_or(0);
    let col_index = view.column_index;
    let row = rows.get(row_index).ok_or("select a row first (j/k)")?;
    let column = columns
        .get(col_index)
        .ok_or("select a column first (h/l)")?;
    let cell = row.0.get(col_index);
    let original = cell.map(Value::render).unwrap_or_default();
    let buffer = if matches!(cell, Some(Value::Null) | None) {
        String::new()
    } else {
        original.clone()
    };
    let edit = CellEdit {
        column_name: column.name.clone(),
        column_type: column.data_type.clone(),
        row_index,
        column_index: col_index,
        original,
        buffer: buffer.clone(),
    };
    let view_overlay = CellEditView {
        column_name: edit.column_name.clone(),
        column_type: edit.column_type.clone(),
        row_index: edit.row_index,
        buffer: edit.buffer.clone(),
        error: None,
    };
    Ok((edit, view_overlay))
}

/// Motion verbs the cell editor accepts. The host translates
/// keyboard events to this enum and the editor mutates itself
/// accordingly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellEditMotion {
    /// `Esc`: drop both edit slots.
    Cancel,
    /// `Backspace`: pop one char from the buffer.
    Backspace,
    /// Any printable char: append to the buffer.
    Insert(char),
}

/// Outcome of [`apply_cell_edit_motion`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellEditOutcome {
    /// Host should leave both slots in place and skip the status bar.
    Continue,
    /// Host should clear both edit slots and post `status`.
    Cancelled { status: &'static str },
}

/// Apply a non-commit motion to the open cell editor. The `Enter`
/// (commit) verb stays in the host because it dispatches an async
/// queue write — it is not modelled here.
pub fn apply_cell_edit_motion(
    editing: &mut Option<CellEdit>,
    view: &mut Option<CellEditView>,
    motion: CellEditMotion,
) -> CellEditOutcome {
    let Some(edit) = editing.as_mut() else {
        return CellEditOutcome::Continue;
    };
    match motion {
        CellEditMotion::Cancel => {
            *editing = None;
            *view = None;
            CellEditOutcome::Cancelled {
                status: "edit cancelled",
            }
        }
        CellEditMotion::Backspace => {
            edit.buffer.pop();
            sync_edit_view(editing.as_ref(), view);
            CellEditOutcome::Continue
        }
        CellEditMotion::Insert(c) => {
            edit.buffer.push(c);
            sync_edit_view(editing.as_ref(), view);
            CellEditOutcome::Continue
        }
    }
}

/// Copy the in-flight buffer from the canonical [`CellEdit`] state
/// onto the [`CellEditView`] overlay and clear any previous error.
/// Called after every keystroke to keep the rendered overlay in sync
/// with the buffer the host is committing on Enter.
pub fn sync_edit_view(editing: Option<&CellEdit>, view: &mut Option<CellEditView>) {
    if let (Some(edit), Some(overlay)) = (editing, view.as_mut()) {
        overlay.buffer = edit.buffer.clone();
        overlay.error = None;
    }
}

/// Apply a navigation `motion` to an open row-detail modal.
///
/// Returns `Some(status_message)` when the modal should be **closed**
/// (i.e. the motion was `Close`); the host is then expected to drop
/// the `RowDetailState` and write the message to the status bar.
/// Returns `None` for in-place navigation that leaves the modal open.
pub fn apply_row_detail_motion(
    state: &mut RowDetailState,
    motion: RowDetailMotion,
) -> Option<&'static str> {
    let col_count = state.columns.len().saturating_sub(1);
    match motion {
        RowDetailMotion::Up => {
            state.selected_column = state.selected_column.saturating_sub(1);
            state.scroll_offset = 0;
        }
        RowDetailMotion::Down => {
            if state.selected_column < col_count {
                state.selected_column += 1;
            }
            state.scroll_offset = 0;
        }
        RowDetailMotion::PageUp => {
            let page = 10usize;
            state.selected_column = state.selected_column.saturating_sub(page);
            state.scroll_offset = 0;
        }
        RowDetailMotion::PageDown => {
            let page = 10usize;
            state.selected_column = (state.selected_column + page).min(col_count);
            state.scroll_offset = 0;
        }
        RowDetailMotion::Top => {
            state.selected_column = 0;
            state.scroll_offset = 0;
        }
        RowDetailMotion::Bottom => {
            state.selected_column = col_count;
            state.scroll_offset = 0;
        }
        RowDetailMotion::Close => return Some("row detail closed"),
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::ResultView;
    use narwhal_core::{ColumnHeader, Row, Value};

    fn rows_bundle(cols: usize, rows: usize) -> ResultBundle {
        let columns: Vec<ColumnHeader> = (0..cols)
            .map(|i| ColumnHeader {
                name: format!("c{i}"),
                data_type: "int".into(),
            })
            .collect();
        let body: Vec<Row> = (0..rows)
            .map(|r| {
                Row((0..cols)
                    .map(|c| Value::Int((r * cols + c) as i64))
                    .collect())
            })
            .collect();
        ResultBundle::single(
            ResultState::Rows {
                columns,
                rows: body,
                elapsed_ms: 0,
                streamed: false,
                index: 0,
                total: 1,
                source: None,
                source_table: None,
            },
            ResultView::new(),
        )
    }

    #[test]
    fn toggle_sort_cycles_asc_desc_none() {
        let mut b = rows_bundle(3, 5);
        b.active_mut().column_index = 1;

        assert_eq!(toggle_sort(&mut b, false), "sort: column 2 ascending");
        assert_eq!(toggle_sort(&mut b, false), "sort: column 2 descending");
        assert_eq!(toggle_sort(&mut b, false), "sort: cleared");
    }

    #[test]
    fn toggle_sort_blocked_while_streaming() {
        let mut b = rows_bundle(2, 2);
        assert_eq!(toggle_sort(&mut b, true), STREAMING_BLOCKED);
        // No mutation should have happened.
        assert!(b.active().sort.is_none());
    }

    #[test]
    fn toggle_sort_no_rows_state() {
        let mut b = ResultBundle::default();
        assert_eq!(toggle_sort(&mut b, false), "no result to sort");
    }

    #[test]
    fn apply_sort_command_clear() {
        let mut b = rows_bundle(2, 2);
        b.active_mut().sort = Some((0, SortDir::Asc));
        assert_eq!(apply_sort_command(&mut b, None, false), "sort: cleared");
        assert!(b.active().sort.is_none());
    }

    #[test]
    fn apply_sort_command_column_one_based() {
        let mut b = rows_bundle(3, 3);
        assert_eq!(
            apply_sort_command(&mut b, Some(2), false),
            "sort: column 2 ascending"
        );
        assert_eq!(b.active().sort, Some((1, SortDir::Asc)));
    }

    #[test]
    fn apply_filter_command_set_clear_open() {
        let mut b = rows_bundle(2, 2);

        // Open prompt.
        assert!(apply_filter_command(&mut b, None, false).starts_with("filter: type to filter"));
        assert!(b.active().filter_prompt_open);

        // Set value.
        assert_eq!(
            apply_filter_command(&mut b, Some("foo".into()), false),
            "filter: foo"
        );
        assert_eq!(b.active().filter, "foo");
        assert!(!b.active().filter_prompt_open);

        // Clear.
        assert_eq!(
            apply_filter_command(&mut b, Some(String::new()), false),
            "filter: cleared"
        );
        assert_eq!(b.active().filter, "");
    }

    #[test]
    fn open_filter_prompt_sets_flag() {
        let mut b = rows_bundle(2, 2);
        assert!(open_filter_prompt(&mut b, false).starts_with("filter: type"));
        assert!(b.active().filter_prompt_open);
    }

    #[test]
    fn handle_escape_filter_wins_over_search() {
        let mut b = rows_bundle(2, 2);
        b.active_mut().filter = "foo".into();
        let mut search = Some(ResultSearch {
            query: "x".into(),
            matches: vec![0],
            current: Some(0),
            editing: false,
        });
        assert_eq!(handle_escape(&mut search, &mut b), "filter cleared");
        assert!(search.is_none(), "search dropped");
        assert_eq!(b.active().filter, "");
    }

    #[test]
    fn handle_escape_only_search() {
        let mut b = rows_bundle(2, 2);
        let mut search = Some(ResultSearch::default());
        assert_eq!(handle_escape(&mut search, &mut b), "search cleared");
        assert!(search.is_none());
    }

    #[test]
    fn handle_escape_nothing_active() {
        let mut b = rows_bundle(2, 2);
        let mut search: Option<ResultSearch> = None;
        assert_eq!(handle_escape(&mut search, &mut b), "");
    }

    #[test]
    fn selected_original_row_uses_visible_indices() {
        let mut b = rows_bundle(2, 5);
        // Pretend filter/sort produced visible_indices = [2, 4, 1]
        // and the user selected the second visible row.
        b.active_mut().visible_indices = vec![2, 4, 1];
        b.active_mut().select(Some(1));
        assert_eq!(selected_original_row(&b), Some(4));
    }

    #[test]
    fn selected_original_row_none_when_unselected() {
        let b = rows_bundle(2, 5);
        assert_eq!(selected_original_row(&b), None);
    }

    #[test]
    fn set_edit_error_writes_overlay_and_status() {
        use crate::result::CellEditView;
        let mut b = rows_bundle(2, 2);
        b.active_mut().edit = Some(CellEditView {
            column_name: "id".into(),
            column_type: "int".into(),
            row_index: 0,
            buffer: "abc".into(),
            error: None,
        });
        let msg = set_edit_error(&mut b, "not an int");
        assert_eq!(msg, "edit failed: not an int");
        assert_eq!(
            b.active().edit.as_ref().unwrap().error.as_deref(),
            Some("not an int"),
        );
    }

    #[test]
    fn prettify_json_ok_and_err() {
        let (pretty, err) = prettify_json(r#"{"a":1}"#);
        assert!(err.is_none());
        assert!(pretty.contains('\n'), "pretty-printed has newline");

        let (raw, err) = prettify_json("not json");
        assert_eq!(raw, "not json");
        assert!(err.is_some());
    }

    #[test]
    fn start_search_arms_prompt() {
        let b = rows_bundle(2, 3);
        let mut search = None;
        assert_eq!(start_search(&mut search, &b), Some("search: ".into()));
        let s = search.unwrap();
        assert!(s.editing);
        assert!(s.query.is_empty());
    }

    #[test]
    fn start_search_blocked_on_non_rows() {
        let b = ResultBundle::default();
        let mut search = None;
        assert_eq!(
            start_search(&mut search, &b),
            Some("no result to search".into())
        );
        assert!(search.is_none());
    }

    #[test]
    fn refresh_search_finds_and_reports_total() {
        use narwhal_core::Value;
        // Build a bundle whose row 1 contains the substring "foo".
        let mut b = rows_bundle(2, 0);
        if let ResultState::Rows { rows, .. } = b.active_state_mut() {
            rows.push(narwhal_core::Row(vec![
                Value::String("alpha".into()),
                Value::String("beta".into()),
            ]));
            rows.push(narwhal_core::Row(vec![
                Value::String("foo".into()),
                Value::String("bar".into()),
            ]));
            rows.push(narwhal_core::Row(vec![
                Value::String("gamma".into()),
                Value::String("foozzz".into()),
            ]));
        }
        let mut search = Some(ResultSearch {
            query: "foo".into(),
            matches: Vec::new(),
            current: None,
            editing: false,
        });
        let msg = refresh_search_matches(&mut search, &b).unwrap();
        assert!(msg.ends_with("1/2"), "got: {msg}");
        let s = search.unwrap();
        assert_eq!(s.matches, vec![1, 2]);
        assert_eq!(s.current, Some(0));
    }

    #[test]
    fn refresh_search_empty_query_clears_matches() {
        let b = rows_bundle(2, 2);
        let mut search = Some(ResultSearch {
            query: String::new(),
            matches: vec![0, 1],
            current: Some(0),
            editing: false,
        });
        assert_eq!(
            refresh_search_matches(&mut search, &b),
            Some("search: ".into())
        );
        let s = search.unwrap();
        assert!(s.matches.is_empty());
        assert!(s.current.is_none());
    }

    #[test]
    fn advance_search_wraps_and_snaps_selection() {
        let mut b = rows_bundle(2, 5);
        let mut search = Some(ResultSearch {
            query: "x".into(),
            matches: vec![1, 3, 4],
            current: Some(0),
            editing: false,
        });
        let msg = advance_search(&mut search, &mut b, 1).unwrap();
        assert!(msg.ends_with("2/3"), "got: {msg}");
        assert_eq!(b.active().selected(), Some(3));

        // Wrap backwards from index 0
        search.as_mut().unwrap().current = Some(0);
        advance_search(&mut search, &mut b, -1);
        assert_eq!(search.as_ref().unwrap().current, Some(2));
        assert_eq!(b.active().selected(), Some(4));
    }

    #[test]
    fn open_cell_popup_requires_selection() {
        let mut b = rows_bundle(2, 3);
        assert_eq!(
            open_cell_popup(&mut b),
            Some("select a row first (j/k)".into()),
        );
        assert!(b.active().popup.is_none());
    }

    #[test]
    fn open_cell_popup_writes_popup() {
        let mut b = rows_bundle(2, 3);
        b.active_mut().select(Some(1));
        b.active_mut().visible_indices = vec![0, 1, 2];
        b.active_mut().column_index = 1;
        assert!(open_cell_popup(&mut b).is_none());
        let popup = b.active().popup.as_ref().unwrap();
        assert_eq!(popup.column_name, "c1");
        assert_eq!(popup.row_index, 1);
    }

    #[test]
    fn open_row_detail_opens_when_clean() {
        let mut b = rows_bundle(3, 4);
        b.active_mut().select(Some(2));
        let mut rd = None;
        assert_eq!(open_row_detail(&b, &mut rd, false), None);
        let s = rd.unwrap();
        assert_eq!(s.row_index, 2);
        assert_eq!(s.columns.len(), 3);
        assert_eq!(s.selected_column, 0);
    }

    #[test]
    fn open_row_detail_blocked_by_popup() {
        let mut b = rows_bundle(3, 4);
        b.active_mut().select(Some(0));
        b.active_mut().popup = Some(CellPopup {
            column_name: "x".into(),
            column_type: "int".into(),
            value_text: "1".into(),
            row_index: 0,
        });
        let mut rd = None;
        // popup is up → silently skip, no status, no state change
        assert_eq!(open_row_detail(&b, &mut rd, false), None);
        assert!(rd.is_none());
    }

    #[test]
    fn apply_row_detail_motion_close_signals_drop() {
        let mut s = RowDetailState {
            row_index: 0,
            columns: Vec::new(),
            values: Vec::new(),
            selected_column: 0,
            scroll_offset: 0,
        };
        assert_eq!(
            apply_row_detail_motion(&mut s, RowDetailMotion::Close),
            Some("row detail closed"),
        );
    }

    #[test]
    fn yank_cell_renders_null_as_empty_and_other_via_render() {
        let mut b = rows_bundle(2, 0);
        if let ResultState::Rows { rows, .. } = b.active_state_mut() {
            rows.push(narwhal_core::Row(vec![
                Value::Null,
                Value::String("x".into()),
            ]));
        }
        b.active_mut().column_index = 0;
        assert_eq!(prepare_yank_cell(&b, Some(0)), Ok(String::new()));
        b.active_mut().column_index = 1;
        assert_eq!(prepare_yank_cell(&b, Some(0)), Ok("x".to_string()));
    }

    #[test]
    fn yank_cell_no_state_errors() {
        let b = ResultBundle::default();
        assert_eq!(prepare_yank_cell(&b, Some(0)), Err("no cell to yank"));
    }

    #[test]
    fn yank_row_tab_joins_and_counts() {
        let mut b = rows_bundle(3, 0);
        if let ResultState::Rows { rows, .. } = b.active_state_mut() {
            rows.push(narwhal_core::Row(vec![
                Value::Int(1),
                Value::Null,
                Value::String("hello".into()),
            ]));
        }
        let (text, cells) = prepare_yank_row(&b, Some(0)).unwrap();
        assert_eq!(cells, 3);
        assert_eq!(text, "1\t\thello");
    }

    #[test]
    fn start_cell_edit_needs_row_source_and_pk() {
        let mut b = rows_bundle(2, 1);
        // `rows_bundle` builds a `Rows` with `source: None`.
        assert!(start_cell_edit(&b, Some(0)).is_err_and(|e| e.contains("read-only")));

        // Patch in a row source without a primary key column.
        if let ResultState::Rows { source, .. } = b.active_state_mut() {
            *source = Some(crate::result::RowSource {
                schema: "main".into(),
                table: "users".into(),
                columns: vec![narwhal_core::Column {
                    name: "id".into(),
                    data_type: "int".into(),
                    primary_key: false,
                    nullable: true,
                    default: None,
                }],
                offset: 0,
                limit: 100,
            });
        }
        assert!(start_cell_edit(&b, Some(0)).is_err_and(|e| e.contains("no primary key")));
    }

    #[test]
    fn start_cell_edit_happy_path_clones_original_into_buffer() {
        let mut b = rows_bundle(2, 1);
        if let ResultState::Rows { source, rows, .. } = b.active_state_mut() {
            *source = Some(crate::result::RowSource {
                schema: "main".into(),
                table: "t".into(),
                columns: vec![narwhal_core::Column {
                    name: "id".into(),
                    data_type: "int".into(),
                    primary_key: true,
                    nullable: false,
                    default: None,
                }],
                offset: 0,
                limit: 100,
            });
            rows[0] = narwhal_core::Row(vec![Value::Int(42), Value::String("alpha".into())]);
        }
        b.active_mut().column_index = 1;
        let (edit, view) = start_cell_edit(&b, Some(0)).expect("ok");
        assert_eq!(edit.original, "alpha");
        assert_eq!(edit.buffer, "alpha");
        assert_eq!(edit.column_index, 1);
        assert_eq!(view.buffer, "alpha");
        assert!(view.error.is_none());
    }

    #[test]
    fn start_cell_edit_null_cell_starts_with_empty_buffer() {
        let mut b = rows_bundle(1, 1);
        if let ResultState::Rows { source, rows, .. } = b.active_state_mut() {
            *source = Some(crate::result::RowSource {
                schema: "main".into(),
                table: "t".into(),
                columns: vec![narwhal_core::Column {
                    name: "x".into(),
                    data_type: "int".into(),
                    primary_key: true,
                    nullable: true,
                    default: None,
                }],
                offset: 0,
                limit: 1,
            });
            rows[0] = narwhal_core::Row(vec![Value::Null]);
        }
        let (edit, _view) = start_cell_edit(&b, Some(0)).expect("ok");
        // `Value::Null.render()` returns the literal `"NULL"`; the
        // historical handler stores that verbatim as the snapshot for
        // the cancel path — the buffer is forced to empty so the user
        // doesn't have to delete the placeholder before typing.
        assert_eq!(edit.original, "NULL");
        assert_eq!(edit.buffer, "");
    }

    #[test]
    fn apply_cell_edit_motion_cancel_clears_slots() {
        let mut editing = Some(CellEdit {
            column_name: "c".into(),
            column_type: "int".into(),
            row_index: 0,
            column_index: 0,
            original: "1".into(),
            buffer: "2".into(),
        });
        let mut view = Some(CellEditView {
            column_name: "c".into(),
            column_type: "int".into(),
            row_index: 0,
            buffer: "2".into(),
            error: None,
        });
        assert_eq!(
            apply_cell_edit_motion(&mut editing, &mut view, CellEditMotion::Cancel),
            CellEditOutcome::Cancelled {
                status: "edit cancelled"
            },
        );
        assert!(editing.is_none());
        assert!(view.is_none());
    }

    #[test]
    fn apply_cell_edit_motion_insert_then_backspace_round_trip() {
        let mut editing = Some(CellEdit {
            column_name: "c".into(),
            column_type: "text".into(),
            row_index: 0,
            column_index: 0,
            original: "abc".into(),
            buffer: "abc".into(),
        });
        let mut view = Some(CellEditView {
            column_name: "c".into(),
            column_type: "text".into(),
            row_index: 0,
            buffer: "abc".into(),
            error: None,
        });
        // Insert 'd' → buffer = "abcd", view mirrors.
        apply_cell_edit_motion(&mut editing, &mut view, CellEditMotion::Insert('d'));
        assert_eq!(editing.as_ref().unwrap().buffer, "abcd");
        assert_eq!(view.as_ref().unwrap().buffer, "abcd");
        // Backspace → buffer = "abc", view mirrors.
        apply_cell_edit_motion(&mut editing, &mut view, CellEditMotion::Backspace);
        assert_eq!(editing.as_ref().unwrap().buffer, "abc");
        assert_eq!(view.as_ref().unwrap().buffer, "abc");
    }

    #[test]
    fn sync_edit_view_clears_error_and_copies_buffer() {
        let editing = Some(CellEdit {
            column_name: "c".into(),
            column_type: "text".into(),
            row_index: 0,
            column_index: 0,
            original: "x".into(),
            buffer: "y".into(),
        });
        let mut view = Some(CellEditView {
            column_name: "c".into(),
            column_type: "text".into(),
            row_index: 0,
            buffer: "stale".into(),
            error: Some("previous error".into()),
        });
        sync_edit_view(editing.as_ref(), &mut view);
        let v = view.as_ref().unwrap();
        assert_eq!(v.buffer, "y");
        assert!(v.error.is_none());
    }

    #[test]
    fn apply_row_detail_motion_navigation_clamps_and_resets_scroll() {
        let mut s = RowDetailState {
            row_index: 0,
            columns: vec![
                narwhal_core::ColumnHeader {
                    name: "a".into(),
                    data_type: "int".into(),
                },
                narwhal_core::ColumnHeader {
                    name: "b".into(),
                    data_type: "int".into(),
                },
                narwhal_core::ColumnHeader {
                    name: "c".into(),
                    data_type: "int".into(),
                },
            ],
            values: Vec::new(),
            selected_column: 0,
            scroll_offset: 7,
        };
        assert_eq!(apply_row_detail_motion(&mut s, RowDetailMotion::Down), None);
        assert_eq!(s.selected_column, 1);
        assert_eq!(s.scroll_offset, 0);

        s.selected_column = 1;
        s.scroll_offset = 4;
        apply_row_detail_motion(&mut s, RowDetailMotion::Up);
        assert_eq!(s.selected_column, 0);
        assert_eq!(s.scroll_offset, 0);

        apply_row_detail_motion(&mut s, RowDetailMotion::Bottom);
        assert_eq!(s.selected_column, 2);

        apply_row_detail_motion(&mut s, RowDetailMotion::Top);
        assert_eq!(s.selected_column, 0);
    }
}
