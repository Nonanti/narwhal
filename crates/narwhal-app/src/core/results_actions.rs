//! Result-pane interactive operations extracted from `core.rs` (L21).
//!
//! Handles every key event that targets the active [`super::ResultBundle`]:
//! navigation, search, filtering, sort toggling, cell yank/edit, row
//! detail modal, and the row-popup overlay.
use crossterm::event::{KeyCode as CtKey, KeyEvent, KeyModifiers};
use narwhal_tui::{CellEditView, CellPopup, SortDir};

use super::{AppCore, CellEdit, ResultSearch, ResultState, RowDetailState};
use crate::run::RunTarget;

impl AppCore {
    pub(super) fn handle_results_key(&mut self, key: KeyEvent) {
        // Row detail modal: sits at the same layer as the cell popup.
        // When open, it intercepts navigation and dismiss keys.
        if self.tabs[self.active_tab].row_detail.is_some() {
            self.handle_row_detail_key(key);
            return;
        }
        if self.tabs[self.active_tab].editing.is_some() {
            self.handle_cell_edit_key(key);
            return;
        }
        if self.tabs[self.active_tab].results.active().popup.is_some() {
            if matches!(key.code, CtKey::Esc | CtKey::Char('q') | CtKey::Enter) {
                self.tabs[self.active_tab].results.active_mut().popup = None;
            }
            return;
        }
        // Filter prompt editing: modal — consumes keys before any
        // other result-pane handler.
        if self.tabs[self.active_tab]
            .results
            .active()
            .filter_prompt_open
        {
            match key.code {
                CtKey::Esc => {
                    let rv = self.tabs[self.active_tab].results.active_mut();
                    rv.filter.clear();
                    rv.filter_prompt_open = false;
                    self.status.message = "filter cleared".into();
                }
                CtKey::Enter => {
                    let filter_text = self.tabs[self.active_tab].results.active().filter.clone();
                    self.tabs[self.active_tab]
                        .results
                        .active_mut()
                        .filter_prompt_open = false;
                    self.status.message = if filter_text.is_empty() {
                        "filter closed".into()
                    } else {
                        format!("filter: {filter_text}")
                    };
                }
                CtKey::Backspace => {
                    self.tabs[self.active_tab].results.active_mut().filter.pop();
                }
                CtKey::Char(c) => {
                    self.tabs[self.active_tab]
                        .results
                        .active_mut()
                        .filter
                        .push(c);
                }
                _ => {}
            }
            return;
        }
        if let Some(search) = self.tabs[self.active_tab].search.as_mut() {
            if search.editing {
                match key.code {
                    CtKey::Esc => {
                        self.tabs[self.active_tab].search = None;
                        self.status.message = "search cancelled".into();
                    }
                    CtKey::Enter => {
                        search.editing = false;
                        self.refresh_search_matches();
                        self.jump_to_current_match();
                    }
                    CtKey::Backspace => {
                        search.query.pop();
                        self.refresh_search_matches();
                    }
                    CtKey::Char(c) => {
                        search.query.push(c);
                        self.refresh_search_matches();
                    }
                    _ => {}
                }
                return;
            }
        }

        // Compute visible row count (after filter/sort) for navigation.
        let (visible_count, col_count) = match self.tabs[self.active_tab].results.active_state() {
            ResultState::Rows { rows, columns, .. } => {
                let vis = self.tabs[self.active_tab]
                    .results
                    .active()
                    .visible_rows(columns, rows);
                (vis.len(), columns.len())
            }
            ResultState::Running { rows, columns, .. } => (rows.len(), columns.len()),
            _ => (0, 0),
        };

        match key.code {
            CtKey::Char('j') | CtKey::Down => self.tabs[self.active_tab]
                .results
                .active_mut()
                .move_down(visible_count),
            CtKey::Char('k') | CtKey::Up => {
                self.tabs[self.active_tab].results.active_mut().move_up()
            }
            CtKey::Char('h') | CtKey::Left => {
                self.tabs[self.active_tab].results.active_mut().move_left()
            }
            CtKey::Char('l') | CtKey::Right => self.tabs[self.active_tab]
                .results
                .active_mut()
                .move_right(col_count),
            CtKey::Char('g') => self.tabs[self.active_tab]
                .results
                .active_mut()
                .select(Some(0)),
            CtKey::Char('G') if visible_count > 0 => {
                self.tabs[self.active_tab]
                    .results
                    .active_mut()
                    .select(Some(visible_count - 1));
            }
            CtKey::Char('s') => self.toggle_sort(),
            CtKey::Char('/') => self.open_filter_prompt(),
            CtKey::Char('n') => self.advance_search(1),
            CtKey::Char('N') => self.advance_search(-1),
            CtKey::Esc => {
                let had_search = self.tabs[self.active_tab].search.take().is_some();
                let had_filter = !self.tabs[self.active_tab]
                    .results
                    .active()
                    .filter
                    .is_empty();
                if had_search {
                    self.status.message = "search cleared".into();
                }
                if had_filter {
                    let rv = self.tabs[self.active_tab].results.active_mut();
                    rv.filter.clear();
                    rv.filter_prompt_open = false;
                    self.status.message = "filter cleared".into();
                }
            }
            CtKey::Enter => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.open_row_detail();
                } else {
                    self.open_cell_popup();
                }
            }
            CtKey::Char('R') => self.open_row_detail(),
            CtKey::Char('e') => self.start_cell_edit(),
            CtKey::Char('y') => self.yank_cell(),
            CtKey::Char('Y') => self.yank_row(),
            CtKey::Char(']') => {
                self.pending_result_leader = Some(']');
            }
            CtKey::Char('[') => {
                self.pending_result_leader = Some('[');
            }
            _ => {}
        }
    }

    // ----- yank -----

    /// Translate the current TableState selection (which is an index
    /// into the visible/rendered rows) to the original row index in
    /// the full result set. Returns `None` when there are no rows.
    fn selected_original_row(&self) -> Option<usize> {
        let tab = &self.tabs[self.active_tab];
        let vis_selected = tab.results.active().selected()?;
        tab.results
            .active()
            .visible_indices
            .get(vis_selected)
            .copied()
    }

    fn yank_cell(&mut self) {
        let tab = &self.tabs[self.active_tab];
        let (rows, _columns) = match tab.results.active_state() {
            ResultState::Rows { rows, columns, .. }
            | ResultState::Running { rows, columns, .. } => (rows, columns),
            _ => {
                self.status.message = "no cell to yank".into();
                return;
            }
        };
        let row_idx = self.selected_original_row().unwrap_or(0);
        let col_idx = tab.results.active().column_index;
        let Some(value) = rows.get(row_idx).and_then(|r| r.0.get(col_idx)) else {
            self.status.message = "no cell selected".into();
            return;
        };
        let text = match value {
            narwhal_core::Value::Null => String::new(),
            other => other.render(),
        };
        match self.clipboard.set_text(&text) {
            Ok(()) => {
                self.status.message = format!("yanked {} char(s) to clipboard", text.len());
            }
            Err(error) => {
                self.status.message = format!("yank failed: {error}");
            }
        }
    }

    fn yank_row(&mut self) {
        let tab = &self.tabs[self.active_tab];
        let rows = match tab.results.active_state() {
            ResultState::Rows { rows, .. } | ResultState::Running { rows, .. } => rows,
            _ => {
                self.status.message = "no row to yank".into();
                return;
            }
        };
        let row_idx = self.selected_original_row().unwrap_or(0);
        let Some(row) = rows.get(row_idx) else {
            self.status.message = "no row selected".into();
            return;
        };
        let text = row
            .0
            .iter()
            .map(|v| match v {
                narwhal_core::Value::Null => String::new(),
                other => other.render(),
            })
            .collect::<Vec<_>>()
            .join("\t");
        match self.clipboard.set_text(&text) {
            Ok(()) => {
                self.status.message = format!("yanked row ({} cell(s)) to clipboard", row.0.len());
            }
            Err(error) => {
                self.status.message = format!("yank failed: {error}");
            }
        }
    }

    fn start_cell_edit(&mut self) {
        // Gather the data we need by value first, then mutate.
        let prepared = {
            let tab = &self.tabs[self.active_tab];
            let (columns, rows, source) = match tab.results.active_state() {
                ResultState::Rows {
                    columns,
                    rows,
                    source: Some(source),
                    ..
                } => (columns, rows, source),
                ResultState::Rows { source: None, .. } => {
                    self.status.message =
                        "this result is read-only (no row source); preview a table to edit".into();
                    return;
                }
                _ => {
                    self.status.message = "no editable cell here".into();
                    return;
                }
            };
            if columns.is_empty() || rows.is_empty() {
                self.status.message = "no rows to edit".into();
                return;
            }
            if !source.columns.iter().any(|c| c.primary_key) {
                self.status.message =
                    format!("{}: no primary key, cell edits are disabled", source.table);
                return;
            }
            let row_index = self.selected_original_row().unwrap_or(0);
            let col_index = tab.results.active().column_index;
            let Some(row) = rows.get(row_index) else {
                self.status.message = "select a row first (j/k)".into();
                return;
            };
            let Some(column) = columns.get(col_index) else {
                self.status.message = "select a column first (h/l)".into();
                return;
            };
            let cell = row.0.get(col_index);
            let original = cell.map(|v| v.render()).unwrap_or_default();
            let buffer = if matches!(cell, Some(narwhal_core::Value::Null) | None) {
                String::new()
            } else {
                original.clone()
            };
            (
                column.name.clone(),
                column.data_type.clone(),
                row_index,
                col_index,
                original,
                buffer,
            )
        };
        let (column_name, column_type, row_index, column_index, original, buffer) = prepared;
        let tab = &mut self.tabs[self.active_tab];
        tab.editing = Some(CellEdit {
            column_name: column_name.clone(),
            column_type: column_type.clone(),
            row_index,
            column_index,
            original,
            buffer: buffer.clone(),
        });
        tab.results.active_mut().edit = Some(CellEditView {
            column_name,
            column_type,
            row_index,
            buffer,
            error: None,
        });
        self.status.message = "edit: Enter saves · Esc cancels".into();
    }

    fn handle_cell_edit_key(&mut self, key: KeyEvent) {
        let Some(edit) = self.tabs[self.active_tab].editing.as_mut() else {
            return;
        };
        match key.code {
            CtKey::Esc => {
                self.tabs[self.active_tab].editing = None;
                self.tabs[self.active_tab].results.active_mut().edit = None;
                self.status.message = "edit cancelled".into();
            }
            CtKey::Enter => self.commit_cell_edit(),
            CtKey::Backspace => {
                edit.buffer.pop();
                self.sync_edit_view();
            }
            CtKey::Char(c) => {
                edit.buffer.push(c);
                self.sync_edit_view();
            }
            _ => {}
        }
    }

    fn sync_edit_view(&mut self) {
        let tab = &mut self.tabs[self.active_tab];
        if let (Some(edit), Some(view)) =
            (tab.editing.as_ref(), tab.results.active_mut().edit.as_mut())
        {
            view.buffer = edit.buffer.clone();
            view.error = None;
        }
    }

    fn commit_cell_edit(&mut self) {
        let Some(edit) = self.tabs[self.active_tab].editing.clone() else {
            return;
        };
        let Some(session) = self.session.as_ref() else {
            self.status.message = "no active connection".into();
            return;
        };
        // Extract everything we need from the result before mutating state.
        let (columns, rows, source) = if let ResultState::Rows {
            columns,
            rows,
            source: Some(source),
            ..
        } = self.tabs[self.active_tab].results.active_state()
        {
            (columns.clone(), rows.clone(), source.clone())
        } else {
            self.status.message = "result is no longer editable".into();
            return;
        };
        let Some(row) = rows.get(edit.row_index).cloned() else {
            self.status.message = "row went away under the editor".into();
            return;
        };
        // L34: prefer the typed parser so editing a `TEXT` column with
        // the value `"true"` stays a string instead of becoming a bool.
        let hint = columns
            .iter()
            .find(|c| c.name == edit.column_name)
            .map(|c| c.data_type.as_str());
        let new_value = crate::edit::parse_input_typed(&edit.buffer, hint);
        let column_order: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let dialect = session.dialect();
        let compiled = match crate::edit::build_update(
            &source.schema,
            &source.table,
            &source.columns,
            &edit.column_name,
            &new_value,
            &row,
            &column_order,
            dialect,
        ) {
            Ok(c) => c,
            Err(error) => {
                self.set_edit_error(error);
                return;
            }
        };
        // Execute the UPDATE on a pool connection (or the open transaction
        // if there is one). We do this synchronously via block_in_place
        // because the edit popup is modal; the UI is already paused.
        let target = match session.transaction.as_ref() {
            Some(txn) => RunTarget::Pinned(txn.conn.clone()),
            None => RunTarget::Pool(session.pool.clone()),
        };
        let sql = compiled.sql.clone();
        let params = compiled.params;
        let outcome = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                match target {
                    RunTarget::Pool(pool) => {
                        let mut conn = pool
                            .acquire()
                            .await
                            .map_err(|e| narwhal_core::Error::Connection(e.to_string()))?;
                        conn.execute(&sql, &params).await
                    }
                    RunTarget::Pinned(handle) => {
                        let mut guard = handle.lock().await;
                        guard.execute(&sql, &params).await
                    }
                }
            })
        });
        match outcome {
            Ok(qr) => {
                let affected = qr.rows_affected.unwrap_or(0);
                if affected != 1 {
                    self.set_edit_error(format!(
                        "refused: UPDATE matched {affected} rows (expected exactly 1)"
                    ));
                    return;
                }
                // Patch the in-memory cell with the parsed value so the
                // grid reflects the new state without re-fetching.
                if let ResultState::Rows { rows, .. } =
                    self.tabs[self.active_tab].results.active_state_mut()
                {
                    if let Some(row) = rows.get_mut(edit.row_index) {
                        if let Some(cell) = row.0.get_mut(edit.column_index) {
                            *cell = new_value;
                        }
                    }
                }
                self.tabs[self.active_tab].editing = None;
                self.tabs[self.active_tab].results.active_mut().edit = None;
                self.status.message = format!("updated 1 row in {}", source.table);
            }
            Err(error) => {
                self.set_edit_error(error.to_string());
            }
        }
    }

    fn set_edit_error(&mut self, message: String) {
        if let Some(view) = self.tabs[self.active_tab]
            .results
            .active_mut()
            .edit
            .as_mut()
        {
            view.error = Some(message.clone());
        }
        self.status.message = format!("edit failed: {message}");
    }

    #[allow(dead_code)]
    fn start_search(&mut self) {
        if !matches!(
            self.tabs[self.active_tab].results.active_state(),
            ResultState::Rows { .. } | ResultState::Running { .. }
        ) {
            self.status.message = "no result to search".into();
            return;
        }
        self.tabs[self.active_tab].search = Some(ResultSearch {
            query: String::new(),
            matches: Vec::new(),
            current: None,
            editing: true,
        });
        self.status.message = "search: ".into();
    }

    pub(super) fn toggle_sort(&mut self) {
        // Streaming guard.
        if self.running {
            self.status.message = "sort/filter unavailable while streaming".into();
            return;
        }
        if !matches!(
            self.tabs[self.active_tab].results.active_state(),
            ResultState::Rows { .. }
        ) {
            self.status.message = "no result to sort".into();
            return;
        }
        let col = self.tabs[self.active_tab].results.active().column_index;
        let view = self.tabs[self.active_tab].results.active_mut();
        let next = match view.sort {
            Some((c, SortDir::Asc)) if c == col => Some((col, SortDir::Desc)),
            Some((c, SortDir::Desc)) if c == col => None,
            _ => Some((col, SortDir::Asc)),
        };
        view.sort = next;
        let msg = match view.sort {
            Some((c, SortDir::Asc)) => format!("sort: column {} ascending", c + 1),
            Some((c, SortDir::Desc)) => format!("sort: column {} descending", c + 1),
            None => "sort: cleared".into(),
            // Future SortDir variants: fall back to ascending wording.
            Some((c, _)) => format!("sort: column {} (custom)", c + 1),
        };
        self.status.message = msg;
    }

    fn open_filter_prompt(&mut self) {
        // Streaming guard.
        if self.running {
            self.status.message = "sort/filter unavailable while streaming".into();
            return;
        }
        if !matches!(
            self.tabs[self.active_tab].results.active_state(),
            ResultState::Rows { .. }
        ) {
            self.status.message = "no result to filter".into();
            return;
        }
        self.tabs[self.active_tab]
            .results
            .active_mut()
            .filter_prompt_open = true;
        self.status.message = "filter: type to filter, Enter accepts, Esc clears".into();
    }

    fn refresh_search_matches(&mut self) {
        let needle = match self.tabs[self.active_tab].search.as_ref() {
            Some(s) if !s.query.is_empty() => s.query.to_lowercase(),
            Some(_) => {
                if let Some(s) = self.tabs[self.active_tab].search.as_mut() {
                    s.matches.clear();
                    s.current = None;
                }
                self.status.message = "search: ".into();
                return;
            }
            None => return,
        };
        let matches = match self.tabs[self.active_tab].results.active_state() {
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
        let search = self.tabs[self.active_tab].search.as_mut().unwrap();
        let query = search.query.clone();
        search.matches = matches;
        search.current = if total == 0 { None } else { Some(0) };
        self.status.message = if total == 0 {
            format!("search: {query} · no matches")
        } else {
            format!("search: {query} · 1/{total}")
        };
    }

    fn advance_search(&mut self, delta: i32) {
        let Some(search) = self.tabs[self.active_tab].search.as_mut() else {
            return;
        };
        if search.matches.is_empty() {
            return;
        }
        let len = search.matches.len() as i32;
        let current = search.current.unwrap_or(0) as i32;
        let next = (current + delta).rem_euclid(len) as usize;
        search.current = Some(next);
        let total = search.matches.len();
        let query = search.query.clone();
        self.status.message = format!("search: {query} · {}/{}", next + 1, total);
        self.jump_to_current_match();
    }

    fn jump_to_current_match(&mut self) {
        let Some(search) = self.tabs[self.active_tab].search.as_ref() else {
            return;
        };
        let Some(idx) = search.current.and_then(|c| search.matches.get(c).copied()) else {
            return;
        };
        self.tabs[self.active_tab]
            .results
            .active_mut()
            .select(Some(idx));
    }

    fn open_cell_popup(&mut self) {
        let Some(row_index) = self.selected_original_row() else {
            self.status.message = "select a row first (j/k)".into();
            return;
        };
        let col_index = self.tabs[self.active_tab].results.active().column_index;
        let (columns, rows) = match self.tabs[self.active_tab].results.active_state() {
            ResultState::Rows { rows, columns, .. } => (columns, rows),
            ResultState::Running { rows, columns, .. } => (columns, rows),
            _ => return,
        };
        let Some(column) = columns.get(col_index) else {
            return;
        };
        let Some(row) = rows.get(row_index) else {
            return;
        };
        let Some(value) = row.0.get(col_index) else {
            return;
        };
        self.tabs[self.active_tab].results.active_mut().popup = Some(CellPopup {
            column_name: column.name.clone(),
            column_type: column.data_type.clone(),
            value_text: value.render(),
            row_index,
        });
    }

    // ----- row detail modal -----

    fn open_row_detail(&mut self) {
        let tab = &self.tabs[self.active_tab];
        // Don't open if another modal at the same layer is already open.
        if tab.row_detail.is_some() || tab.results.active().popup.is_some() || tab.editing.is_some()
        {
            return;
        }
        // Compute visible rows to map selected index → original row index.
        // This avoids depending on `visible_indices` being populated by
        // a prior render pass.
        let Some(vis_selected) = tab.results.active().selected() else {
            self.status.message = "no row selected".into();
            return;
        };
        let (columns, rows) = match tab.results.active_state() {
            ResultState::Rows { columns, rows, .. } => (columns.clone(), rows.clone()),
            ResultState::Running { columns, rows, .. } => (columns.clone(), rows.clone()),
            _ => {
                self.status.message = "no result to inspect".into();
                return;
            }
        };
        let visible = tab.results.active().visible_rows(&columns, &rows);
        let Some(&row_idx) = visible.get(vis_selected) else {
            self.status.message = "no row selected".into();
            return;
        };
        let Some(row) = rows.get(row_idx) else {
            return;
        };
        self.tabs[self.active_tab].row_detail = Some(RowDetailState {
            row_index: row_idx,
            columns,
            values: row.0.clone(),
            selected_column: 0,
            scroll_offset: 0,
        });
    }

    fn handle_row_detail_key(&mut self, key: KeyEvent) {
        let Some(state) = self.tabs[self.active_tab].row_detail.as_mut() else {
            return;
        };
        let col_count = state.columns.len().saturating_sub(1);
        match key.code {
            CtKey::Up | CtKey::Char('k') => {
                state.selected_column = state.selected_column.saturating_sub(1);
                state.scroll_offset = 0;
            }
            CtKey::Down | CtKey::Char('j') => {
                if state.selected_column < col_count {
                    state.selected_column += 1;
                }
                state.scroll_offset = 0;
            }
            CtKey::PageUp => {
                let page = 10usize; // approximate page size
                state.selected_column = state.selected_column.saturating_sub(page);
                state.scroll_offset = 0;
            }
            CtKey::PageDown => {
                let page = 10usize;
                state.selected_column = (state.selected_column + page).min(col_count);
                state.scroll_offset = 0;
            }
            CtKey::Char('g') => {
                state.selected_column = 0;
                state.scroll_offset = 0;
            }
            CtKey::Char('G') => {
                state.selected_column = col_count;
                state.scroll_offset = 0;
            }
            CtKey::Esc | CtKey::Char('R') => {
                self.tabs[self.active_tab].row_detail = None;
                self.status.message = "row detail closed".into();
            }
            CtKey::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.tabs[self.active_tab].row_detail = None;
                self.status.message = "row detail closed".into();
            }
            _ => {}
        }
    }
}
