//! Run-loop integration extracted from `core.rs` (L21).
//!
//! Hosts the receiver-side of the `RunUpdate` and `MetaUpdate` channels
//! plus the per-statement finalisation pipeline (plugin transforms,
//! explain decoding, DDL-triggered schema refresh).
use std::sync::Arc;
use std::time::{Duration, Instant};

use narwhal_core::{ColumnHeader, Row};
use narwhal_tui::{ExplainPlanLine, Pane};

use super::render_helpers::{extract_explain_plan, is_explain_result};
use super::text_utils::truncate;
use super::{AppCore, HistoryState, ResultBundle, ResultState, ResultView};
use crate::ddl::{build_dump, build_table_ddl};
use crate::meta::{spawn_meta_request, MetaRequest, MetaUpdate};
use crate::run::RunUpdate;

impl AppCore {
    pub(super) fn spawn_cancel(&mut self) {
        let slot = self.cancel_slot.clone();
        tokio::spawn(async move {
            // H6: Take the handle out of the slot under a short-lived
            // guard so the mutex is NOT held across `.await` on
            // `cancel()`. Holding a tokio `Mutex` across an await
            // serialises concurrent cancel attempts and violates the
            // project's lock-across-await rule. The run loop replaces
            // `cancel_slot` to `None` once the statement settles, so
            // taking is safe — there is nothing useful to put back.
            let handle = {
                let mut guard = slot.lock().await;
                guard.take()
            };
            if let Some(handle) = handle {
                if let Err(error) = handle.cancel().await {
                    tracing::warn!(target: "narwhal::app", error = %error, "cancel failed");
                }
            }
        });
        self.status.message = "cancellation requested".into();
    }

    /// Receive the next [`RunUpdate`] from the worker channel.
    pub async fn recv_run_update(&mut self) -> Option<RunUpdate> {
        self.run_rx.recv().await
    }

    /// Non-blocking variant of [`Self::recv_run_update`] used by tests
    /// that need to drain whatever has been queued without awaiting
    /// new traffic.
    #[doc(hidden)]
    pub fn try_recv_run_update(&mut self) -> Option<RunUpdate> {
        self.run_rx.try_recv().ok()
    }

