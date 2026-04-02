//! `AppCore` top-level dispatch: render, key/mouse handling, the
//! `:`-prompt command parser, snippet insertion.

use crossterm::event::{KeyCode as CtKey, KeyEvent};
use narwhal_domain::Motion as DomainMotion;
use narwhal_tui::{
    ChartPlaceholder, ChartView, ChartViewKind, CompletionItemView, CompletionPopupView,
    ConfirmModalView, EditorSearchHighlight, GotoModalView, GotoRowView, HistoryModalState,
    HistoryRow, HistoryRowOutcome, Pane, PivotPlaceholder, PivotTableView, RootLayout,
    RowDetailView, SearchHighlight, SidebarRow, SidebarView, SnippetsModalState, StatusBarView,
    ContextMenuItemView, ContextMenuView, WizardFieldView, WizardView, render_confirm_modal,
    render_context_menu, render_goto_modal, render_help_modal,
    render_history_modal, render_root, render_row_detail, render_snippets_modal, render_wizard,
};
use ratatui::Frame;
use ratatui::layout::Rect;

use super::render_helpers::{
    ChartLayoutOwned, PivotLayoutOwned, chart_payload, connection_color_to_ratatui,
    display_from_state, pivot_payload, sidebar_depth, sidebar_kind, sidebar_label,
};
use super::text_utils::split_head_arg;
use super::{AppCore, ResultState};
use crate::commands::{Command, parse};
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
        // T1-T3-A: refresh tree-sitter highlights for the editor
        // before any immutable borrows further down (search /
        // completion / editor_search) lock the tab. The method only
        // touches `tab.editor`, `tab.ts_parser`, `tab.sql_highlights`
        // — disjoint from those views.
        let _ = tab.sql_highlights();
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
        // T2-T4-C: derive the chart payload, if a chart is active on
        // this tab. The owned envelope below outlives the borrow we
        // hand into `RootLayout`; without it the labels / values
        // references would dangle.
        let chart_owned: Option<ChartLayoutOwned> = tab
            .chart
            .as_ref()
            .and_then(|cfg| chart_payload(cfg, &tab.results.states[active_idx]));
        let chart_layout = chart_owned.as_ref().map(|owned| match owned {
            ChartLayoutOwned::Ok(data) => Ok(ChartView {
                kind: match data.kind {
                    crate::core::chart::ChartKind::Bar => ChartViewKind::Bar,
                    crate::core::chart::ChartKind::Line => ChartViewKind::Line,
                    crate::core::chart::ChartKind::Sparkline => ChartViewKind::Sparkline,
                },
                title: data.title.as_str(),
                labels: data.labels.as_slice(),
                values: data.values.as_slice(),
            }),
            ChartLayoutOwned::Err { title, message } => Err(ChartPlaceholder {
                title: title.as_str(),
                message: message.as_str(),
            }),
        });
        // T2-T4-D: derive the pivot payload, if a pivot is active on
        // this tab. Same owned-envelope pattern as the chart slot.
        let pivot_owned: Option<PivotLayoutOwned> = tab
            .pivot
            .as_ref()
            .and_then(|cfg| pivot_payload(cfg, &tab.results.states[active_idx]));
        let pivot_layout = pivot_owned.as_ref().map(|owned| match owned {
            PivotLayoutOwned::Ok { table, agg_label } => Ok(PivotTableView {
                title: "result",
                agg_label: agg_label.as_str(),
                row_dim_headers: table.row_dim_headers.as_slice(),
                col_headers: table.col_headers.as_slice(),
                rows: table.rows.as_slice(),
            }),
            PivotLayoutOwned::Err { title, message } => Err(PivotPlaceholder {
                title: title.as_str(),
                message: message.as_str(),
            }),
        });
        // v1.1 #2: pull the active connection's accent colour, if any.
        // Lives on `Session.config.params.color`; the conversion to
        // ratatui::Color is in `connection_color_to_ratatui` below.
        let accent_color = self
            .session
            .active
            .as_ref()
            .and_then(|s| s.config.params.color)
            .map(connection_color_to_ratatui);
        // MR-M3 / R3-N4: a sticky notification (set via
        // `status.notify(...)`) wins over the per-frame `message`
        // slot until its TTL expires, so one-shot warnings like
        // "multi-line paste collapsed secondary cursors" survive
        // the next keystroke instead of being clobbered. The render
        // path uses the read-only `peek_notification`; expiry is
        // ticked one level up (see `run_loop` / event dispatch).
        let status_text: String = if let Some(notif) = self.ui.status.peek_notification() {
            notif.to_owned()
        } else {
            self.ui.status.message.clone()
        };
        let mut layout = RootLayout {
            mode: self.ui.vim.mode(),
            focus: self.ui.focus,
            status_bar: StatusBarView {
                connection: self.ui.status.connection.as_deref(),
                message: &status_text,
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
            // T1-T3-A: SQL highlight spans are sourced from the per-tab
            // tree-sitter Parser. Wiring lives in the tab state
            // (`Tab::sql_highlights()`) and is refreshed lazily on
            // dirty marks; see docs/dev/t1-t3-a-treesitter.md.
            editor_sql_highlights: tab.sql_highlights.as_deref(),
            chart: chart_layout,
            pivot: pivot_layout,
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

        // Editor right-click context menu. Drawn above the editor
        // pane but below the higher-priority modals so a
        // long-running confirm prompt still wins.
        if let Some(menu) = self.ui.context_menu.as_ref() {
            let items: Vec<ContextMenuItemView<'_>> = menu
                .items
                .iter()
                .map(|i| ContextMenuItemView {
                    label: i.label,
                    disabled: i.disabled,
                })
                .collect();
            let view = ContextMenuView {
                anchor: menu.anchor,
                items: &items,
                selected: menu.selected,
            };
            render_context_menu(frame, area, &view, &self.ui.theme);
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

        // Diagram modal (Focused / Impact) — sits below the JSON
        // viewer in the modal stack but above pending preview.
        if let Some(state) = self.ui.tabs[self.ui.active_tab].diagram.as_ref() {
            let mode = match state.mode {
                crate::core::DiagramMode::Focused => narwhal_tui::DiagramViewMode::Focused,
                crate::core::DiagramMode::Impact => narwhal_tui::DiagramViewMode::Impact,
            };
            let view = narwhal_tui::DiagramView {
                mode,
                model: &state.model,
                center: &state.center,
                impact: &state.impact,
                selected: state.selected,
                scroll: state.scroll,
                icons: state.icons,
            };
            narwhal_tui::render_diagram(frame, area, &view, &self.ui.theme);
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
        // Diagram modal sits just below the JSON viewer so a user can
        // pop a JSON cell open *from inside* the diagram modal without
        // losing the diagram. Owns its own keymap; no chord falls
        // through to underlying panes.
        if self.ui.tabs[self.ui.active_tab].diagram.is_some() {
            self.handle_diagram_key(key).await;
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
            MouseEventKind::Drag(MouseButton::Left) => {
                self.handle_left_drag(pos).await;
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.ui.mouse_drag = None;
            }
            MouseEventKind::Down(MouseButton::Middle) => {
                self.handle_middle_click(pos).await;
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.handle_right_click(pos).await;
            }
            MouseEventKind::ScrollUp => {
                self.handle_scroll(pos, -1).await;
            }
            MouseEventKind::ScrollDown => {
                self.handle_scroll(pos, 1).await;
            }
            _ => {}
        }
    }

    /// Translate a screen `(col, row)` inside the editor area into a
    /// `(buffer_row, buffer_col)` byte offset. Accounts for the
    /// border, the line-number gutter and the current scroll.
    /// Returns `None` when the click landed outside the editor's
    /// inner text region (border, gutter only, ...).
    fn editor_click_to_buffer_pos(&self, pos: (u16, u16)) -> Option<(usize, usize)> {
        let layout = &self.ui.last_layout;
        let rect = layout.editor;
        if !rect.contains(ratatui::layout::Position::new(pos.0, pos.1)) {
            return None;
        }
        // Editor rect has a 1-cell border on every side; the inner
        // text area starts at (x+1, y+1).
        let inner_x = rect.x.saturating_add(1);
        let inner_y = rect.y.saturating_add(1);
        if pos.0 < inner_x || pos.1 < inner_y {
            return None;
        }
        let buf = &self.ui.tabs[self.ui.active_tab].editor;
        let gutter = narwhal_tui::gutter_width(buf.line_count()) as u16;
        let row_in_view = pos.1 - inner_y;
        let col_in_view = pos.0.saturating_sub(inner_x).saturating_sub(gutter);
        let target_row = (buf.scroll() + row_in_view as usize).min(
            buf.line_count().saturating_sub(1),
        );
        let line_len = buf.get_line(target_row).len();
        let target_col = (col_in_view as usize).min(line_len);
        Some((target_row, target_col))
    }

    async fn handle_left_drag(&mut self, pos: (u16, u16)) {
        if self.ui.mouse_mode != narwhal_config::MouseSelectionMode::Enabled {
            return;
        }
        let Some(drag) = self.ui.mouse_drag else {
            return;
        };
        let Some((row, col)) = self.editor_click_to_buffer_pos(pos) else {
            return;
        };
        let tab = &mut self.ui.tabs[self.ui.active_tab];
        if tab.id() != drag.tab_id {
            return;
        }
        // Anchor the selection at the original click position the
        // first time drag is observed; subsequent drag events just
        // move the head.
        if tab.editor.selection().is_none() {
            tab.editor.set_selection(Some(
                narwhal_domain::editor::Selection::character(drag.anchor, drag.anchor),
            ));
        }
        tab.editor.extend_selection_to(row, col);
        // Move the cursor with the drag head so the renderer keeps
        // up.
        tab.editor.set_cursor(row, col);
    }

    async fn handle_middle_click(&mut self, pos: (u16, u16)) {
        // Middle-click paste targets the editor only; clicks
        // elsewhere are ignored to stay consistent with most
        // terminals.
        let Some((row, col)) = self.editor_click_to_buffer_pos(pos) else {
            return;
        };
        let text = match self.deps.clipboard.get_text() {
            Ok(t) => t,
            Err(e) => {
                self.ui.status.message = format!("clipboard error: {e}");
                return;
            }
        };
        if text.is_empty() {
            return;
        }
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        let before = buf.snapshot();
        buf.set_cursor(row, col);
        buf.insert_str(&text);
        buf.commit_undo_snapshot(before);
        self.ui.focus = Pane::Editor;
    }

    async fn handle_right_click(&mut self, pos: (u16, u16)) {
        // Only open the menu inside the editor pane; right-clicks
        // elsewhere fall through to the keyboard-level handlers.
        let Some((row, col)) = self.editor_click_to_buffer_pos(pos) else {
            return;
        };
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        // If the click lands outside the current selection, move
        // the cursor and drop the old selection — mirrors most
        // desktop editors.
        let in_selection = buf.selection().is_some_and(|s| {
            let (start, end) = s.normalised();
            (start.0, start.1) <= (row, col) && (row, col) <= (end.0, end.1)
        });
        if !in_selection {
            buf.clear_selection();
            buf.set_cursor(row, col);
        }
        self.ui.focus = Pane::Editor;
        let has_selection = buf.has_selection();
        let has_clipboard = self
            .deps
            .clipboard
            .get_text()
            .is_ok_and(|t| !t.is_empty());
        self.ui.context_menu = Some(crate::core::state::ui::ContextMenuState {
            anchor: pos,
            selected: 0,
            items: vec![
                crate::core::state::ui::ContextMenuItem {
                    label: "Cut",
                    action: crate::core::state::ui::ContextMenuAction::Cut,
                    disabled: !has_selection,
                },
                crate::core::state::ui::ContextMenuItem {
                    label: "Copy",
                    action: crate::core::state::ui::ContextMenuAction::Copy,
                    disabled: !has_selection,
                },
                crate::core::state::ui::ContextMenuItem {
                    label: "Paste",
                    action: crate::core::state::ui::ContextMenuAction::Paste,
                    disabled: !has_clipboard,
                },
                crate::core::state::ui::ContextMenuItem {
                    label: "Select All",
                    action: crate::core::state::ui::ContextMenuAction::SelectAll,
                    disabled: false,
                },
                crate::core::state::ui::ContextMenuItem {
                    label: "Run Selection",
                    action: crate::core::state::ui::ContextMenuAction::RunSelection,
                    disabled: !has_selection,
                },
                crate::core::state::ui::ContextMenuItem {
                    label: "Find",
                    action: crate::core::state::ui::ContextMenuAction::Find,
                    disabled: false,
                },
                crate::core::state::ui::ContextMenuItem {
                    label: "Toggle Comment",
                    action: crate::core::state::ui::ContextMenuAction::ToggleComment,
                    disabled: false,
                },
            ],
        });
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

        // Editor body: place the cursor (and on double/triple
        // click select a word / line) before falling through to a
        // plain focus change.
        if self.ui.mouse_mode != narwhal_config::MouseSelectionMode::Disabled {
            if let Some((row, col)) = self.editor_click_to_buffer_pos(pos) {
                let click_count = self.bump_click_counter(pos);
                let tab = &mut self.ui.tabs[self.ui.active_tab];
                tab.editor.set_cursor(row, col);
                match click_count {
                    1 => {
                        tab.editor.clear_selection();
                        // Arm drag-selection: subsequent Drag
                        // events extend from this anchor.
                        if self.ui.mouse_mode
                            == narwhal_config::MouseSelectionMode::Enabled
                        {
                            self.ui.mouse_drag =
                                Some(crate::core::state::ui::MouseDragState {
                                    tab_id: tab.id(),
                                    anchor: (row, col),
                                });
                        }
                    }
                    2 => {
                        // Word select: snap to the surrounding word.
                        let (w_start, w_end) = word_bounds_at(
                            tab.editor.get_line(row),
                            col,
                        );
                        tab.editor.set_selection(Some(
                            narwhal_domain::editor::Selection::character(
                                (row, w_start),
                                (row, w_end),
                            ),
                        ));
                        tab.editor.set_cursor(row, w_end);
                    }
                    _ => {
                        // Triple-and-up: line select.
                        let line_len = tab.editor.get_line(row).len();
                        tab.editor.set_selection(Some(
                            narwhal_domain::editor::Selection::line(
                                (row, 0),
                                (row, line_len),
                            ),
                        ));
                    }
                }
                self.ui.focus = Pane::Editor;
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

    /// Bump the multi-click counter at `pos`. Returns the resulting
    /// click count (1 = single, 2 = double, 3+ = triple-and-up).
    /// Clicks reset to 1 when more than 500ms elapsed since the
    /// last click or the position moved by more than 2 cells in
    /// either axis.
    fn bump_click_counter(&mut self, pos: (u16, u16)) -> u8 {
        let now = std::time::Instant::now();
        const WINDOW: std::time::Duration = std::time::Duration::from_millis(500);
        let next_count = match self.ui.last_click {
            Some(prev)
                if now.duration_since(prev.at) <= WINDOW
                    && prev.pos.0.abs_diff(pos.0) <= 2
                    && prev.pos.1.abs_diff(pos.1) <= 2 =>
            {
                prev.count.saturating_add(1)
            }
            _ => 1,
        };
        self.ui.last_click = Some(crate::core::state::ui::LastClick {
            at: now,
            pos,
            count: next_count,
        });
        next_count
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
            Command::Export {
                format,
                path,
                options,
            } => self.export_results(&format, &path, options).await,
            Command::DumpSchema { target } => self.dump_schema(target).await,
            Command::DiagramExport {
                format,
                path,
                table,
                schema,
            } => {
                self.export_diagram(format, path, table, schema).await;
            }
            Command::DiagramFocus(table) => self.open_diagram_focus(table).await,
            Command::DiagramImpact(table) => self.open_diagram_impact(table).await,
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
            Command::SchemaDiff {
                source,
                target,
                dialect,
                schema,
                table,
                schema_map,
            } => {
                self.schema_diff_command(source, target, dialect, schema, table, schema_map)
                    .await;
            }
            Command::Lint => self.lint_buffer_command().await,
            Command::Chart(arg) => self.chart_command(arg).await,
            Command::Pivot(arg) => self.pivot_command(arg).await,
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

/// Find the start + end (exclusive) byte offsets of the word that
/// contains `col` inside `line`. A "word" is a maximal run of
/// alphanumeric or `_` characters; if the cursor lands on whitespace
/// the range collapses around the cursor (start == end).
fn word_bounds_at(line: &str, col: usize) -> (usize, usize) {
    let bytes = line.as_bytes();
    let len = bytes.len();
    if col > len {
        return (len, len);
    }
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    // If the cursor sits on a non-word byte but the *previous* byte
    // is a word byte, snap left so double-clicking just past the
    // end of a word still selects it.
    let pivot = if col == len || !is_word(bytes[col]) {
        if col > 0 && is_word(bytes[col - 1]) {
            col - 1
        } else {
            return (col, col);
        }
    } else {
        col
    };
    let mut start = pivot;
    while start > 0 && is_word(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = pivot + 1;
    while end < len && is_word(bytes[end]) {
        end += 1;
    }
    (start, end)
}

#[cfg(test)]
mod word_bounds_tests {
    use super::word_bounds_at;

    #[test]
    fn middle_of_word() {
        assert_eq!(word_bounds_at("hello world", 2), (0, 5));
    }

    #[test]
    fn end_of_word_snaps_left() {
        // col == 5 sits on the space; previous byte 'o' is a word
        // char so we snap.
        assert_eq!(word_bounds_at("hello world", 5), (0, 5));
    }

    #[test]
    fn whitespace_collapses() {
        // col 5 is space, col 5-1='o' is word - that hits the snap.
        // Use col=6 (start of "world" but prev is space too).
        assert_eq!(word_bounds_at("a  b", 1), (0, 1));
        // The middle space.
        assert_eq!(word_bounds_at("a  b", 2), (2, 2));
    }

    #[test]
    fn beginning_of_word() {
        assert_eq!(word_bounds_at("select count", 7), (7, 12));
    }

    #[test]
    fn beyond_end_clamps() {
        assert_eq!(word_bounds_at("abc", 99), (3, 3));
    }

    #[test]
    fn underscore_is_word() {
        assert_eq!(word_bounds_at("user_id = 1", 2), (0, 7));
    }
}
