//! Result-pane interactive operations extracted from `core.rs` (L21).
//!
//! Handles every key event that targets the active [`super::ResultBundle`]:
//! navigation, search, filtering, sort toggling, cell yank/edit, row
//! detail modal, and the row-popup overlay.
use crossterm::event::{KeyCode as CtKey, KeyEvent, KeyModifiers};
use narwhal_tui::{CellEditView, CellPopup, SortDir};

use super::{AppCore, CellEdit, JsonViewerState, ResultSearch, ResultState, RowDetailState};
use crate::action::{Action, KeyGroup};
use crate::keymap::KeyChord;
use narwhal_core::Value;

impl AppCore {
    pub(super) fn handle_results_key(&mut self, key: KeyEvent) {
        // Row detail modal: sits at the same layer as the cell popup.
        // When open, it intercepts navigation and dismiss keys.
        if self.ui.tabs[self.ui.active_tab].row_detail.is_some() {
            self.handle_row_detail_key(key);
            return;
        }
        if self.ui.tabs[self.ui.active_tab].editing.is_some() {
            self.handle_cell_edit_key(key);
            return;
        }
        if self.ui.tabs[self.ui.active_tab]
            .results
            .active()
            .popup
            .is_some()
        {
            if matches!(key.code, CtKey::Esc | CtKey::Char('q') | CtKey::Enter) {
                self.ui.tabs[self.ui.active_tab].results.active_mut().popup = None;
            }
            return;
        }
        // Filter prompt editing: modal — consumes keys before any
        // other result-pane handler.
        if self.ui.tabs[self.ui.active_tab]
            .results
            .active()
            .filter_prompt_open
        {
            match key.code {
                CtKey::Esc => {
                    let rv = self.ui.tabs[self.ui.active_tab].results.active_mut();
                    rv.filter.clear();
                    rv.filter_prompt_open = false;
                    self.ui.status.message = "filter cleared".into();
                }
                CtKey::Enter => {
                    let filter_text = self.ui.tabs[self.ui.active_tab]
                        .results
                        .active()
                        .filter
                        .clone();
                    self.ui.tabs[self.ui.active_tab]
                        .results
                        .active_mut()
                        .filter_prompt_open = false;
                    self.ui.status.message = if filter_text.is_empty() {
                        "filter closed".into()
                    } else {
                        format!("filter: {filter_text}")
                    };
                }
                CtKey::Backspace => {
                    self.ui.tabs[self.ui.active_tab]
                        .results
                        .active_mut()
                        .filter
                        .pop();
                }
                CtKey::Char(c) => {
                    self.ui.tabs[self.ui.active_tab]
                        .results
                        .active_mut()
                        .filter
                        .push(c);
                }
                _ => {}
            }
            return;
        }
        if let Some(search) = self.ui.tabs[self.ui.active_tab].search.as_mut() {
            if search.editing {
                match key.code {
                    CtKey::Esc => {
                        self.ui.tabs[self.ui.active_tab].search = None;
                        self.ui.status.message = "search cancelled".into();
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

        // L36: route the live chord through the configured keymap.
        // Unbound chords are silently dropped — the old hard-coded
        // match did the same, and falling through would risk shadowing
        // legitimate future bindings.
        let chord = KeyChord::from_event(key);
        if let Some(action) = self.deps.keymap.resolve(KeyGroup::Results, chord) {
            self.apply_results_action(action);
        }
    }

    /// Execute a single [`Action`] in the Results pane.
    ///
    /// Centralised so the dispatch layer (key events), tests, and any
    /// future plugin / macro layer all funnel through the same path.
    /// Each arm delegates to the existing helper so the diff with the
    /// pre-L36 code stays small.
    pub(super) fn apply_results_action(&mut self, action: Action) {
        // Recompute the visible row count up-front — several navigation
        // arms need it and computing inside each arm would force the
        // borrow checker to revalidate the `results` borrow.
        let (visible_count, col_count) =
            match self.ui.tabs[self.ui.active_tab].results.active_state() {
                ResultState::Rows { rows, columns, .. } => {
                    let vis = self.ui.tabs[self.ui.active_tab]
                        .results
                        .active()
                        .visible_rows(columns, rows);
                    (vis.len(), columns.len())
                }
                ResultState::Running { rows, columns, .. } => (rows.len(), columns.len()),
                _ => (0, 0),
            };

        match action {
            Action::ResultsMoveDown => self.ui.tabs[self.ui.active_tab]
                .results
                .active_mut()
                .move_down(visible_count),
            Action::ResultsMoveUp => {
                self.ui.tabs[self.ui.active_tab]
                    .results
                    .active_mut()
                    .move_up();
            }
            Action::ResultsMoveLeft => {
                self.ui.tabs[self.ui.active_tab]
                    .results
                    .active_mut()
                    .move_left();
            }
            Action::ResultsMoveRight => self.ui.tabs[self.ui.active_tab]
                .results
                .active_mut()
                .move_right(col_count),
            Action::ResultsFirstRow => self.ui.tabs[self.ui.active_tab]
                .results
                .active_mut()
                .select(Some(0)),
            Action::ResultsLastRow if visible_count > 0 => {
                self.ui.tabs[self.ui.active_tab]
                    .results
                    .active_mut()
                    .select(Some(visible_count - 1));
            }
            Action::ResultsLastRow => {}
            Action::ResultsToggleSort => self.toggle_sort(),
            Action::ResultsOpenFilterPrompt => self.open_filter_prompt(),
            Action::ResultsNextMatch => self.advance_search(1),
            Action::ResultsPrevMatch => self.advance_search(-1),
            Action::ResultsEscape => self.handle_results_escape(),
            Action::ResultsOpenCellPopup => self.open_cell_popup(),
            Action::ResultsOpenRowDetail => self.open_row_detail(),
            Action::ResultsStartCellEdit => self.start_cell_edit(),
            Action::ResultsYankCell => self.yank_cell(),
            Action::ResultsYankRow => self.yank_row(),
            Action::ResultsNextStatementLeader => {
                self.ui.pending_result_leader = Some(']');
            }
            Action::ResultsPrevStatementLeader => {
                self.ui.pending_result_leader = Some('[');
            }
            // ─── Row CRUD + Pending changes (L36) ────────────────────────────────
            Action::ResultsAppendRow => self.append_row(),
            Action::ResultsDuplicateRow => self.duplicate_row(),
            Action::ResultsDeleteRow => self.delete_row(),
            Action::ResultsCommitPending => self.commit_pending(),
            Action::ResultsDiscardPending => self.discard_pending(),
            Action::ResultsOpenPendingPreview => self.toggle_pending_preview(),
            // ─── Metadata tabs (L36) ────────────────────────────────
            Action::MetaTabRecords => self.switch_meta_tab(narwhal_tui::MetaTab::Records),
            Action::MetaTabColumns => self.switch_meta_tab(narwhal_tui::MetaTab::Columns),
            Action::MetaTabConstraints => {
                self.switch_meta_tab(narwhal_tui::MetaTab::Constraints);
            }
            Action::MetaTabForeignKeys => {
                self.switch_meta_tab(narwhal_tui::MetaTab::ForeignKeys);
            }
            Action::MetaTabIndexes => self.switch_meta_tab(narwhal_tui::MetaTab::Indexes),
            // ─── JSON viewer (L36) ──────────────────────────────────
            // `z` from a cell opens the viewer over the currently
            // selected cell. `Z` from the row-detail modal opens it
            // over the column the row-detail cursor is on; that arm
            // is routed through `handle_row_detail_key` below.
            Action::OpenJsonViewerCell => self.open_json_viewer_for_cell(),
            Action::OpenJsonViewerRow => {
                // Should never reach here — the chord lives in the
                // RowDetail group. We keep the arm explicit so the
                // exhaustive match below does not need a misleading
                // fallthrough message.
                self.ui.status.message = "open JSON from the row detail modal with `Z`".into();
            }
            // `Action` is `#[non_exhaustive]` so the compiler insists on
            // a fallthrough arm. Today the variants above are exhaustive;
            // the arm exists purely to forward-compatibly absorb new
            // additions without breaking compilation, and to keep status
            // bar feedback honest.
            other => {
                self.ui.status.message =
                    format!("action '{other:?}' not yet implemented in the Results pane");
            }
        }
    }

    /// Switch the metadata sub-view shown in the Results pane.
    ///
    /// Behaviour depends on what's currently on screen:
    ///
    /// - **Already in `TableDetail`:** swap `active_meta_tab` in place
    ///   for `Columns`/`Constraints`/`ForeignKeys`/`Indexes`, or run a
    ///   preview query for `Records`.
    /// - **In a `Rows` preview** (sidebar pressed `o` or the user came
    ///   from `Records`): targeting `Columns`/etc. re-describes the
    ///   table and lands back in `TableDetail`; targeting `Records`
    ///   is a no-op (already there).
    /// - **Neither:** the action becomes a status hint so the keypress
    ///   never silently does nothing.
    pub(super) fn switch_meta_tab(&mut self, target: narwhal_tui::MetaTab) {
        // Snapshot the current table identity (if any) before borrowing.
        let (current_schema, current_table, in_table_detail) = {
            let tab = &self.ui.tabs[self.ui.active_tab];
            match tab.results.active_state() {
                ResultState::TableDetail { schema, .. } => {
                    (schema.table.schema.clone(), schema.table.name.clone(), true)
                }
                ResultState::Rows {
                    source: Some(src), ..
                } => (src.schema.clone(), src.table.clone(), false),
                _ => {
                    self.ui.status.message = format!(
                        "tab '{}' needs a table on screen — open one from the sidebar first",
                        target.label()
                    );
                    return;
                }
            }
        };

        match target {
            narwhal_tui::MetaTab::Records => {
                // Records = paged preview. If we're already in a Rows
                // preview, no-op; otherwise dispatch a fresh preview.
                if in_table_detail {
                    self.run_preview(&current_schema, &current_table, 0);
                } else {
                    self.ui.status.message = "already on Records".into();
                }
            }
            other => {
                if in_table_detail {
                    // Cheap in-place mutation; the schema payload stays.
                    if let ResultState::TableDetail {
                        active_meta_tab, ..
                    } = self.ui.tabs[self.ui.active_tab].results.active_state_mut()
                    {
                        *active_meta_tab = other;
                    }
                    self.ui.status.message = format!("meta tab → {}", other.label());
                } else {
                    // Coming from a Rows preview: re-describe the table
                    // and land in the requested sub-view. The describe
                    // helper resets the state to `TableDetail` with
                    // `Columns` as the default, so we overwrite the
                    // active tab afterwards.
                    self.describe_table_into_result(&current_schema, &current_table);
                    if let ResultState::TableDetail {
                        active_meta_tab, ..
                    } = self.ui.tabs[self.ui.active_tab].results.active_state_mut()
                    {
                        *active_meta_tab = other;
                    }
                    self.ui.status.message = format!("meta tab → {}", other.label());
                }
            }
        }
    }

    /// Format `value` as pretty-printed JSON when it parses, or fall
    /// back to the raw text on failure. Returns the rendered text and
    /// an optional parse-error string the modal surfaces in its footer.
    fn prettify_json(raw: &str) -> (String, Option<String>) {
        match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(v) => match serde_json::to_string_pretty(&v) {
                Ok(s) => (s, None),
                Err(e) => (raw.to_owned(), Some(e.to_string())),
            },
            Err(e) => (raw.to_owned(), Some(e.to_string())),
        }
    }

    /// Open the JSON viewer over the cell currently focused in the
    /// result pane. Always opens — the modal will surface a parse
    /// error in its footer when the cell value is not legal JSON, so
    /// the user still gets pretty-printed-on-best-effort behaviour for
    /// quasi-JSON text.
    pub(super) fn open_json_viewer_for_cell(&mut self) {
        // `selected_original_row` only resolves once the table has been
        // rendered at least once (it consults the cached
        // `visible_indices`). Headless tests skip the render pass, so
        // we mirror the `cell_edit` fallback of `unwrap_or(0)` — a
        // missing mapping means there is no active filter/sort, in
        // which case row 0 is the natural target anyway.
        let row_idx = self.selected_original_row().unwrap_or(0);
        let (column_name, column_type, raw) = {
            let tab = &self.ui.tabs[self.ui.active_tab];
            let col_idx = tab.results.active().column_index;
            let (columns, rows) = match tab.results.active_state() {
                ResultState::Rows { columns, rows, .. }
                | ResultState::Running { columns, rows, .. } => (columns, rows),
                _ => {
                    self.ui.status.message = "no JSON cell here".into();
                    return;
                }
            };
            let Some(column) = columns.get(col_idx) else {
                self.ui.status.message = "select a column first (h/l)".into();
                return;
            };
            let Some(row) = rows.get(row_idx) else {
                self.ui.status.message = "no row selected".into();
                return;
            };
            let raw_text = match row.0.get(col_idx) {
                Some(Value::Null) | None => String::new(),
                Some(v) => v.render(),
            };
            (column.name.clone(), column.data_type.clone(), raw_text)
        };
        if raw.is_empty() {
            self.ui.status.message = "cell is NULL or empty — nothing to view".into();
            return;
        }
        let (pretty, parse_error) = Self::prettify_json(&raw);
        let title = format!("{column_name} ({column_type})");
        self.ui.tabs[self.ui.active_tab].json_viewer = Some(JsonViewerState {
            title,
            pretty,
            raw,
            scroll: 0,
            parse_error,
        });
        self.ui.status.message = "JSON viewer: j/k scroll · y copy · q close".into();
    }

    /// Open the JSON viewer from inside the row-detail modal: the
    /// active column there is the source. Called from
    /// `handle_row_detail_key` when the `OpenJsonViewerRow` action
    /// resolves.
    pub(super) fn open_json_viewer_from_row_detail(&mut self) {
        let Some(state) = self.ui.tabs[self.ui.active_tab].row_detail.as_ref() else {
            return;
        };
        let Some(column) = state.columns.get(state.selected_column) else {
            self.ui.status.message = "select a column first (j/k)".into();
            return;
        };
        let raw = match state.values.get(state.selected_column) {
            Some(Value::Null) | None => String::new(),
            Some(v) => v.render(),
        };
        if raw.is_empty() {
            self.ui.status.message = "cell is NULL or empty — nothing to view".into();
            return;
        }
        let title = format!("{} ({})", column.name, column.data_type);
        let (pretty, parse_error) = Self::prettify_json(&raw);
        self.ui.tabs[self.ui.active_tab].json_viewer = Some(JsonViewerState {
            title,
            pretty,
            raw,
            scroll: 0,
            parse_error,
        });
        self.ui.status.message = "JSON viewer: j/k scroll · y copy · q close".into();
    }

    /// Modal handler for the JSON viewer. Owns its own key vocabulary;
    /// the configured keymap is *not* consulted here because the modal
    /// has hard-coded reflexes (`q`/`Esc` always closes, etc.) that the
    /// user cannot reasonably want to unbind.
    pub(super) fn handle_json_viewer_key(&mut self, key: KeyEvent) {
        let active = self.ui.active_tab;
        let Some(state) = self.ui.tabs[active].json_viewer.as_mut() else {
            return;
        };
        let total_lines = state.pretty.lines().count() as u16;
        let max_scroll = total_lines.saturating_sub(1);
        match key.code {
            CtKey::Esc | CtKey::Char('q') => {
                self.ui.tabs[active].json_viewer = None;
                self.ui.status.message = "JSON viewer closed".into();
            }
            CtKey::Char('j') | CtKey::Down => {
                state.scroll = state.scroll.saturating_add(1).min(max_scroll);
            }
            CtKey::Char('k') | CtKey::Up => {
                state.scroll = state.scroll.saturating_sub(1);
            }
            CtKey::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                state.scroll = state.scroll.saturating_add(10).min(max_scroll);
            }
            CtKey::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                state.scroll = state.scroll.saturating_sub(10);
            }
            CtKey::Char('g') => state.scroll = 0,
            CtKey::Char('G') => state.scroll = max_scroll,
            CtKey::Char('y') => {
                let text = state.pretty.clone();
                let len = text.len();
                match self.deps.clipboard.set_text(&text) {
                    Ok(()) => {
                        self.ui.status.message = format!("yanked {len} char(s) (pretty)");
                    }
                    Err(e) => self.ui.status.message = format!("yank failed: {e}"),
                }
            }
            CtKey::Char('Y') => {
                let text = state.raw.clone();
                let len = text.len();
                match self.deps.clipboard.set_text(&text) {
                    Ok(()) => {
                        self.ui.status.message = format!("yanked {len} char(s) (raw)");
                    }
                    Err(e) => self.ui.status.message = format!("yank failed: {e}"),
                }
            }
            _ => {}
        }
    }

    fn handle_results_escape(&mut self) {
        let had_search = self.ui.tabs[self.ui.active_tab].search.take().is_some();
        let had_filter = !self.ui.tabs[self.ui.active_tab]
            .results
            .active()
            .filter
            .is_empty();
        if had_search {
            self.ui.status.message = "search cleared".into();
        }
        if had_filter {
            let rv = self.ui.tabs[self.ui.active_tab].results.active_mut();
            rv.filter.clear();
            rv.filter_prompt_open = false;
            self.ui.status.message = "filter cleared".into();
        }
    }

    /// Translate the current `TableState` selection (which is an index
    /// into the visible/rendered rows) to the original row index in
    /// the full result set. Returns `None` when there are no rows.
    fn selected_original_row(&self) -> Option<usize> {
        let tab = &self.ui.tabs[self.ui.active_tab];
        let vis_selected = tab.results.active().selected()?;
        tab.results
            .active()
            .visible_indices
            .get(vis_selected)
            .copied()
    }

    fn yank_cell(&mut self) {
        let tab = &self.ui.tabs[self.ui.active_tab];
        let (rows, _columns) = match tab.results.active_state() {
            ResultState::Rows { rows, columns, .. }
            | ResultState::Running { rows, columns, .. } => (rows, columns),
            _ => {
                self.ui.status.message = "no cell to yank".into();
                return;
            }
        };
        let row_idx = self.selected_original_row().unwrap_or(0);
        let col_idx = tab.results.active().column_index;
        let Some(value) = rows.get(row_idx).and_then(|r| r.0.get(col_idx)) else {
            self.ui.status.message = "no cell selected".into();
            return;
        };
        let text = match value {
            narwhal_core::Value::Null => String::new(),
            other => other.render(),
        };
        match self.deps.clipboard.set_text(&text) {
            Ok(()) => {
                self.ui.status.message = format!("yanked {} char(s) to clipboard", text.len());
            }
            Err(error) => {
                self.ui.status.message = format!("yank failed: {error}");
            }
        }
    }

    fn yank_row(&mut self) {
        let tab = &self.ui.tabs[self.ui.active_tab];
        let rows = match tab.results.active_state() {
            ResultState::Rows { rows, .. } | ResultState::Running { rows, .. } => rows,
            _ => {
                self.ui.status.message = "no row to yank".into();
                return;
            }
        };
        let row_idx = self.selected_original_row().unwrap_or(0);
        let Some(row) = rows.get(row_idx) else {
            self.ui.status.message = "no row selected".into();
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
        match self.deps.clipboard.set_text(&text) {
            Ok(()) => {
                self.ui.status.message =
                    format!("yanked row ({} cell(s)) to clipboard", row.0.len());
            }
            Err(error) => {
                self.ui.status.message = format!("yank failed: {error}");
            }
        }
    }

    fn start_cell_edit(&mut self) {
        // Gather the data we need by value first, then mutate.
        let prepared = {
            let tab = &self.ui.tabs[self.ui.active_tab];
            let (columns, rows, source) = match tab.results.active_state() {
                ResultState::Rows {
                    columns,
                    rows,
                    source: Some(source),
                    ..
                } => (columns, rows, source),
                ResultState::Rows { source: None, .. } => {
                    self.ui.status.message =
                        "this result is read-only (no row source); preview a table to edit".into();
                    return;
                }
                _ => {
                    self.ui.status.message = "no editable cell here".into();
                    return;
                }
            };
            if columns.is_empty() || rows.is_empty() {
                self.ui.status.message = "no rows to edit".into();
                return;
            }
            if !source.columns.iter().any(|c| c.primary_key) {
                self.ui.status.message =
                    format!("{}: no primary key, cell edits are disabled", source.table);
                return;
            }
            let row_index = self.selected_original_row().unwrap_or(0);
            let col_index = tab.results.active().column_index;
            let Some(row) = rows.get(row_index) else {
                self.ui.status.message = "select a row first (j/k)".into();
                return;
            };
            let Some(column) = columns.get(col_index) else {
                self.ui.status.message = "select a column first (h/l)".into();
                return;
            };
            let cell = row.0.get(col_index);
            let original = cell.map(narwhal_core::Value::render).unwrap_or_default();
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
        let tab = &mut self.ui.tabs[self.ui.active_tab];
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
        self.ui.status.message = "edit: Enter saves · Esc cancels".into();
    }

    fn handle_cell_edit_key(&mut self, key: KeyEvent) {
        let Some(edit) = self.ui.tabs[self.ui.active_tab].editing.as_mut() else {
            return;
        };
        match key.code {
            CtKey::Esc => {
                self.ui.tabs[self.ui.active_tab].editing = None;
                self.ui.tabs[self.ui.active_tab].results.active_mut().edit = None;
                self.ui.status.message = "edit cancelled".into();
            }
            // L36: cell edit no longer touches the database directly.
            // The Enter key queues the change so the user can review it
            // alongside any insert/delete in the pending preview before
            // committing with Ctrl-S.
            CtKey::Enter => self.queue_cell_edit_commit(),
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
        let tab = &mut self.ui.tabs[self.ui.active_tab];
        if let (Some(edit), Some(view)) =
            (tab.editing.as_ref(), tab.results.active_mut().edit.as_mut())
        {
            view.buffer = edit.buffer.clone();
            view.error = None;
        }
    }

    // L36: commit_cell_edit was removed when cell edits became staged
    // mutations — see `queue_cell_edit_commit` on the pending_actions
    // impl. The optimistic UPDATE generator now lives in
    // `narwhal_commands::pending::compile`.

    pub(super) fn set_edit_error(&mut self, message: String) {
        if let Some(view) = self.ui.tabs[self.ui.active_tab]
            .results
            .active_mut()
            .edit
            .as_mut()
        {
            view.error = Some(message.clone());
        }
        self.ui.status.message = format!("edit failed: {message}");
    }

    #[allow(dead_code)]
    fn start_search(&mut self) {
        if !matches!(
            self.ui.tabs[self.ui.active_tab].results.active_state(),
            ResultState::Rows { .. } | ResultState::Running { .. }
        ) {
            self.ui.status.message = "no result to search".into();
            return;
        }
        self.ui.tabs[self.ui.active_tab].search = Some(ResultSearch {
            query: String::new(),
            matches: Vec::new(),
            current: None,
            editing: true,
        });
        self.ui.status.message = "search: ".into();
    }

    pub(super) fn toggle_sort(&mut self) {
        // Streaming guard.
        if self.process.running {
            self.ui.status.message = "sort/filter unavailable while streaming".into();
            return;
        }
        if !matches!(
            self.ui.tabs[self.ui.active_tab].results.active_state(),
            ResultState::Rows { .. }
        ) {
            self.ui.status.message = "no result to sort".into();
            return;
        }
        let col = self.ui.tabs[self.ui.active_tab]
            .results
            .active()
            .column_index;
        let view = self.ui.tabs[self.ui.active_tab].results.active_mut();
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
        self.ui.status.message = msg;
    }

    fn open_filter_prompt(&mut self) {
        // Streaming guard.
        if self.process.running {
            self.ui.status.message = "sort/filter unavailable while streaming".into();
            return;
        }
        if !matches!(
            self.ui.tabs[self.ui.active_tab].results.active_state(),
            ResultState::Rows { .. }
        ) {
            self.ui.status.message = "no result to filter".into();
            return;
        }
        self.ui.tabs[self.ui.active_tab]
            .results
            .active_mut()
            .filter_prompt_open = true;
        self.ui.status.message = "filter: type to filter, Enter accepts, Esc clears".into();
    }

    fn refresh_search_matches(&mut self) {
        let needle = match self.ui.tabs[self.ui.active_tab].search.as_ref() {
            Some(s) if !s.query.is_empty() => s.query.to_lowercase(),
            Some(_) => {
                if let Some(s) = self.ui.tabs[self.ui.active_tab].search.as_mut() {
                    s.matches.clear();
                    s.current = None;
                }
                self.ui.status.message = "search: ".into();
                return;
            }
            None => return,
        };
        let matches = match self.ui.tabs[self.ui.active_tab].results.active_state() {
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
        let Some(search) = self.ui.tabs[self.ui.active_tab].search.as_mut() else {
            return;
        };
        let query = search.query.clone();
        search.matches = matches;
        search.current = if total == 0 { None } else { Some(0) };
        self.ui.status.message = if total == 0 {
            format!("search: {query} · no matches")
        } else {
            format!("search: {query} · 1/{total}")
        };
    }

    fn advance_search(&mut self, delta: i32) {
        let Some(search) = self.ui.tabs[self.ui.active_tab].search.as_mut() else {
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
        self.ui.status.message = format!("search: {query} · {}/{}", next + 1, total);
        self.jump_to_current_match();
    }

    fn jump_to_current_match(&mut self) {
        let Some(search) = self.ui.tabs[self.ui.active_tab].search.as_ref() else {
            return;
        };
        let Some(idx) = search.current.and_then(|c| search.matches.get(c).copied()) else {
            return;
        };
        self.ui.tabs[self.ui.active_tab]
            .results
            .active_mut()
            .select(Some(idx));
    }

    fn open_cell_popup(&mut self) {
        let Some(row_index) = self.selected_original_row() else {
            self.ui.status.message = "select a row first (j/k)".into();
            return;
        };
        let col_index = self.ui.tabs[self.ui.active_tab]
            .results
            .active()
            .column_index;
        let (columns, rows) = match self.ui.tabs[self.ui.active_tab].results.active_state() {
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
        self.ui.tabs[self.ui.active_tab].results.active_mut().popup = Some(CellPopup {
            column_name: column.name.clone(),
            column_type: column.data_type.clone(),
            value_text: value.render(),
            row_index,
        });
    }

    fn open_row_detail(&mut self) {
        let tab = &self.ui.tabs[self.ui.active_tab];
        // Don't open if another modal at the same layer is already open.
        if tab.row_detail.is_some() || tab.results.active().popup.is_some() || tab.editing.is_some()
        {
            return;
        }
        // Compute visible rows to map selected index → original row index.
        // This avoids depending on `visible_indices` being populated by
        // a prior render pass.
        let Some(vis_selected) = tab.results.active().selected() else {
            self.ui.status.message = "no row selected".into();
            return;
        };
        let (columns, rows) = match tab.results.active_state() {
            ResultState::Rows { columns, rows, .. } => (columns.clone(), rows.clone()),
            ResultState::Running { columns, rows, .. } => (columns.clone(), rows.clone()),
            _ => {
                self.ui.status.message = "no result to inspect".into();
                return;
            }
        };
        let visible = tab.results.active().visible_rows(&columns, &rows);
        let Some(&row_idx) = visible.get(vis_selected) else {
            self.ui.status.message = "no row selected".into();
            return;
        };
        let Some(row) = rows.get(row_idx) else {
            return;
        };
        self.ui.tabs[self.ui.active_tab].row_detail = Some(RowDetailState {
            row_index: row_idx,
            columns,
            values: row.0.clone(),
            selected_column: 0,
            scroll_offset: 0,
        });
    }

    fn handle_row_detail_key(&mut self, key: KeyEvent) {
        // L36: try the configured keymap first for chords that
        // pre-empt the modal's reflexes (today only `Z` to launch the
        // JSON viewer over the focused column).
        let chord = KeyChord::from_event(key);
        if self.deps.keymap.resolve(KeyGroup::RowDetail, chord) == Some(Action::OpenJsonViewerRow) {
            self.open_json_viewer_from_row_detail();
            return;
        }
        let Some(state) = self.ui.tabs[self.ui.active_tab].row_detail.as_mut() else {
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
                self.ui.tabs[self.ui.active_tab].row_detail = None;
                self.ui.status.message = "row detail closed".into();
            }
            CtKey::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.ui.tabs[self.ui.active_tab].row_detail = None;
                self.ui.status.message = "row detail closed".into();
            }
            _ => {}
        }
    }
}