    pub async fn handle_run_update(&mut self, update: RunUpdate) {
        // All mutations target the tab that *started* the run, not the
        // tab the user may have switched to in the meantime (K1-A fix).
        let rt = self.run_tab_index();
        match update {
            RunUpdate::StatementStarted { index, total, sql } => {
                let streaming = matches!(
                    self.tabs[rt].results.active_state(),
                    ResultState::Running {
                        streaming: true,
                        ..
                    }
                );
                // If there is a previously-finalized entry sitting in the
                // active slot (index > 1 means we've finished at least one
                // statement), snapshot it before overwriting.
                //
                // Note: `index` is 1-based, so index > 1 means we are
                // past the first StatementStarted.
                if index > 1 {
                    let state = self.tabs[rt].results.states.swap_remove(0);
                    let view = self.tabs[rt].results.views.swap_remove(0);
                    self.pending_result_entries_states.push(state);
                    self.pending_result_entries_views.push(view);
                }
                // Create a fresh entry for the new statement.
                self.tabs[rt].results = ResultBundle::single(
                    ResultState::Running {
                        sql: sql.clone(),
                        index,
                        total,
                        columns: Vec::new(),
                        rows: Vec::new(),
                        streaming,
                        started_at: Instant::now(),
                        last_render: Instant::now(),
                    },
                    ResultView::new(),
                );
                self.status.message = format!("running {index}/{total}: {}", truncate(&sql, 60));
            }
            RunUpdate::HeaderReady { columns: cols } => {
                if let ResultState::Running { columns, .. } =
                    self.tabs[rt].results.active_state_mut()
                {
                    *columns = cols;
                }
            }
            RunUpdate::RowsAppended { rows: new_rows } => {
                if let ResultState::Running {
                    rows,
                    last_render,
                    streaming: true,
                    ..
                } = self.tabs[rt].results.active_state_mut()
                {
                    rows.extend(new_rows);
                    let now = Instant::now();
                    if now.duration_since(*last_render)
                        >= narwhal_tui::constants::STREAM_RENDER_THROTTLE
                    {
                        *last_render = now;
                    }
                } else if let ResultState::Running { rows, .. } =
                    self.tabs[rt].results.active_state_mut()
                {
                    rows.extend(new_rows);
                }
            }
            RunUpdate::StatementFinished {
                elapsed_ms,
                rows_returned,
                rows_affected,
                streamed,
            } => {
                self.finalize_statement(elapsed_ms, rows_returned, rows_affected, streamed)
                    .await;
            }
            RunUpdate::Failed {
                error,
                elapsed_ms,
                cancelled,
            } => {
                // When a streaming query was cancelled by the user,
                // transition to Cancelled state so the title bar shows
                // the partial row count instead of an error message.
                if cancelled {
                    if let ResultState::Running {
                        rows,
                        streaming: true,
                        started_at,
                        ..
                    } = self.tabs[rt].results.active_state()
                    {
                        let rows_so_far = rows.len();
                        let elapsed_ms = started_at
                            .elapsed()
                            .as_millis()
                            .try_into()
                            .unwrap_or(u64::MAX);
                        *self.tabs[rt].results.active_state_mut() = ResultState::Cancelled {
                            rows_so_far,
                            elapsed_ms,
                        };
                        self.tabs[rt].results.active_mut().reset();
                        return;
                    }
                }
                *self.tabs[rt].results.active_state_mut() = ResultState::Error {
                    message: error,
                    elapsed_ms,
                };
                self.tabs[rt].results.active_mut().reset();
            }
            RunUpdate::AllDone {
                successes,
                failures,
                ddl,
            } => {
                self.running = false;
                self.run_tab = None;

                // Build the final ResultBundle from collected entries.
                // Push the current (last) active entry into the pending
                // list first so everything is in order.
                let current_state = self.tabs[rt].results.states.swap_remove(0);
                let current_view = self.tabs[rt].results.views.swap_remove(0);
                self.pending_result_entries_states.push(current_state);
                self.pending_result_entries_views.push(current_view);

                let states = std::mem::take(&mut self.pending_result_entries_states);
                let views = std::mem::take(&mut self.pending_result_entries_views);
                self.tabs[rt].results = if states.len() == 1 {
                    // Single result — no strip, behaviour-preserving.
                    ResultBundle::single(
                        states.into_iter().next().unwrap_or(ResultState::Empty),
                        views.into_iter().next().unwrap_or_default(),
                    )
                } else {
                    let len = states.len();
                    let mut bundle = ResultBundle::multi(states, views);
                    // Default to showing the last result — this is the
                    // SELECT the user most likely cares about, and matches
                    // the pre-bundle behaviour where the final statement
                    // overwrote the result pane.
                    bundle.active = len - 1;
                    bundle
                };

                let base = if failures == 0 {
                    format!("done · {successes} statement(s)")
                } else {
                    format!("done · {successes} ok · {failures} failed")
                };
                self.status.message = match self.plugin_warning.take() {
                    Some(warning) => format!("{base} · {warning}"),
                    None => base,
                };

                // If any successful statement was DDL, schedule a
                // debounced schema refresh so the sidebar stays in
                // sync. The current session id is captured so the
                // refresh is suppressed if the user switches before
                // the debounce fires (bug C5).
                if ddl {
                    if let Some(id) = self.session.as_ref().map(|s| s.config.id) {
                        self.schedule_schema_refresh(id);
                    }
                }
            }
            RunUpdate::SchemaRefresh { session_id } => {
                let current = self.session.as_ref().map(|s| s.config.id);
                if current == Some(session_id) {
                    self.refresh_schema();
                } else {
                    tracing::debug!(
                        target = ?session_id,
                        current = ?current,
                        "discarding stale schema refresh; session changed"
                    );
                }
            }
        }
    }

