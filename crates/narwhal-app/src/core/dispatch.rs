//! `AppCore` top-level dispatch: render, key/mouse handling, the
//! `:`-prompt command parser, snippet insertion.

use crossterm::event::{KeyCode as CtKey, KeyEvent};
use narwhal_domain::Motion as DomainMotion;
use narwhal_tui::{
    render_confirm_modal, render_goto_modal, render_help_modal, render_history_modal, render_root,
    render_row_detail, render_snippets_modal, render_wizard, CompletionItemView,
    CompletionPopupView, ConfirmModalView, EditorSearchHighlight, GotoModalView, GotoRowView,
    HistoryModalState, HistoryRow, HistoryRowOutcome, Pane, RootLayout, RowDetailView,
    SearchHighlight, SidebarRow, SidebarView, SnippetsModalState, StatusBarView, WizardFieldView,
    WizardView,
};
use ratatui::layout::Rect;
use ratatui::Frame;

use super::render_helpers::{
    connection_color_to_ratatui, display_from_state, sidebar_depth, sidebar_kind, sidebar_label,
};
use super::text_utils::split_head_arg;
use super::{AppCore, ResultState};
use crate::commands::{parse, Command};
use crate::completion::CompletionKind;
use crate::run::RunMode;
use crate::wizard::DRIVERS;

impl AppCore {
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let labels: Vec<String> = self.ui.sidebar_items.iter().map(sidebar_label).collect();
        let rows: Vec<SidebarRow<'_>> = self
            .ui
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
        let visible =
            SidebarView::visible_rows(self.ui.last_layout.sidebar.height.saturating_sub(2));
        self.ui.sidebar_scroll = SidebarView::clamp_scroll(
            self.ui.sidebar_index,
            self.ui.sidebar_scroll,
            visible,
            rows.len(),
        );
        let sidebar_view = SidebarView {
            items: &rows,
            selected_index: self.ui.sidebar_index,
            scroll_offset: self.ui.sidebar_scroll,
            focused: self.ui.focus == Pane::Sidebar,
        };
        let editor_title = self.editor_title_with_tabs();
        // Read pending count before the mutable borrow below.
        let pending_count = self.ui.tabs[self.ui.active_tab].pending.len();

        let tab = &mut self.ui.tabs[self.ui.active_tab];
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
        // v1.1 #2: pull the active connection's accent colour, if any.
        // Lives on `Session.config.params.color`; the conversion to
        // ratatui::Color is in `connection_color_to_ratatui` below.
        let accent_color = self
            .session
            .active
            .as_ref()
            .and_then(|s| s.config.params.color)
            .map(connection_color_to_ratatui);
        let mut layout = RootLayout {
            mode: self.ui.vim.mode(),
            focus: self.ui.focus,
            status_bar: StatusBarView {
                connection: self.ui.status.connection.as_deref(),
                message: &self.ui.status.message,
                transaction: self.ui.status.transaction.as_deref(),
                pending: Some(pending_count),
                read_only: self.session.read_only,
            },
            running: self.process.running,
            theme: &self.ui.theme,
            sidebar: sidebar_view,
            editor: &mut tab.editor,
            editor_title: &editor_title,
            result_view: &mut tab.results.views[active_idx],
            result: result_display,
            completion: completion_view,
            editor_search: editor_search_view,
            result_count,
            active_result: active_idx,
            accent_color,
        };
        self.ui.last_layout = render_root(frame, area, &mut layout);

