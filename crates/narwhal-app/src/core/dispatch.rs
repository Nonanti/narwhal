//! `AppCore` top-level dispatch: render, key/mouse handling, the
//! `:`-prompt command parser, snippet insertion.

use crossterm::event::{KeyCode as CtKey, KeyEvent};
use narwhal_tui::{
    render_help_modal, render_history_modal, render_root, render_row_detail, render_snippets_modal,
    render_wizard, CompletionItemView, CompletionPopupView, EditorSearchHighlight,
    HistoryModalState, HistoryRow, HistoryRowOutcome, Pane, RootLayout, RowDetailView,
    SearchHighlight, SidebarRow, SidebarView, SnippetsModalState, StatusBarView, WizardFieldView,
    WizardView,
};
use ratatui::layout::Rect;
use ratatui::Frame;

use super::render_helpers::{display_from_state, sidebar_depth, sidebar_kind, sidebar_label};
use super::text_utils::split_head_arg;
use super::{AppCore, ResultState};
use crate::commands::{parse, Command};
use crate::completion::CompletionKind;
use crate::run::RunMode;
use crate::wizard::DRIVERS;

impl AppCore {
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let labels: Vec<String> = self.sidebar_items.iter().map(sidebar_label).collect();
        let rows: Vec<SidebarRow<'_>> = self
            .sidebar_items
            .iter()
            .zip(labels.iter())
            .map(|(item, label)| SidebarRow {
                depth: sidebar_depth(item),
                kind: sidebar_kind(item),
                label: label.as_str(),
            })
            .collect();
        // L24: pre-clamp the scroll offset against the last known
        // sidebar viewport so the cached `sidebar_scroll` we keep around
        // (for the next click handler / snapshot test) is always
        // consistent with what the renderer is about to draw. The
        // renderer itself also clamps, but doing it here too keeps the
        // host's view of the world honest.
        let visible = SidebarView::visible_rows(self.last_layout.sidebar.height.saturating_sub(2));
        self.sidebar_scroll =
            SidebarView::clamp_scroll(self.sidebar_index, self.sidebar_scroll, visible, rows.len());
        let sidebar_view = SidebarView {
            items: &rows,
            selected_index: self.sidebar_index,
            scroll_offset: self.sidebar_scroll,
            focused: self.focus == Pane::Sidebar,
        };
        let editor_title = self.editor_title_with_tabs();
        // Read pending count before the mutable borrow below.
        let pending_count = self.tabs[self.active_tab].pending.len();

