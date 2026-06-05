//! Terminal-bound application entry point.
//!
//! [`App`] owns a [`crate::core::AppCore`] and wires it to a real terminal:
//! it enters raw mode, reads crossterm events, drives the run-update channel,
//! and renders on every iteration. All non-IO behaviour lives in
//! [`crate::core::AppCore`].

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use narwhal_config::{ConnectionsFile, CredentialStore, Settings, VaultRegistry};
use narwhal_history::Journal;
use tokio::time::sleep_until;
use tracing::{debug, info};

use crate::clipboard::Clipboard;
use crate::core::AppCore;
use crate::draw_scheduler::{DrawDecision, DrawScheduler, DrawTrigger};
use crate::persist;
use crate::registry::DriverRegistry;
use crate::run::RunUpdate;
use crate::terminal::TerminalGuard;

pub struct App {
    core: AppCore,
    /// T1-T3-B: workspace-state snapshot file path, if persistence
    /// is wired up. Populated via
    /// [`App::with_workspace_state_path`]; left `None` in headless
    /// tests and the MCP server (neither persists tabs).
    workspace_state_path: Option<std::path::PathBuf>,
    /// T1-T3-B: cached persist toggles, snapshotted from
    /// [`Settings::workspace::persist`] when
    /// [`App::with_settings`] is called. Drives whether
    /// [`App::run`] writes a snapshot on clean exit.
    persist_settings: narwhal_config::WorkspacePersistSettings,
    /// T1-T3-B: connection name surfaced by
    /// [`crate::persist::apply`] at startup. Re-opened
    /// asynchronously on the first tick of the event loop so the
    /// initial render isn't blocked on a network dial.
    pending_restore_connection: Option<String>,
}

impl App {
    pub fn new(
        registry: DriverRegistry,
        connections: ConnectionsFile,
        history: Option<Arc<Journal>>,
    ) -> Self {
        Self {
            core: AppCore::new(registry, connections, history),
            workspace_state_path: None,
            persist_settings: narwhal_config::WorkspacePersistSettings::default(),
            pending_restore_connection: None,
        }
    }

    /// Construct an [`App`] that uses the supplied credential store. The
    /// binary passes a [`narwhal_config::KeyringStore`]; tests may pass an
    /// in-memory store.
    pub fn with_credentials(
        registry: DriverRegistry,
        connections: ConnectionsFile,
        history: Option<Arc<Journal>>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        Self {
            core: AppCore::with_credentials(registry, connections, history, credentials),
            workspace_state_path: None,
            persist_settings: narwhal_config::WorkspacePersistSettings::default(),
            pending_restore_connection: None,
        }
    }

    /// Inject every replaceable runtime service in one call. See
    /// [`AppCore::with_services`].
    pub fn with_services(
        registry: DriverRegistry,
        connections: ConnectionsFile,
        history: Option<Arc<Journal>>,
        credentials: Arc<dyn CredentialStore>,
        clipboard: Arc<dyn Clipboard>,
    ) -> Self {
        Self {
            core: AppCore::with_services(registry, connections, history, credentials, clipboard),
            workspace_state_path: None,
            persist_settings: narwhal_config::WorkspacePersistSettings::default(),
            pending_restore_connection: None,
        }
    }

    /// Override the persistence location for connections produced via the
    /// `:add` wizard. Should be called immediately after [`Self::new`].
    pub fn with_connections_path(mut self, path: std::path::PathBuf) -> Self {
        self.core.set_connections_path(path);
        self
    }

    /// Wire the per-connection recency cache so the sidebar can show
    /// most-recently-opened connections first. Safe to skip in tests;
    /// without it the ordering falls back to alphabetical.
    pub fn with_last_used_path(mut self, path: std::path::PathBuf) -> Self {
        self.core.set_last_used_path(path);
        self
    }

    /// T1-T2-B: install the secret-vault provider registry. The
    /// binary builds this from `settings.vault.providers`; tests
    /// usually leave it empty (the default).
    #[must_use]
    pub fn with_vault(mut self, vault: Arc<VaultRegistry>) -> Self {
        self.core.set_vault(vault);
        self
    }