    /// Handle a [`MetaUpdate`] delivered from the background metadata
    /// channel. This is the counterpart of [`Self::handle_run_update`]
    /// for non-query metadata operations (H11).
    pub fn handle_meta_update(&mut self, update: MetaUpdate) {
        match update {
            MetaUpdate::DumpSchemaReady { tab_id, tables } => {
                let Some(session) = self.session.as_ref() else {
                    self.status.message = "no active connection".into();
                    return;
                };
                let dialect = session.dialect();
                // C5: resolve the stable id to the *current* index;
                // if the originating tab was closed, drop the reply
                // with a status message instead of writing DDL into
                // an arbitrary tab.
                let Some(idx) = self.tabs.iter().position(|t| t.id() == tab_id) else {
                    tracing::debug!(
                        target: "narwhal::app",
                        tab_id,
                        "dropping dump-schema reply: originating tab closed"
                    );
                    self.status.message = "dump-schema cancelled: target tab was closed".into();
                    return;
                };
                let ddl = if tables.len() == 1 {
                    build_table_ddl(&tables[0], dialect)
                } else {
                    build_dump(&tables, dialect)
                };
                self.tabs[idx].editor.clear();
                self.tabs[idx].editor.insert_str(&ddl);
                self.status.message = format!(
                    "dump-schema: wrote {} table(s) into the editor buffer",
                    tables.len()
                );
                // Only switch focus / surface to the user if the user
                // is still on the originating tab; otherwise stay put.
                if idx == self.active_tab {
                    self.focus = Pane::Editor;
                }
            }
            MetaUpdate::SchemasRefreshed {
                session_id,
                schemas,
            } => {
                // H8: drop the reply if the user switched sessions since
                // the refresh was dispatched; otherwise we'd overwrite
                // the new session's listing with stale data.
                let current = self.session.as_ref().map(|s| s.config.id);
                if current != Some(session_id) {
                    tracing::debug!(
                        target: "narwhal::app",
                        ?session_id,
                        ?current,
                        "dropping stale SchemasRefreshed; session changed"
                    );
                    return;
                }
                if let Some(session) = self.session.as_mut() {
                    session.schemas = schemas;
                }
                self.rebuild_sidebar();
                let table_count = self.count_sidebar_tables();
                self.status.message = format!("schema refreshed · {table_count} tables");
            }
            MetaUpdate::SessionOpened { config_id, result } => {
                // H7: drop the reply if the user opened another
                // connection in the meantime (or hit `:close`).
                if !self.pending_session_opens.remove(&config_id) {
                    tracing::debug!(
                        target: "narwhal::app",
                        ?config_id,
                        "dropping SessionOpened: no pending open for this id"
                    );
                    return;
                }
                match result {
                    Ok(session) => {
                        self.apply_opened_session(*session);
                    }
                    Err(message) => {
                        self.status.connection = None;
                        self.status.message = format!("connect failed: {message}");
                    }
                }
            }
            MetaUpdate::HistoryReady { entries } => {
                if entries.is_empty() {
                    self.status.message = "no history entries".into();
                    return;
                }
                self.history_state = Some(HistoryState {
                    entries,
                    filter: String::new(),
                    selected: 0,
                });
                self.status.message =
                    "history: type to filter · Enter insert · Shift-Enter run · Esc close".into();
            }
            MetaUpdate::MetaFailed { message } => {
                self.status.message = message;
            }
        }
    }

    /// Send a metadata request to the background worker.
    ///
    /// Sprint 7 (LOW): the historical docstring promised a `false`
    /// return when no session was active or the channel was closed,
    /// but the body always returned `true`. The signature is kept for
    /// API stability — the return value is now intentionally
    /// `true` ("request was queued") and per-request gating is the
    /// caller's responsibility:
    ///
    /// - `LoadHistory` succeeds even without a session (handled by
    ///   the meta worker's own "no history journal" branch).
    /// - `RefreshSchemas` / `DumpSchemaAll` need a pool; the worker
    ///   surfaces `MetaFailed { message: "no active connection" }` if
    ///   the session disappeared between dispatch and execution.
    /// - `OpenSession` always succeeds at dispatch time.
    ///
    /// Callers that need pre-dispatch validation should check
    /// `self.session.is_some()` themselves before constructing the
    /// request.
    pub(super) fn dispatch_meta(&mut self, request: MetaRequest) -> bool {
        let pool = self.session.as_ref().map(|s| s.pool.clone());
        spawn_meta_request(
            request,
            pool,
            self.history_journal.clone(),
            Some(self.credentials.clone()),
            self.meta_tx.clone(),
        );
        true
    }