        let tab = &mut self.tabs[self.active_tab];
        let search_view = tab.search.as_ref().map(|s| SearchHighlight {
            matches: &s.matches,
            current: s.current,
        });
        // Extract result state and view via the active index to avoid
        // overlapping borrows on `tab.results`.
        let active_idx = tab.results.active;
        let result_display =
            display_from_state(&tab.results.states[active_idx], search_view.as_ref());
        let completion_item_views: Vec<CompletionItemView<'_>> = tab
            .completion
            .as_ref()
            .map(|s| {
                s.items
                    .iter()
                    .map(|c| CompletionItemView {
                        text: c.text.as_str(),
                        kind_glyph: match c.kind {
                            CompletionKind::Keyword => "K",
                            CompletionKind::Table => "T",
                            CompletionKind::Column => "C",
                            CompletionKind::Function => "ƒ",
                        },
                        detail: c.detail.as_deref(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let completion_view = tab.completion.as_ref().map(|s| CompletionPopupView {
            items: &completion_item_views,
            selected: s.selected,
            anchor: (0, 0), // overwritten by render_root once it knows the editor rect
        });
        let editor_search_view =
            if tab.editor_search.highlight && !tab.editor_search.needle.is_empty() {
                Some(EditorSearchHighlight {
                    matches: &tab.editor_search.matches,
                    needle_len: tab.editor_search.needle.len(),
                    current: tab.editor_search.current,
                })
            } else {
                None
            };
        let result_count = tab.results.len();
        let mut layout = RootLayout {
            mode: self.vim.mode(),
            focus: self.focus,
            status_bar: StatusBarView {
                connection: self.status.connection.as_deref(),
                message: &self.status.message,
                transaction: self.status.transaction.as_deref(),
                pending: Some(pending_count),
                read_only: self.read_only,
            },
            running: self.running,
            theme: &self.theme,
            sidebar: sidebar_view,
            editor: &mut tab.editor,
            editor_title: &editor_title,
            result_view: &mut tab.results.views[active_idx],
            result: result_display,
            completion: completion_view,
            editor_search: editor_search_view,
            result_count,
            active_result: active_idx,
        };
        self.last_layout = render_root(frame, area, &mut layout);

        if let Some(wizard) = self.wizard.as_ref() {
            let view = WizardView {
                drivers: DRIVERS,
                driver_index: wizard.driver_index,
                fields: wizard
                    .fields
                    .iter()
                    .map(|f| WizardFieldView {
                        label: f.label,
                        value: f.value.expose(),
                        secret: f.secret,
                    })
                    .collect(),
                focused: wizard.focused,
                error: self.wizard_error.as_deref(),
            };
            render_wizard(frame, area, &view, &self.theme);
        }

        if self.help_open {
            render_help_modal(frame, area, &self.theme);
        }

        if let Some(state) = self.history_state.as_ref() {
            // Pre-format every per-row string into one owned tuple so
            // the borrowed view can reference stable storage.
            // Tuple layout: (timestamp, connection, sql, elapsed, rows,
            // outcome). Output strings are short and built once per
            // render — fine for a modal that only opens on demand.
            let visible_data: Vec<(String, String, String, String, String, HistoryRowOutcome)> =
                state
                    .visible_entries()
                    .iter()
                    .map(|e| {
                        let ts = e.timestamp.format("%Y-%m-%d %H:%M:%S").to_string();
                        let conn = e.connection_name.as_deref().unwrap_or("<local>").to_owned();
                        let elapsed = format_elapsed(e.elapsed_ms);
                        let rows = format_rows(e.rows_returned, e.rows_affected);
                        let outcome = match e.outcome {
                            narwhal_history::Outcome::Success => HistoryRowOutcome::Success,
                            narwhal_history::Outcome::Cancelled => HistoryRowOutcome::Cancelled,
                            narwhal_history::Outcome::Failed => HistoryRowOutcome::Failed,
                            // Forward-compat: any future outcome
                            // variant renders as the cautious yellow
                            // "cancelled" glyph until classified.
                            _ => HistoryRowOutcome::Cancelled,
                        };
                        (ts, conn, e.sql.clone(), elapsed, rows, outcome)
                    })
                    .collect();
            let modal_state = HistoryModalState {
                total: state.entries.len(),
                visible: visible_data
                    .iter()
                    .map(|(ts, conn, sql, elapsed, rows, outcome)| HistoryRow {
                        timestamp: ts.as_str(),
                        connection: conn.as_str(),
                        sql: sql.as_str(),
                        outcome: *outcome,
                        elapsed: elapsed.as_str(),
                        rows: rows.as_str(),
                    })
                    .collect(),
                filter: &state.filter,
                selected: state.selected,
            };
            render_history_modal(frame, area, &modal_state, &self.theme);
        }

        // Snippets modal.
        if let Some(modal) = self.snippets_modal.as_ref() {
            let modal_state = SnippetsModalState {
                entries: modal.entries.iter().map(String::as_str).collect(),
                selected: modal.selected,
            };
            render_snippets_modal(frame, area, &modal_state, &self.theme);
        }

        // Row detail modal — same layer as cell popup, rendered on
        // top of the result pane.
        if let Some(state) = self.tabs[self.active_tab].row_detail.as_ref() {
            let view = RowDetailView {
                columns: &state.columns,
                values: &state.values,
                selected_column: state.selected_column,
                scroll_offset: state.scroll_offset,
                row_index: state.row_index,
            };
            render_row_detail(frame, area, &view, &self.theme);
        }

        // Pending-changes preview (L36) — stacks above the result
        // pane but below the JSON viewer (which is the very top layer).
        if self.tabs[self.active_tab].pending_preview.is_some() {
            let mutations: Vec<String> = self.tabs[self.active_tab]
                .pending
                .iter()
                .map(crate::pending::PendingMutation::summary)
                .collect();
            let scroll = self.tabs[self.active_tab]
                .pending_preview
                .as_ref()
                .map_or(0, |s| s.scroll);
            let view = narwhal_tui::PendingPreviewView {
                mutations: &mutations,
                scroll,
            };
            narwhal_tui::render_pending_preview(frame, area, &view, &self.theme);
        }

        // JSON viewer (L36) — stacks above every other overlay so it
        // can be opened from the cell popup *or* from inside the row
        // detail modal.
        if let Some(state) = self.tabs[self.active_tab].json_viewer.as_ref() {
            let view = narwhal_tui::JsonViewerView {
                title: &state.title,
                pretty: &state.pretty,
                raw: &state.raw,
                scroll: state.scroll,
                parse_error: state.parse_error.as_deref(),
            };
            narwhal_tui::render_json_viewer(frame, area, &view, &self.theme);
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if self.wizard.is_some() {
            self.handle_wizard_key(key);
            return;
        }
        // L36: JSON viewer sits at the very top of the modal stack and
        // gets first refusal on every key. Once open, no other handler
        // (help, history, wizard, ...) sees the keypress.
        if self.tabs[self.active_tab].json_viewer.is_some() {
            self.handle_json_viewer_key(key);
            return;
        }
        // L36: pending preview modal is the next layer down. Owns its
        // own scroll vocabulary; commit/discard/close are forwarded to
        // the regular Results pane handlers so users can keep their
        // muscle memory.
        if self.tabs[self.active_tab].pending_preview.is_some() {
            self.handle_pending_preview_key(key);
            return;
        }
        // When the help modal is open, it intercepts Esc / ? / F1 to
        // close and silently consumes every other key so the user
        // doesn't accidentally trigger an action behind the overlay.
        if self.help_open {
            match key.code {
                CtKey::Esc | CtKey::F(1) => {
                    self.help_open = false;
                }
                CtKey::Char('?') if key.modifiers.is_empty() => {
                    self.help_open = false;
                }
                _ => {
                    // consumed but no-op
                }
            }
            return;
        }
        // When the history modal is open, it intercepts all keys.
        if self.history_state.is_some() {
            self.handle_history_key(key);
            return;
        }
        // When the snippets modal is open, it intercepts all keys.
        if self.snippets_modal.is_some() {
            self.handle_snippets_key(key);
            return;
        }
        if self.handle_global_key(key) {
            return;
        }
        // Pending result-tab leader: `]` or `[` was pressed, waiting
        // for `r` to complete the sequence. Any other key cancels.
        if let Some(leader) = self.pending_result_leader.take() {
            if key.code == CtKey::Char('r') && key.modifiers.is_empty() {
                match leader {
                    ']' => self.cycle_result_tab(1),
                    '[' => self.cycle_result_tab(-1),
                    _ => {}
                }
            }
            return;
        }
        match self.focus {
            Pane::Editor => self.handle_editor_key(key),
            Pane::Sidebar => self.handle_sidebar_key(key),
            Pane::Results => self.handle_results_key(key),
            // Future panes fall through to the editor handler until wired.
            _ => self.handle_editor_key(key),
        }
    }

    /// Route a crossterm `MouseEvent` through the same handlers the
    /// keyboard path uses. `LayoutRegions` from the most recent render
    /// provides the hit-test rects.
    pub fn handle_mouse(&mut self, event: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};

        let pos = (event.column, event.row);

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.handle_left_click(pos);
            }
            MouseEventKind::ScrollUp => {
                self.handle_scroll(pos, -1);
            }
            MouseEventKind::ScrollDown => {
                self.handle_scroll(pos, 1);
            }
            // Up, Moved, Drag are no-ops for now.
            _ => {}
        }
    }

    fn handle_left_click(&mut self, pos: (u16, u16)) {
        let layout = self.last_layout.clone();

        // Priority: completion popup > sidebar tables > result headers/rows > pane focus.
        for (rect, item_index) in &layout.completion_items {
            if rect.contains(ratatui::layout::Position::new(pos.0, pos.1)) {
                self.accept_completion_at(*item_index);
                return;
            }
        }

        for (rect, sidebar_idx) in &layout.sidebar_tables {
            if rect.contains(ratatui::layout::Position::new(pos.0, pos.1)) {
                self.click_sidebar_table(*sidebar_idx);
                return;
            }
        }

        for (rect, result_idx) in &layout.result_tabs {
            if rect.contains(ratatui::layout::Position::new(pos.0, pos.1)) {
                self.click_result_tab(*result_idx);
                return;
            }
        }

        for (rect, col_idx) in &layout.result_headers {
            if rect.contains(ratatui::layout::Position::new(pos.0, pos.1)) {
                // Sort cycle action: move column focus and toggle sort.
                self.tabs[self.active_tab].results.active_mut().column_index = *col_idx;
                self.toggle_sort();
                return;
            }
        }

        for (rect, row_idx) in &layout.result_rows {
            if rect.contains(ratatui::layout::Position::new(pos.0, pos.1)) {
                self.tabs[self.active_tab]
                    .results
                    .active_mut()
                    .select(Some(*row_idx));
                self.focus = Pane::Results;
                self.status.message = format!("focus → {}", Pane::Results.label());
                return;
            }
        }

        // Fall through to pane focus change.
        if layout
            .sidebar
            .contains(ratatui::layout::Position::new(pos.0, pos.1))
        {
            self.focus = Pane::Sidebar;
            self.status.message = format!("focus → {}", Pane::Sidebar.label());
        } else if layout
            .editor
            .contains(ratatui::layout::Position::new(pos.0, pos.1))
        {
            self.focus = Pane::Editor;
            self.status.message = format!("focus → {}", Pane::Editor.label());
        } else if layout
            .results
            .contains(ratatui::layout::Position::new(pos.0, pos.1))
        {
            self.focus = Pane::Results;
            self.status.message = format!("focus → {}", Pane::Results.label());
        }
    }

    fn handle_scroll(&mut self, pos: (u16, u16), delta: i32) {
        let layout = &self.last_layout;

        if layout
            .results
            .contains(ratatui::layout::Position::new(pos.0, pos.1))
        {
            let row_count = match self.tabs[self.active_tab].results.active_state() {
                ResultState::Rows { rows, .. } | ResultState::Running { rows, .. } => rows.len(),
                _ => return,
            };
            if delta > 0 {
                self.tabs[self.active_tab]
                    .results
                    .active_mut()
                    .move_down(row_count);
            } else {
                self.tabs[self.active_tab].results.active_mut().move_up();
            }
        } else if layout
            .editor
            .contains(ratatui::layout::Position::new(pos.0, pos.1))
        {
            // Editor scroll: move cursor line offset without changing column.
            let height = layout.editor.height.saturating_sub(2) as usize; // subtract borders
            if height == 0 {
                return;
            }
            let buf = &mut self.tabs[self.active_tab].editor;
            if delta > 0 {
                // Scroll down: move cursor down
                buf.apply_motion(narwhal_vim::Motion::Down, 1);
                buf.ensure_visible(height);
            } else {
                buf.apply_motion(narwhal_vim::Motion::Up, 1);
                buf.ensure_visible(height);
            }
        } else if layout
            .sidebar
            .contains(ratatui::layout::Position::new(pos.0, pos.1))
        {
            // L24: mouse wheel over the sidebar pans the viewport by
            // 3 rows per tick. The selection stays put so the user can
            // peek at off-screen rows without losing context.
            self.scroll_sidebar(if delta > 0 { 3 } else { -3 });
        }
    }

    // accept_completion_at, handle_global_key, handle_editor_key, column_cache,
    // maybe_auto_complete, open_editor_search, handle_editor_search_key,
    // refresh_editor_search_matches, jump_to_editor_search_match,
    // sync_editor_search_current, repeat_editor_search, execute_substitute,
    // trigger_completion, handle_completion_key, apply_action, complete_prompt
    // moved to `core::editor_dispatch`.

    /// Execute a command exactly as if the user submitted it from command-line
    /// mode. Useful from tests.
    pub fn execute_command(&mut self, raw: &str) {
        match parse(raw) {
            Command::Quit => self.should_quit = true,
            Command::Open(name) => self.open_named(&name),
            Command::Close => self.close_session(),
            Command::Refresh => self.refresh_schema(),
            Command::Run => self.dispatch_current_statement(RunMode::Execute),
            Command::RunAll => self.dispatch_all_statements(RunMode::Execute),
            Command::Stream => self.dispatch_current_statement(RunMode::Stream),
            Command::StreamAll => self.dispatch_all_statements(RunMode::Stream),
            Command::Cancel => self.spawn_cancel(),
            Command::Clear => {
                self.tabs[self.active_tab].editor.clear();
                *self.tabs[self.active_tab].results.active_state_mut() = ResultState::Empty;
                self.tabs[self.active_tab].results.active_mut().reset();
                self.status.message = "buffer cleared".into();
            }
            Command::Explain => self.dispatch_explain(),
            Command::Export { format, path } => self.export_results(&format, &path),
            Command::DumpSchema { target } => self.dump_schema(target),
            Command::Add => self.start_wizard(),
            Command::Format => self.format_current_statement(),
            Command::FormatAll => self.format_all_statements(),
            Command::Url(dsn) => self.start_wizard_from_url(&dsn),
            Command::Test(target) => self.test_connection(target.as_deref()),
            Command::Edit(name) => self.start_wizard_edit(&name),
            Command::NextPage => self.next_page(),
            Command::PrevPage => self.prev_page(),
            Command::PageSize(n) => self.set_page_size(n),
            Command::Begin(iso) => self.begin_transaction(iso),
            Command::Commit => self.commit_transaction(),
            Command::Rollback => self.rollback_transaction(),
            Command::Savepoint(name) => self.savepoint(&name),
            Command::Release(name) => self.release_savepoint(&name),
            Command::RollbackTo(name) => self.rollback_to_savepoint(&name),
            Command::Remove(name) => self.remove_connection(&name),
            Command::Forget(name) => self.forget_password(&name),
            Command::PluginLoad(path) => self.load_plugin(&path),
            Command::PluginList => self.list_plugins(),
            Command::History => self.open_history(),
            Command::Pending => self.toggle_pending_preview(),
            Command::NewTab => self.new_tab(),
            Command::CloseTab => self.close_tab(),
            Command::NextTab => self.cycle_tab(1),
            Command::PrevTab => self.cycle_tab(-1),
            Command::Help(None) => {
                self.status.message =
                    "open <name> · close · refresh · run · run-all · stream · stream-all · explain · export <csv|json|insert> <path> · cancel · quit"
                        .into();
            }
            Command::Help(Some(name)) => {
                // Built-ins first — aliases (`o`, `q`, ...) resolve back
                // to their primary key before the lookup.
                let resolved = crate::commands::resolve_builtin_alias(&name);
                if let Some((_, desc)) = crate::commands::BUILTIN_COMMAND_DESCRIPTIONS
                    .iter()
                    .find(|(key, _)| *key == resolved)
                {
                    self.status.message = format!(":{name} — {desc}");
                } else if let Some(plugin) = self.plugins.plugin_for(&name) {
                    // Plugin command: pull the descriptor straight off
                    // the owning plugin instead of walking the full
                    // catalogue. plugin_for already located it.
                    let desc = plugin
                        .commands()
                        .into_iter()
                        .find(|cmd| cmd.name == name)
                        .map_or_else(|| "(no description)".into(), |cmd| cmd.description);
                    self.status.message = format!(":{name} — {desc}");
                } else {
                    self.status.message = format!("unknown command: {name}");
                }
            }
            Command::Substitute {
                range,
                pattern,
                replacement,
                global,
                confirm,
            } => self.execute_substitute(range, &pattern, &replacement, global, confirm),
            Command::NoHlSearch => {
                self.tabs[self.active_tab].editor_search.highlight = false;
                self.tabs[self.active_tab].editor_search.needle.clear();
                self.tabs[self.active_tab].editor_search.matches.clear();
                self.tabs[self.active_tab].editor_search.current = None;
                self.status.message = "search highlight cleared".into();
            }
            Command::SaveSnippet { name } => self.save_snippet(&name),
            Command::LoadSnippet { name } => self.load_snippet_by_name(&name),
            Command::RemoveSnippet { name } => self.remove_snippet(&name),
            Command::ListSnippets => self.open_snippets_modal(),
            Command::Empty => {}
            Command::Unknown(text) => {
                // Before reporting the command as unknown, give the
                // plugin registry a chance to claim it. The first whitespace
                // token is the command name; everything after is passed to
                // the handler verbatim.
                let (head, arg) = split_head_arg(&text);
                if self.plugins.plugin_for(head).is_some() {
                    self.dispatch_plugin(head, arg);
                } else {
                    self.status.message = format!("unknown command: {text}");
                }
            }
        }
    }

    // Plugin lifecycle and dispatch methods moved to `core::plugins` (L21).

    /// Insert raw text into the editor buffer. Used by tests to seed
    /// statements without simulating individual key presses.
    pub fn insert_into_editor(&mut self, text: &str) {
        self.tabs[self.active_tab].editor.insert_str(text);
    }

    // Session lifecycle (open_named, open_connection*, close_session),
    // schema (refresh_schema, count_sidebar_tables, schedule_schema_refresh),
    // dispatch (dispatch_current_statement, dispatch_all_statements, dispatch_batch),
    // wizard entry (start_wizard) and removal (remove_connection, forget_password)
    // moved to `core::sessions` (L21).

    // cancel_wizard, commit_wizard, handle_wizard_key moved to `core::modals` (L21).

    // new_tab/close_tab/cycle_tab/cycle_result_tab moved to `core::tabs` (L21).

    // dump_schema, dump_schema_single, dispatch_explain, export_results
    // moved to `core::dump_export` (L21).

    // Run-loop / meta-update / finalize_statement / spawn_cancel moved to
    // `core::run_loop` (L21).
}

/// L36 #5: format a millisecond duration the way the history modal
/// wants to see it: `"-"` when zero (entry predates timing capture),
/// `"12ms"` under one second, `"1.4s"` between one second and one
/// minute, otherwise `"1m23s"`.
fn format_elapsed(ms: u64) -> String {
    if ms == 0 {
        return "-".into();
    }
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    let total_secs = ms / 1_000;
    if total_secs < 60 {
        let tenths = (ms % 1_000) / 100;
        return format!("{total_secs}.{tenths}s");
    }
    let minutes = total_secs / 60;
    let seconds = total_secs % 60;
    format!("{minutes}m{seconds:02}s")
}

/// L36 #5: format the rows-returned / rows-affected pair into a single
/// column. `↓` prefix for returned rows (the SELECT-style case),
/// `∼` prefix for affected rows (the UPDATE/DELETE-style case),
/// empty when both are absent.
fn format_rows(returned: Option<u64>, affected: Option<u64>) -> String {
    if let Some(r) = returned {
        return format!("↓{r}");
    }
    if let Some(a) = affected {
        return format!("∼{a}");
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_elapsed_thresholds() {
        assert_eq!(format_elapsed(0), "-");
        assert_eq!(format_elapsed(7), "7ms");
        assert_eq!(format_elapsed(999), "999ms");
        assert_eq!(format_elapsed(1_000), "1.0s");
        assert_eq!(format_elapsed(1_450), "1.4s");
        assert_eq!(format_elapsed(59_999), "59.9s");
        assert_eq!(format_elapsed(60_000), "1m00s");
        assert_eq!(format_elapsed(83_000), "1m23s");
    }

    #[test]
    fn format_rows_variants() {
        assert_eq!(format_rows(Some(42), None), "↓42");
        assert_eq!(format_rows(None, Some(3)), "∼3");
        assert_eq!(format_rows(None, None), "");
        // Returned takes precedence over affected.
        assert_eq!(format_rows(Some(1), Some(2)), "↓1");
    }
}
