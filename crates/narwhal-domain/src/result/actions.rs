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

use super::{ResultBundle, ResultSearch, ResultState, SortDir};

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
            .map(|r| Row((0..cols).map(|c| Value::Int((r * cols + c) as i64)).collect()))
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
        assert_eq!(
            apply_sort_command(&mut b, None, false),
            "sort: cleared"
        );
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
        assert!(
            apply_filter_command(&mut b, None, false)
                .starts_with("filter: type to filter")
        );
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
}
