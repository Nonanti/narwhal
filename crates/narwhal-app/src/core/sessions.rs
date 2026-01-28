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

use super::{AppCore, ResultBundle, ResultState, ResultView, SidebarItem};
use crate::meta::MetaRequest;
use crate::run::{spawn_run, RunContext, RunMode, RunRequest, RunTarget, RunUpdate};
use crate::session::Session;
use crate::statements::{all_statements, statement_at_cursor};
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
        // Keyring miss → fall back to the same env / `~/.pgpass`
        // lookups that `psql` honours. Keeps the keyring as the
        // primary store while letting users who already curate a
        // pgpass file connect without re-entering passwords.
        let plain_password = plain_password
            .or_else(|| narwhal_config::resolve_fallback_password(&config.driver, &config.params));
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
        let password_for_open = password;
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
                    let mut state = self
                        .plugin_state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.pool = Some(session.pool.clone());
                    state.in_transaction = false;
                }
                let opened_id = session.config.id;
                self.session = Some(session);
                self.touch_last_used(opened_id);
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
            let mut state = self
                .plugin_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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

    pub(super) fn dispatch_current_statement(&mut self, mode: RunMode) {
        let Some(session) = self.session.as_ref() else {
            self.status.message = "no active connection".into();
            return;
        };
        let Some(sql) = statement_at_cursor(&self.tabs[self.active_tab].editor, session.dialect())
        else {
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
        let statements = all_statements(&self.tabs[self.active_tab].editor, session.dialect());
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

    /// Pre-fill the wizard from a connection URL. The user still gets to
    /// tweak the form (and *must* fill in `name`, which the URL doesn't
    /// carry) before committing. Existing IDs aren't touched — the
    /// wizard always allocates a fresh id at commit time.
    pub(super) fn start_wizard_from_url(&mut self, dsn: &str) {
        let parsed = match narwhal_config::parse_url(dsn) {
            Ok(p) => p,
            Err(error) => {
                self.status.message = format!("url: {error}");
                return;
            }
        };
        // Override the parser-generated `host:port/db` label with the
        // bare database name so the user sees a friendly slug in the
        // sidebar; they can still rename before pressing Enter.
        let mut config = parsed.config;
        if let Some(db) = config.params.database.clone() {
            config.name = db;
        } else if let Some(host) = config.params.host.clone() {
            config.name = host;
        }
        let password = parsed
            .password
            .map(|p| secrecy::SecretString::new(p.into_boxed_str()));
        self.wizard = Some(ConnectionWizard::from_config(&config, password, None));
        self.wizard_error = None;
        self.status.message = "url: review fields · Tab moves · Enter saves · Esc cancels".into();
    }

    /// Open the wizard with every field pre-populated from an existing
    /// saved connection. The stored password (if any) is fetched from
    /// the keyring and slotted in as a `SecretString`. Committing the
    /// wizard rewrites the entry in place via `existing_id`.
    pub(super) fn start_wizard_edit(&mut self, name: &str) {
        let Some(config) = self
            .connections
            .connections
            .iter()
            .find(|c| c.name == name)
            .cloned()
        else {
            self.status.message = format!("edit: no connection named '{name}'");
            return;
        };
        let password = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.credentials.get(config.id))
        })
        .ok()
        .flatten();
        let existing_id = Some(config.id);
        self.wizard = Some(ConnectionWizard::from_config(
            &config,
            password,
            existing_id,
        ));
        self.wizard_error = None;
        self.status.message =
            format!("edit '{name}': Tab moves · ←/→ driver · Enter saves · Esc cancels");
    }

    /// Attempt a transient connection and close it immediately. With no
    /// argument, pings the active session; with an argument, looks the
    /// name up in `connections.toml` (or parses the argument as a URL)
    /// and opens a one-shot session.
    pub(super) fn test_connection(&mut self, target: Option<&str>) {
        let Some(target) = target else {
            // No argument: ping the active session by opening a fresh pool
            // connection. We only check that we can acquire it.
            let Some(session) = self.session.as_ref() else {
                self.status.message = "test: no active connection (§ :test <name|url>)".into();
                return;
            };
            let label = session.config.name.clone();
            let pool = session.pool.clone();
            let outcome = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async move { pool.acquire().await })
            });
            match outcome {
                Ok(_) => self.status.message = format!("test ok: {label}"),
                Err(e) => self.status.message = format!("test failed: {label} — {e}"),
            }
            return;
        };

        // URL form takes precedence over name lookups so users can
        // sanity-check a DSN before saving it.
        let (config, password) = if target.contains("://") || target.starts_with("sqlite:") {
            match narwhal_config::parse_url(target) {
                Ok(p) => (p.config, p.password),
                Err(error) => {
                    self.status.message = format!("test: invalid url: {error}");
                    return;
                }
            }
        } else {
            let Some(config) = self
                .connections
                .connections
                .iter()
                .find(|c| c.name == target)
                .cloned()
            else {
                self.status.message = format!("test: connection not found: {target}");
                return;
            };
            let password = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(self.credentials.get(config.id))
            })
            .ok()
            .flatten()
            .map(|s| s.expose_secret().to_owned());
            (config, password)
        };

        let Ok(driver) = self.registry.get(&config.driver) else {
            self.status.message = format!("test: driver not registered: {}", config.driver);
            return;
        };
        let driver = driver.clone();
        let label = if config.name.is_empty() {
            target.to_owned()
        } else {
            config.name.clone()
        };
        self.status.message = format!("testing {label}…");
        // Open a transient session and drop it on the spot. Session::open
        // already performs the handshake, so a successful return is the
        // signal that credentials + network are fine.
        let outcome = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { Session::open(driver, config, password).await })
        });
        match outcome {
            Ok(session) => {
                let driver_name = session.driver.name().to_owned();
                drop(session);
                self.status.message = format!("test ok: {label} · {driver_name}");
            }
            Err(e) => {
                self.status.message = format!("test failed: {label} — {e}");
            }
        }
    }

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
        // Drop the recency entry so the cache doesn't leak tombstones
        // and the next-sort run doesn't trip over a stale id.
        self.last_used.forget(removed.id);
        if let Some(path) = self.last_used_path.as_ref() {
            if let Err(error) = self.last_used.save(path) {
                debug!(target: "narwhal::app", error = %error, "last-used save failed during remove");
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
                let mut state = self
                    .plugin_state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
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