    /// T2-T2-D: install an [`narwhal_audit::AuditService`]. The
    /// binary builds this from `settings.audit` and calls this method
    /// when one or more sinks are configured. Without this call, emit
    /// sites short-circuit and the audit log is silent.
    #[must_use]
    pub fn with_audit_service(mut self, svc: Arc<narwhal_audit::AuditService>) -> Self {
        self.core.set_audit_service(svc);
        self
    }

    /// Apply a user-supplied [`Settings`] payload. Currently the only
    /// field that takes effect at runtime is `theme`; the remaining
    /// `editor` / `keybindings` fields are accepted and persisted but
    /// will be honoured in a follow-up release (see the v1.0 release
    /// notes for the planned activation timeline).
    pub fn with_settings(mut self, settings: Settings) -> Self {
        // T1-T3-B: capture the persist toggles before `apply_settings`
        // consumes the payload. Clone is cheap (four bools) and
        // keeps the field accessible from `App::run`.
        self.persist_settings = settings.workspace.persist.clone();
        self.core.apply_settings(settings);
        self
    }

    /// T1-T3-B: point the persist layer at a `workspace-state.toml`
    /// path and replay any snapshot found there onto the live
    /// [`AppCore`]. Must be called *before* the user has opened a
    /// connection or edited a tab — the binary's entry point
    /// (`narwhal::main`) does this immediately after
    /// [`Self::with_settings`].
    ///
    /// A missing file, a malformed file, or a forward-version file
    /// all degrade to "no restore"; the launch never fails because
    /// of persistence. Errors surface as `tracing::warn` log lines so
    /// operators can investigate.
    #[must_use]
    pub fn with_workspace_state_path(mut self, path: std::path::PathBuf) -> Self {
        self.workspace_state_path = Some(path.clone());
        if !self.persist_settings.enabled {
            tracing::debug!(
                target: "narwhal::persist",
                "workspace persist disabled in settings; skip restore"
            );
            return self;
        }
        match persist::load_at_start(&path) {
            Ok(None) => {
                tracing::debug!(
                    target: "narwhal::persist",
                    path = %path.display(),
                    "no workspace-state file; first run",
                );
            }
            Ok(Some(snapshot)) => {
                let pending = persist::apply(&mut self.core, snapshot, &self.persist_settings);
                self.pending_restore_connection = pending;
                tracing::info!(
                    target: "narwhal::persist",
                    path = %path.display(),
                    pending_connection = ?self.pending_restore_connection,
                    "restored workspace state",
                );
            }
            Err(err) => {
                tracing::warn!(
                    target: "narwhal::persist",
                    path = %path.display(),
                    error = %err,
                    "workspace-state restore skipped",
                );
            }
        }
        self
    }

    /// Auto-load every `*.lua` file in `dir`. See
    /// [`AppCore::auto_load_plugins`] for details.
    pub fn with_plugins_dir(mut self, dir: &std::path::Path) -> Self {
        self.core.auto_load_plugins(dir);
        self
    }

    /// L36 #11: refuse every row-level mutation. The TUI still loads
    /// and the user can still issue freeform SELECTs, but the pending
    /// pipeline (o/O/d/cell edit) bails with a banner instead of
    /// queueing a change. Set by the CLI flag `--read-only`.
    #[must_use]
    pub const fn with_read_only(mut self, on: bool) -> Self {
        self.core.set_read_only(on);
        self
    }