        if let Some(wizard) = self.modals.wizard.as_ref() {
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
                error: self.modals.wizard_error.as_deref(),
            };
            render_wizard(frame, area, &view, &self.ui.theme);
        }

        if self.modals.help_open {
            render_help_modal(frame, area, &self.ui.theme);
        }

        if let Some(state) = self.modals.history.as_ref() {
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
                        let elapsed = narwhal_tui::widgets::history::format_elapsed(e.elapsed_ms);
                        let rows = narwhal_tui::widgets::history::format_rows(
                            e.rows_returned,
                            e.rows_affected,
                        );
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
            render_history_modal(frame, area, &modal_state, &self.ui.theme);
        }

        // Snippets modal.
        if let Some(modal) = self.modals.snippets.as_ref() {
            let modal_state = SnippetsModalState {
                entries: modal.entries.iter().map(String::as_str).collect(),
                selected: modal.selected,
            };
            render_snippets_modal(frame, area, &modal_state, &self.ui.theme);
        }

        // v1.1 #1: goto fuzzy navigator sits above help/history/snippets
        // but below the confirm modal (write-safety is paramount).
        if let Some(modal) = self.modals.goto.as_ref() {
            // Slice the ranked match list down to what fits the
            // viewport (~20 rows max). Selection is mirrored into
            // the slice offset so the highlighted row is always
            // visible.
            const ROW_BUDGET: usize = 20;
            let total = modal.matches.len();
            let cursor = modal.cursor;
            // Centre the visible window on the cursor when the
            // corpus exceeds the budget.
            let start = cursor.saturating_sub(ROW_BUDGET / 2);
            let end = (start + ROW_BUDGET).min(total);
            let visible: Vec<GotoRowView<'_>> = (start..end)
                .filter_map(|i| {
                    let m = modal.matches.get(i)?;
                    let entry = modal.corpus.get(m.entry_idx)?;
                    let badge = match entry.kind {
                        narwhal_core::TableKind::Table => "T",
                        narwhal_core::TableKind::View => "V",
                        narwhal_core::TableKind::MaterializedView => "M",
                        narwhal_core::TableKind::SystemTable => "S",
                        _ => "",
                    };
                    Some(GotoRowView {
                        qualified: entry.qualified.as_str(),
                        badge,
                    })
                })
                .collect();
            let view = GotoModalView {
                query: &modal.query,
                selected: cursor.saturating_sub(start),
                rows: visible,
                total,
            };
            render_goto_modal(frame, area, &view, &self.ui.theme);
        }

        // v1.1 #2: write-confirmation modal sits on top of everything
        // else (above help, history, snippets, goto) so the user can't run
        // a write "through" a help screen they forgot to close.
        if let Some(modal) = self.modals.confirm.as_ref() {
            let view = ConfirmModalView {
                prompt: &modal.prompt,
                accept_keyword: &modal.accept_keyword,
                buffer: &modal.buffer,
                satisfied: modal.is_satisfied(),
            };
            render_confirm_modal(frame, area, &view, &self.ui.theme);
        }

        // Row detail modal — same layer as cell popup, rendered on
        // top of the result pane.
        if let Some(state) = self.ui.tabs[self.ui.active_tab].row_detail.as_ref() {
            let view = RowDetailView {
                columns: &state.columns,
                values: &state.values,
                selected_column: state.selected_column,
                scroll_offset: state.scroll_offset,
                row_index: state.row_index,
            };
            render_row_detail(frame, area, &view, &self.ui.theme);
        }

        // Pending-changes preview (L36) — stacks above the result
        // pane but below the JSON viewer (which is the very top layer).
        if self.ui.tabs[self.ui.active_tab].pending_preview.is_some() {
            let mutations: Vec<String> = self.ui.tabs[self.ui.active_tab]
                .pending
                .iter()
                .map(crate::pending::PendingMutation::summary)
                .collect();
            let scroll = self.ui.tabs[self.ui.active_tab]
                .pending_preview
                .as_ref()
                .map_or(0, |s| s.scroll);
            let view = narwhal_tui::PendingPreviewView {
                mutations: &mutations,
                scroll,
            };
            narwhal_tui::render_pending_preview(frame, area, &view, &self.ui.theme);
        }

        // JSON viewer (L36) — stacks above every other overlay so it
        // can be opened from the cell popup *or* from inside the row
        // detail modal.
        if let Some(state) = self.ui.tabs[self.ui.active_tab].json_viewer.as_ref() {
            let view = narwhal_tui::JsonViewerView {
                title: &state.title,
                pretty: &state.pretty,
                raw: &state.raw,
                scroll: state.scroll,
                parse_error: state.parse_error.as_deref(),
            };
            narwhal_tui::render_json_viewer(frame, area, &view, &self.ui.theme);
        }
    }

    pub async fn handle_key(&mut self, key: KeyEvent) {
        // H7 compat: when an `:open` is in flight we wait briefly for
        // the background `SessionOpened` reply so a follow-up key sees
        // the new session. In production this is a no-op once the
        // user's typing rhythm exceeds the connect latency; on tests
        // it lets `execute_command(":open ...")` + `handle_key` flow
        // continue working without a manual
        // `await_pending_session_opens` call. The wait runs through
        // `block_in_place` so the multi-thread runtime keeps draining
        // other workers in the meantime.
        if !self.session.pending_session_opens.is_empty() {
            self.await_pending_session_opens_sync().await;
        }
        if self.modals.wizard.is_some() {
            self.handle_wizard_key(key).await;
            return;
        }
        // v1.1 #2: write-confirmation modal. Owns the keyboard
        // exclusively while open; either matches the accept keyword
        // and resumes the held batch, or Esc cancels.
        if self.modals.confirm.is_some() {
            self.handle_confirm_key(key).await;
            return;
        }
        // L36: JSON viewer sits at the very top of the modal stack and
        // gets first refusal on every key. Once open, no other handler
        // (help, history, wizard, ...) sees the keypress.
        if self.ui.tabs[self.ui.active_tab].json_viewer.is_some() {
            self.handle_json_viewer_key(key).await;
            return;
        }
        // L36: pending preview modal is the next layer down. Owns its
        // own scroll vocabulary; commit/discard/close are forwarded to
        // the regular Results pane handlers so users can keep their
        // muscle memory.
        if self.ui.tabs[self.ui.active_tab].pending_preview.is_some() {
            self.handle_pending_preview_key(key).await;
            return;
        }
        // When the help modal is open, it intercepts Esc / ? / F1 to
        // close and silently consumes every other key so the user
        // doesn't accidentally trigger an action behind the overlay.
        if self.modals.help_open {
            match key.code {
                CtKey::Esc | CtKey::F(1) => {
                    self.modals.help_open = false;
                }
                CtKey::Char('?') if key.modifiers.is_empty() => {
                    self.modals.help_open = false;
                }
                _ => {
                    // consumed but no-op
                }
            }
            return;
        }
        // When the history modal is open, it intercepts all keys.
        if self.modals.history.is_some() {
            self.handle_history_key(key).await;
            return;
        }
        // When the snippets modal is open, it intercepts all keys.
        if self.modals.snippets.is_some() {
            self.handle_snippets_key(key).await;
            return;
        }
        // v1.1 #1: goto fuzzy navigator owns the foreground while open.
        if self.modals.goto.is_some() {
            self.handle_goto_key(key).await;
            return;
        }
        if self.handle_global_key(key).await {
            return;
        }
        // Pending result-tab leader: `]` or `[` was pressed, waiting
        // for `r` to complete the sequence. Any other key cancels.
        if let Some(leader) = self.ui.pending_result_leader.take() {
            if key.code == CtKey::Char('r') && key.modifiers.is_empty() {
                match leader {
                    ']' => self.cycle_result_tab(1).await,
                    '[' => self.cycle_result_tab(-1).await,
                    _ => {}
                }
            }
            return;
        }
        match self.ui.focus {
            Pane::Editor => self.handle_editor_key(key).await,
            Pane::Sidebar => self.handle_sidebar_key(key).await,
            Pane::Results => self.handle_results_key(key).await,
            // Future panes fall through to the editor handler until wired.
            _ => self.handle_editor_key(key).await,
        }
    }

    /// Sprint 7 (LOW): paste handler. Inserts the pasted text into
    /// the active tab's editor in one shot so newlines are preserved
    /// instead of being interpreted as `Enter` keypresses one-by-one
    /// (which would trip motion handlers and the modal command
    /// prompt). Other panes do not currently accept paste.
    pub async fn editor_paste(&mut self, text: &str) {
        if matches!(self.ui.focus, Pane::Editor) {
            self.ui.tabs[self.ui.active_tab].editor.insert_str(text);
            self.ui.status.message = format!("pasted {} char(s)", text.chars().count());
        }
    }

    /// Route a crossterm `MouseEvent` through the same handlers the
    /// keyboard path uses. `LayoutRegions` from the most recent render
    /// provides the hit-test rects.
    pub async fn handle_mouse(&mut self, event: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};

        let pos = (event.column, event.row);

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.handle_left_click(pos).await;
            }
            MouseEventKind::ScrollUp => {
                self.handle_scroll(pos, -1).await;
            }
            MouseEventKind::ScrollDown => {
                self.handle_scroll(pos, 1).await;
            }
            // Up, Moved, Drag are no-ops for now.
            _ => {}
        }
    }

    async fn handle_left_click(&mut self, pos: (u16, u16)) {
        let layout = self.ui.last_layout.clone();

        // Priority: completion popup > sidebar tables > result headers/rows > pane focus.
        for (rect, item_index) in &layout.completion_items {
            if rect.contains(ratatui::layout::Position::new(pos.0, pos.1)) {
                self.accept_completion_at(*item_index).await;
                return;
            }
        }

        for (rect, sidebar_idx) in &layout.sidebar_tables {
            if rect.contains(ratatui::layout::Position::new(pos.0, pos.1)) {
                self.click_sidebar_table(*sidebar_idx).await;
                return;
            }
        }

        for (rect, result_idx) in &layout.result_tabs {
            if rect.contains(ratatui::layout::Position::new(pos.0, pos.1)) {
                self.click_result_tab(*result_idx).await;
                return;
            }
        }

        for (rect, col_idx) in &layout.result_headers {
            if rect.contains(ratatui::layout::Position::new(pos.0, pos.1)) {
                // Sort cycle action: move column focus and toggle sort.
                self.ui.tabs[self.ui.active_tab]
                    .results
                    .active_mut()
                    .column_index = *col_idx;
                self.toggle_sort().await;
                return;
            }
        }

        for (rect, row_idx) in &layout.result_rows {
            if rect.contains(ratatui::layout::Position::new(pos.0, pos.1)) {
                self.ui.tabs[self.ui.active_tab]
                    .results
                    .active_mut()
                    .select(Some(*row_idx));
                self.ui.focus = Pane::Results;
                self.ui.status.message = format!("focus → {}", Pane::Results.label());
                return;
            }
        }

        // Fall through to pane focus change.
        if layout
            .sidebar
            .contains(ratatui::layout::Position::new(pos.0, pos.1))
        {
            self.ui.focus = Pane::Sidebar;
            self.ui.status.message = format!("focus → {}", Pane::Sidebar.label());
        } else if layout
            .editor
            .contains(ratatui::layout::Position::new(pos.0, pos.1))
        {
            self.ui.focus = Pane::Editor;
            self.ui.status.message = format!("focus → {}", Pane::Editor.label());
        } else if layout
            .results
            .contains(ratatui::layout::Position::new(pos.0, pos.1))
        {
            self.ui.focus = Pane::Results;
            self.ui.status.message = format!("focus → {}", Pane::Results.label());
        }
    }

    async fn handle_scroll(&mut self, pos: (u16, u16), delta: i32) {
        let layout = &self.ui.last_layout;

        if layout
            .results
            .contains(ratatui::layout::Position::new(pos.0, pos.1))
        {
            let row_count = match self.ui.tabs[self.ui.active_tab].results.active_state() {
                ResultState::Rows { rows, .. } | ResultState::Running { rows, .. } => rows.len(),
                _ => return,
            };
            if delta > 0 {
                self.ui.tabs[self.ui.active_tab]
                    .results
                    .active_mut()
                    .move_down(row_count);
            } else {
                self.ui.tabs[self.ui.active_tab]
                    .results
                    .active_mut()
                    .move_up();
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
            let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
            if delta > 0 {
                // Scroll down: move cursor down
                buf.apply_motion(DomainMotion::Down, 1);
                buf.ensure_visible(height);
            } else {
                buf.apply_motion(DomainMotion::Up, 1);
                buf.ensure_visible(height);
            }
        } else if layout
            .sidebar
            .contains(ratatui::layout::Position::new(pos.0, pos.1))
        {
            // L24: mouse wheel over the sidebar pans the viewport by
            // 3 rows per tick. The selection stays put so the user can
            // peek at off-screen rows without losing context.
            self.scroll_sidebar(if delta > 0 { 3 } else { -3 }).await;
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
    pub async fn execute_command(&mut self, raw: &str) {
        // H7 compat: any command other than `:open` that follows an
        // in-flight open should see the freshly-opened session. Mirror
        // the same brief wait that `handle_key` does so callers can
        // chain `execute_command(":open foo"); execute_command(":run")`
        // without explicit drains.
        let parsed = parse(raw);
        if !matches!(parsed, Command::Open(_) | Command::Quit | Command::Cancel)
            && !self.session.pending_session_opens.is_empty()
        {
            self.await_pending_session_opens_sync().await;
        }
        match parsed {
            Command::Quit => self.process.should_quit = true,
            Command::Open(name) => self.open_named(&name).await,
            Command::Close => self.close_session().await,
            Command::Refresh => self.refresh_schema().await,
            Command::Run => self.dispatch_current_statement(RunMode::Execute).await,
            Command::RunAll => self.dispatch_all_statements(RunMode::Execute).await,
            Command::Stream => self.dispatch_current_statement(RunMode::Stream).await,
            Command::StreamAll => self.dispatch_all_statements(RunMode::Stream).await,
            Command::Cancel => self.spawn_cancel(),
            Command::Clear => {
                self.ui.tabs[self.ui.active_tab].editor.clear();
                *self.ui.tabs[self.ui.active_tab].results.active_state_mut() = ResultState::Empty;
                self.ui.tabs[self.ui.active_tab]
                    .results
                    .active_mut()
                    .reset();
                self.ui.status.message = "buffer cleared".into();
            }
            Command::Explain => self.dispatch_explain().await,
            Command::Export { format, path } => self.export_results(&format, &path).await,
            Command::DumpSchema { target } => self.dump_schema(target).await,
            Command::Add => self.start_wizard().await,
            Command::Format => self.format_current_statement().await,
            Command::FormatAll => self.format_all_statements().await,
            Command::Url(dsn) => self.start_wizard_from_url(&dsn).await,
            Command::Test(target) => self.test_connection(target.as_deref()).await,
            Command::Edit(name) => self.start_wizard_edit(&name).await,
            Command::NextPage => self.next_page().await,
            Command::PrevPage => self.prev_page().await,
            Command::PageSize(n) => self.set_page_size(n).await,
            Command::Begin(iso) => self.begin_transaction(iso).await,
            Command::Commit => self.commit_transaction().await,
            Command::Rollback => self.rollback_transaction().await,
            Command::Savepoint(name) => self.savepoint(&name).await,
            Command::Release(name) => self.release_savepoint(&name).await,
            Command::RollbackTo(name) => self.rollback_to_savepoint(&name).await,
            Command::Remove(name) => self.remove_connection(&name).await,
            Command::Forget(name) => self.forget_password(&name).await,
            Command::PluginLoad(path) => self.load_plugin(&path).await,
            Command::PluginList => self.list_plugins().await,
            Command::History(filter) => self.open_history_with_filter(filter).await,
            Command::Pending => self.toggle_pending_preview().await,
            Command::Submit => self.commit_pending().await,
            Command::Revert => self.discard_pending().await,
            Command::NewTab => self.new_tab().await,
            Command::CloseTab => self.close_tab().await,
            Command::NextTab => self.cycle_tab(1).await,
            Command::PrevTab => self.cycle_tab(-1).await,
            Command::Help(None) => {
                self.ui.status.message =
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
                    self.ui.status.message = format!(":{name} — {desc}");
                } else if let Some(plugin) = self.deps.plugins.plugin_for(&name) {
                    // Plugin command: pull the descriptor straight off
                    // the owning plugin instead of walking the full
                    // catalogue. plugin_for already located it.
                    let desc = plugin
                        .commands()
                        .into_iter()
                        .find(|cmd| cmd.name == name)
                        .map_or_else(|| "(no description)".into(), |cmd| cmd.description);
                    self.ui.status.message = format!(":{name} — {desc}");
                } else {
                    self.ui.status.message = format!("unknown command: {name}");
                }
            }
            Command::Substitute {
                range,
                pattern,
                replacement,
                global,
                confirm,
            } => {
                self.execute_substitute(range, &pattern, &replacement, global, confirm)
                    .await;
            }
            Command::NoHlSearch => {
                self.ui.tabs[self.ui.active_tab].editor_search.highlight = false;
                self.ui.tabs[self.ui.active_tab]
                    .editor_search
                    .needle
                    .clear();
                self.ui.tabs[self.ui.active_tab]
                    .editor_search
                    .matches
                    .clear();
                self.ui.tabs[self.ui.active_tab].editor_search.current = None;
                self.ui.status.message = "search highlight cleared".into();
            }
            Command::SaveSnippet { name } => self.save_snippet(&name).await,
            Command::LoadSnippet { name } => self.load_snippet_by_name(&name).await,
            Command::RemoveSnippet { name } => self.remove_snippet(&name).await,
            Command::ListSnippets => self.open_snippets_modal().await,
            Command::Goto => self.open_goto_modal().await,
            Command::Filter(spec) => self.apply_filter_command(spec).await,
            Command::Sort(arg) => self.apply_sort_command(arg).await,
            Command::DiffSchema { left, right } => self.diff_schema_command(left, right).await,
            Command::Lint => self.lint_buffer_command().await,
            Command::Template(name) => self.insert_template_command(name).await,
            Command::Empty => {}
            Command::Unknown(text) => {
                // Before reporting the command as unknown, give the
                // plugin registry a chance to claim it. The first whitespace
                // token is the command name; everything after is passed to
                // the handler verbatim.
                let (head, arg) = split_head_arg(&text);
                if self.deps.plugins.plugin_for(head).is_some() {
                    self.dispatch_plugin(head, arg).await;
                } else {
                    self.ui.status.message = format!("unknown command: {text}");
                }
            }
        }
    }

    // Plugin lifecycle and dispatch methods moved to `core::plugins` (L21).

    /// Insert raw text into the editor buffer. Used by tests to seed
    /// statements without simulating individual key presses.
    pub async fn insert_into_editor(&mut self, text: &str) {
        self.ui.tabs[self.ui.active_tab].editor.insert_str(text);
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
