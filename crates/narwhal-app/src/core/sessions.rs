//! Session lifecycle + statement dispatch extracted from `core.rs` (L21).
//!
//! Handles `:open`/`:close`/`:remove`/`:forget`/`:refresh`/`:add`,
//! the per-statement run dispatch (`Command::Run`, `Command::RunAll`,
//! mouse double-click) and the debounced schema-refresh timer.
use std::sync::atomic::Ordering;
use std::time::Instant;

use narwhal_core::ConnectionConfig;
use narwhal_tui::Pane;
use secrecy::ExposeSecret;
use tracing::debug;
use uuid::Uuid;

use crate::editor::{all_statements, statement_at_cursor};
use super::{AppCore, ResultBundle, ResultState, ResultView, SidebarItem};
use crate::meta::MetaRequest;
use crate::run::{spawn_run, RunContext, RunMode, RunRequest, RunTarget, RunUpdate};
use crate::session::Session;
use crate::wizard::ConnectionWizard;

impl AppCore {
    pub(super) fn open_named(&mut self, target: &str) {
        if target.contains("://") || target.starts_with("sqlite:") {
            match narwhal_config::parse_url(target) {
                Ok(parsed) => {
                    self.open_connection_with_password(parsed.config, parsed.password);
                }
                Err(error) => {
                    self.status.message = format!("invalid url: {error}");
                }
            }
            return;
        }
        let Some(config) = self
            .connections
            .connections
            .iter()
            .find(|c| c.name == target)
            .cloned()
        else {
            self.status.message = format!("connection not found: {target}");
            return;
        };
        self.open_connection(config);
    }