    pub async fn run(mut self) -> Result<()> {
        let mut guard = TerminalGuard::enter()?;
        let mut events = EventStream::new();

        info!(target: "narwhal::app", "event loop started");
        // T1-T3-B: kick off the restored-connection re-open *before*
        // the first draw so the initial frame can already show the
        // "connecting to …" status line. The dispatcher handles a
        // missing/renamed connection name gracefully (sets a
        // status-bar warning).
        if let Some(name) = self.pending_restore_connection.take() {
            tracing::debug!(
                target: "narwhal::persist",
                connection = %name,
                "re-opening restored connection",
            );
            self.core.reopen_restored_connection(&name).await;
        }
        self.draw(&mut guard)?;
        let mut scheduler = DrawScheduler::new(Instant::now());

        while !self.core.should_quit() {
            // Far-future sentinel when no deferred draw is pending; the
            // sleep arm of select! parks indefinitely. When a stream
            // update has been coalesced, wake at the throttle deadline
            // so the trailing flush draws the final batch.
            let deadline = scheduler
                .deadline()
                .unwrap_or_else(|| Instant::now() + std::time::Duration::from_secs(3600));
            let trigger = tokio::select! {
                event = events.next() => {
                    match event {
                        Some(Ok(ev)) => {
                            self.handle_event(ev).await;
                            Some(DrawTrigger::Force)
                        }
                        Some(Err(error)) => {
                            tracing::error!(target: "narwhal::app", error = %error, "event read failed");
                            break;
                        }
                        None => break,
                    }
                }
                Some(update) = self.core.run_rx.recv() => {
                    let is_stream = matches!(update, RunUpdate::RowsAppended { .. });
                    self.core.handle_run_update(update).await;
                    Some(if is_stream { DrawTrigger::Stream } else { DrawTrigger::Force })
                }
                Some(meta) = self.core.meta_rx.recv() => {
                    self.core.handle_meta_update(meta);
                    Some(DrawTrigger::Force)
                }
                () = sleep_until(deadline.into()) => None,
            };

            let now = Instant::now();
            let decision = match trigger {
                Some(t) => scheduler.on_event(t, now),
                None => scheduler.on_tick(now),
            };
            if matches!(decision, DrawDecision::DrawNow) {
                self.draw(&mut guard)?;
            }
        }

        info!(target: "narwhal::app", "event loop terminated");
        // T1-T3-B: clean-exit snapshot. Panic unwinds never reach
        // this point (the run loop would have propagated the
        // panic), which matches the brief's "save on clean exit
        // only" requirement. All failures here are logged but
        // swallowed — a broken snapshot must not block teardown.
        if let Some(path) = self.workspace_state_path.clone() {
            if self.persist_settings.enabled {
                let snapshot = persist::snapshot(&self.core);
                match persist::save_at_exit(&snapshot, &path) {
                    Ok(persist::SaveOutcome::Canonical(p)) => {
                        tracing::info!(
                            target: "narwhal::persist",
                            path = %p.display(),
                            "workspace-state snapshot written",
                        );
                    }
                    Ok(persist::SaveOutcome::PerPid(p)) => {
                        tracing::info!(
                            target: "narwhal::persist",
                            path = %p.display(),
                            "workspace-state lock contended; wrote per-pid snapshot",
                        );
                    }
                    Err(err) => {
                        tracing::warn!(
                            target: "narwhal::persist",
                            error = %err,
                            "workspace-state snapshot save failed",
                        );
                    }
                }
            } else {
                tracing::debug!(
                    target: "narwhal::persist",
                    "workspace persist disabled; not writing snapshot",
                );
            }
        }
        Ok(())
    }

    fn draw(&mut self, guard: &mut TerminalGuard) -> Result<()> {
        guard
            .terminal
            .draw(|frame| self.core.render(frame, frame.area()))?;
        Ok(())
    }

    async fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                self.core.handle_key(key).await;
            }
            Event::Mouse(m) => self.core.handle_mouse(m).await,
            Event::Resize(_, _) => debug!(target: "narwhal::app", "terminal resized"),
            // Sprint 7 (LOW): bracketed-paste support. crossterm emits
            // `Event::Paste(s)` for OSC 200 paste sequences when paste
            // mode is enabled at terminal init. Route the payload
            // straight into the editor so multi-line pastes preserve
            // their newlines instead of being interpreted as `Enter`
            // keystrokes one-by-one (which would trip motion handlers
            // and modal commands). Other panes ignore paste — only
            // the editor accepts text input today.
            Event::Paste(text) => self.core.editor_paste(&text).await,
            _ => {}
        }
    }
}