    /// Drive the worker channel to completion. Useful from tests after
    /// dispatching a batch: pumps every [`RunUpdate`] until `AllDone`.
    ///
    /// H7: also drains any pending `SessionOpened` meta replies
    /// up-front so tests that call `:open` and then expect the session
    /// to be live continue to work without an extra `drain_meta`
    /// step on every call site.
    pub async fn drain_run_updates(&mut self) {
        if !self.pending_session_opens.is_empty() {
            self.await_pending_session_opens().await;
        }
        while self.running {
            match self.recv_run_update().await {
                Some(update) => self.handle_run_update(update).await,
                None => break,
            }
        }
    }

    /// Non-blocking sibling of [`Self::await_pending_session_opens`].
    /// Drains any meta updates that are *already* sitting in the
    /// channel; does **not** wait if none are queued.
    pub fn drain_ready_meta_updates(&mut self) {
        while let Ok(update) = self.meta_rx.try_recv() {
            self.handle_meta_update(update);
        }
    }

    /// Sync wrapper around [`Self::await_pending_session_opens`] for
    /// use from `handle_key`. Uses `block_in_place` so the multi-thread
    /// runtime keeps draining other workers; the wait is bounded by
    /// the inner timeout in `await_pending_session_opens`.
    pub fn await_pending_session_opens_sync(&mut self) {
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(self.await_pending_session_opens());
            });
        }
    }

    /// Block until every pending `OpenSession` has been resolved
    /// (success or failure). Used by [`Self::drain_run_updates`] and
    /// exposed for direct use in tests that call `:open` outside the
    /// usual run-channel flow.
    pub async fn await_pending_session_opens(&mut self) {
        while !self.pending_session_opens.is_empty() {
            if let Ok(Some(update)) =
                tokio::time::timeout(Duration::from_secs(5), self.meta_rx.recv()).await
            {
                self.handle_meta_update(update);
            } else {
                // Channel closed or timed out — clear the ledger so
                // we don't spin forever.
                self.pending_session_opens.clear();
                break;
            }
        }
    }

    /// Like [`Self::drain_run_updates`] but also waits for any pending
    /// debounced schema refresh to fire. Useful in tests that need to
    /// observe the auto-refresh side-effect of a DDL statement.
    pub async fn drain_run_updates_and_refresh(&mut self) {
        self.drain_run_updates().await;
        // Wait for the debounce timer to fire (200ms + small slack).
        if self.refresh_task.is_some() {
            tokio::time::sleep(Duration::from_millis(350)).await;
            // The debounce task sends SchemaRefresh through run_rx;
            // consume it.
            while let Ok(update) = self.run_rx.try_recv() {
                self.handle_run_update(update).await;
            }
        }
        // The SchemaRefresh handler now dispatches a MetaRequest::RefreshSchemas
        // to the background channel. Drain that too.
        self.drain_meta_updates().await;
    }

    /// Drain pending metadata updates from the meta channel.
    /// Useful in tests after dispatching a meta request
    /// (e.g. `refresh_schemas`, `dump_schema all`, `open_history`).
    ///
    /// Waits up to 2 seconds for at least one update to arrive, then
    /// drains the channel completely.
    pub async fn drain_meta_updates(&mut self) {
        // Wait for the background task to produce at least one result.
        if let Some(update) = tokio::time::timeout(Duration::from_secs(2), self.meta_rx.recv())
            .await
            .ok()
            .flatten()
        {
            self.handle_meta_update(update);
        }
        // Drain any remaining.
        while let Ok(update) = self.meta_rx.try_recv() {
            self.handle_meta_update(update);
        }
    }

    /// Run every loaded plugin's `transform_result` hook over the rows
    /// just produced by a row-returning statement. EXPLAIN output and
    /// `Affected`-only results skip this path because mutating them
    /// would defeat the purpose of those views.
    ///
    /// Failures from a transform are reported to the status bar but the
    /// original (untransformed) rows are still surfaced — a misbehaving
    /// plugin shouldn't be able to hide query data from the user.
    /// Issue B / H7 (sprint 5): the previous implementation used
    /// `block_in_place` + `Handle::block_on` to bridge the synchronous
    /// `handle_run_update` path to the async plugin trait. Now that
    /// `handle_run_update` is itself `async`, we await the transform
    /// directly — no scheduler-thread blockage, no current-thread
    /// runtime deadlock risk.
    async fn apply_plugin_transforms(
        &mut self,
        columns: Vec<ColumnHeader>,
        rows: Vec<Row>,
        elapsed_ms: u64,
    ) -> (Vec<ColumnHeader>, Vec<Row>) {
        if self.plugins.plugins().is_empty() {
            return (columns, rows);
        }
        let plugins = Arc::clone(&self.plugins);
        let mut qr = narwhal_core::QueryResult {
            columns,
            rows,
            rows_affected: None,
            elapsed_ms,
        };
        let result = plugins.transform_result(&mut qr).await;
        if let Err(errors) = result {
            // The registry already ran every plugin's transform regardless
            // of intermediate failures (see PluginRegistry::transform_result),
            // so `qr` reflects whatever each transform managed in place.
            // Surface every failure at once — the AllDone status message
            // would otherwise clobber it.
            self.plugin_warning = Some(format!("plugin transform failed: {errors}"));
        }
        (qr.columns, qr.rows)
    }

    async fn finalize_statement(
        &mut self,
        elapsed_ms: u64,
        rows_returned: usize,
        rows_affected: Option<u64>,
        streamed: bool,
    ) {
        let rt = self.run_tab_index();
        let (columns, rows, index, total, sql) =
            match std::mem::take(self.tabs[rt].results.active_state_mut()) {
                ResultState::Running {
                    columns,
                    rows,
                    index,
                    total,
                    sql,
                    ..
                } => (columns, rows, index, total, sql),
                other => {
                    *self.tabs[rt].results.active_state_mut() = other;
                    return;
                }
            };
        if columns.is_empty() {
            *self.tabs[rt].results.active_state_mut() = ResultState::Affected {
                rows: rows_affected.unwrap_or(0),
                elapsed_ms,
                index,
                total,
            };
        } else if is_explain_result(&columns) {
            match extract_explain_plan(&rows) {
                Ok(plan) => {
                    *self.tabs[rt].results.active_state_mut() = ResultState::Explain {
                        lines: plan
                            .lines
                            .into_iter()
                            .map(|l| ExplainPlanLine {
                                depth: l.depth,
                                text: l.text,
                            })
                            .collect(),
                        planning_time_ms: plan.planning_time_ms,
                        execution_time_ms: plan.execution_time_ms,
                    };
                    self.status.message = format!("explain ok · {elapsed_ms} ms");
                    return;
                }
                Err(error) => {
                    *self.tabs[rt].results.active_state_mut() = ResultState::Error {
                        message: format!("explain parse failed: {error}"),
                        elapsed_ms,
                    };
                    return;
                }
            }
        } else {
            // Take the pending row source (set by preview_sidebar_selection)
            // and attach it to the result so cell edits can target the
            // originating table.
            let source = self.tabs[rt].pending_source.take();
            let source_table = crate::export::extract_source_table(&sql);
            let (columns, rows) = self
                .apply_plugin_transforms(columns, rows, elapsed_ms)
                .await;
            *self.tabs[rt].results.active_state_mut() = ResultState::Rows {
                columns,
                rows,
                elapsed_ms,
                streamed,
                index,
                total,
                source,
                source_table,
            };
        }
        self.status.message = match rows_affected {
            Some(n) => format!("ok {index}/{total} · {n} affected · {elapsed_ms} ms"),
            None => format!("ok {index}/{total} · {rows_returned} rows · {elapsed_ms} ms"),
        };
    }
}