    fn open_connection(&mut self, config: ConnectionConfig) {
        let password = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.credentials.get(config.id))
        });
        let password = match password {
            Ok(secret) => secret,
            Err(error) => {
                debug!(target: "narwhal::app", error = %error, "keyring lookup failed; continuing without password");
                None
            }
        };
        // Convert SecretString → Option<String> for the Session::open API.
        // The driver layer still expects plain String; we expose the secret
        // only here and let it drop naturally after the call.
        let plain_password = password.map(|s| s.expose_secret().to_owned());
        self.open_connection_with_password(config, plain_password);
    }

    fn open_connection_with_password(
        &mut self,
        config: ConnectionConfig,
        password: Option<String>,
    ) {
        let Ok(driver) = self.registry.get(&config.driver) else {
            self.status.message = format!("driver not registered: {}", config.driver);
            return;
        };
        let label = config.name.clone();
        self.status.message = format!("connecting to {label}…");

        let driver = driver.clone();
        let password_for_open = password.clone();
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { Session::open(driver, config, password_for_open).await })
        });
        match result {
            Ok(mut session) => {
                if let Err(error) = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(session.refresh_schemas())
                }) {
                    debug!(target: "narwhal::app", error = %error, "schema refresh on connect failed");
                }
                self.status.connection = Some(format!(
                    "{} · {}",
                    session.config.name,
                    session.driver.name()
                ));
                self.status.message = format!(
                    "connected · {} · {}",
                    session.config.name,
                    session.driver.name()
                );
                // Publish the new pool to the plugin executor so any
                // `narwhal.sql_run` calls hit the freshly-opened
                // connection. Opening a connection always closes any
                // prior `:begin` state implicitly, so the TX flag
                // resets here too.
                {
                    let mut state = self.plugin_state.lock().expect("plugin_state poisoned");
                    state.pool = Some(session.pool.clone());
                    state.in_transaction = false;
                }
                self.session = Some(session);
                self.rebuild_sidebar();
                self.focus = Pane::Editor;
            }
            Err(error) => {
                self.status.message = format!("connect failed: {error}");
            }
        }
    }

    pub(super) fn close_session(&mut self) {
        if self.session.take().is_some() {
            let mut state = self.plugin_state.lock().expect("plugin_state poisoned");
            state.pool = None;
            state.in_transaction = false;
            drop(state);
            self.status.connection = None;
            self.status.transaction = None;
            self.status.message = "connection closed".into();
            self.rebuild_sidebar();
        }
    }

    pub(super) fn refresh_schema(&mut self) {
        let Some(_session) = self.session.as_ref() else {
            self.status.message = "no active connection".into();
            return;
        };
        // H11: Offload to the meta channel so the UI stays responsive
        // during schema refreshes on databases with many schemas/tables.
        self.dispatch_meta(MetaRequest::RefreshSchemas);
        self.status.message = "refreshing schema…".into();
    }

    /// Count the number of tables currently shown in the sidebar.
    pub(super) fn count_sidebar_tables(&self) -> usize {
        self.sidebar_items
            .iter()
            .filter(|item| matches!(item, SidebarItem::Table { .. }))
            .count()
    }

    /// Schedule a debounced schema refresh against `session_id`. Each
    /// call resets the 200ms timer; the refresh fires once the timer
    /// expires without being rescheduled. A migration with 50 DDL
    /// statements fires exactly one refresh.
    ///
    /// The `session_id` is round-tripped through the
    /// [`RunUpdate::SchemaRefresh`] payload so the handler can drop
    /// the notification if the user has switched sessions in the
    /// meantime (bug C5).
    pub(super) fn schedule_schema_refresh(&mut self, session_id: Uuid) {
        // Release so the spawned task's Acquire swap sees this store
        // even on weakly-ordered architectures (ARM64, POWER).  (Y4-B fix.)
        self.refresh_pending.store(true, Ordering::Release);
        // Drop the previous task if any — aborting cancels its sleep.
        if let Some(handle) = self.refresh_task.take() {
            handle.abort();
        }
        let tx = self.run_tx.clone();
        let pending = self.refresh_pending.clone();
        self.refresh_task = Some(
            tokio::spawn(async move {
                tokio::time::sleep(narwhal_tui::constants::SCHEMA_REFRESH_DEBOUNCE).await;
                if pending.swap(false, Ordering::Acquire) {
                    let _ = tx.send(RunUpdate::SchemaRefresh { session_id }).await;
                }
            })
            .abort_handle(),
        );
    }

    // ----- dispatch -----

    pub(super) fn dispatch_current_statement(&mut self, mode: RunMode) {
        let Some(session) = self.session.as_ref() else {
            self.status.message = "no active connection".into();
            return;
        };
        let Some(sql) = statement_at_cursor(
            &self.tabs[self.active_tab].editor,
            session.dialect(),
        ) else {
            self.status.message = "no statement under cursor".into();
            return;
        };
        let trimmed = sql.trim().trim_end_matches(';').trim().to_owned();
        if trimmed.is_empty() {
            self.status.message = "no statement under cursor".into();
            return;
        }
        self.dispatch_batch(vec![trimmed], mode);
    }

    pub(super) fn dispatch_all_statements(&mut self, mode: RunMode) {
        let Some(session) = self.session.as_ref() else {
            self.status.message = "no active connection".into();
            return;
        };
        let statements = all_statements(
            &self.tabs[self.active_tab].editor,
            session.dialect(),
        );
        if statements.is_empty() {
            self.status.message = "buffer contains no statements".into();
            return;
        }
        self.dispatch_batch(statements, mode);
    }

    pub(super) fn dispatch_batch(&mut self, statements: Vec<String>, mode: RunMode) {
        if self.running {
            self.status.message = "a query is already running".into();
            return;
        }
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let target = match session.transaction.as_ref() {
            Some(txn) => RunTarget::Pinned(txn.conn.clone()),
            None => RunTarget::Pool(session.pool.clone()),
        };
        let ctx = RunContext {
            target,
            history: self.history_journal.clone(),
            connection_id: session.config.id,
            connection_name: session.config.name.clone(),
            driver: session.driver.name().to_owned(),
        };
        let request = RunRequest { statements, mode };
        self.running = true;
        self.run_tab = Some(self.active_tab);
        self.pending_result_entries_states.clear();
        self.pending_result_entries_views.clear();
        self.tabs[self.active_tab].row_detail = None;
        let now = Instant::now();
        // Reset the bundle to a single empty entry for the running state.
        self.tabs[self.active_tab].results = ResultBundle::single(
            ResultState::Running {
                sql: String::new(),
                index: 0,
                total: request.statements.len(),
                columns: Vec::new(),
                rows: Vec::new(),
                streaming: matches!(mode, RunMode::Stream),
                started_at: now,
                last_render: now,
            },
            ResultView::new(),
        );
        self.status.message = match mode {
            RunMode::Execute => "executing…".into(),
            RunMode::Stream => "streaming…".into(),
        };
        spawn_run(ctx, request, self.cancel_slot.clone(), self.run_tx.clone());
    }

    pub(super) fn start_wizard(&mut self) {
        self.wizard = Some(ConnectionWizard::new());
        self.wizard_error = None;
        self.status.message = "add: Tab moves · ←/→ driver · Enter saves · Esc cancels".into();
    }

    // ----- transactions -----
    // Transaction methods (begin/commit/rollback/savepoint/release/
    // rollback_to_savepoint, with_txn_conn) moved to
    // `core::transactions` (L21).

    pub(super) fn remove_connection(&mut self, name: &str) {
        let Some(pos) = self
            .connections
            .connections
            .iter()
            .position(|c| c.name == name)
        else {
            self.status.message = format!("remove: no connection named '{name}'");
            return;
        };
        let removed = self.connections.connections.remove(pos);
        if let Some(path) = self.connections_path.as_ref() {
            if let Err(error) = self.connections.save(path) {
                // Restore in-memory state so we don't drift from disk.
                self.connections.connections.insert(pos, removed);
                self.status.message = format!("remove failed: {error}");
                return;
            }
        }
        if let Err(error) = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.credentials.delete(removed.id))
        }) {
            debug!(target: "narwhal::app", error = %error, "keyring delete failed during remove");
        }
        if let Some(session) = self.session.as_ref() {
            if session.config.id == removed.id {
                self.session = None;
                let mut state = self.plugin_state.lock().expect("plugin_state poisoned");
                state.pool = None;
                state.in_transaction = false;
            }
        }
        self.rebuild_sidebar();
        self.status.message = format!("removed connection '{name}'");
    }

    pub(super) fn forget_password(&mut self, name: &str) {
        let Some(config) = self.connections.connections.iter().find(|c| c.name == name) else {
            self.status.message = format!("forget: no connection named '{name}'");
            return;
        };
        match tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.credentials.delete(config.id))
        }) {
            Ok(()) => {
                self.status.message = format!("forgot password for '{name}'");
            }
            Err(error) => {
                self.status.message = format!("forget failed: {error}");
            }
        }
    }

}
