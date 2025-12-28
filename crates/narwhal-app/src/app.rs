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
use narwhal_config::{ConnectionsFile, CredentialStore};
use narwhal_history::Journal;
use tokio::time::sleep_until;
use tracing::{debug, info};

use crate::clipboard::Clipboard;
use crate::core::AppCore;
use crate::draw_scheduler::{DrawDecision, DrawScheduler, DrawTrigger};
use crate::registry::DriverRegistry;
use crate::run::RunUpdate;
use crate::terminal::TerminalGuard;

pub struct App {
    core: AppCore,
}

impl App {
    pub fn new(
        registry: DriverRegistry,
        connections: ConnectionsFile,
        history: Option<Arc<Journal>>,
    ) -> Self {
        Self {
            core: AppCore::new(registry, connections, history),
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
        }
    }

    /// Override the persistence location for connections produced via the
    /// `:add` wizard. Should be called immediately after [`Self::new`].
    pub fn with_connections_path(mut self, path: std::path::PathBuf) -> Self {
        self.core.set_connections_path(path);
        self
    }

    /// Auto-load every `*.lua` file in `dir`. See
    /// [`AppCore::auto_load_plugins`] for details.
    pub fn with_plugins_dir(mut self, dir: &std::path::Path) -> Self {
        self.core.auto_load_plugins(dir);
        self
    }

    pub async fn run(mut self) -> Result<()> {
        let mut guard = TerminalGuard::enter()?;
        let mut events = EventStream::new();

        info!(target: "narwhal::app", "event loop started");
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
                            self.handle_event(ev);
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
                    self.core.handle_run_update(update);
                    Some(if is_stream { DrawTrigger::Stream } else { DrawTrigger::Force })
                }
                Some(meta) = self.core.meta_rx.recv() => {
                    self.core.handle_meta_update(meta);
                    Some(DrawTrigger::Force)
                }
                _ = sleep_until(deadline.into()) => None,
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
        Ok(())
    }

    fn draw(&mut self, guard: &mut TerminalGuard) -> Result<()> {
        guard
            .terminal
            .draw(|frame| self.core.render(frame, frame.area()))?;
        Ok(())
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.core.handle_key(key),
            Event::Mouse(m) => self.core.handle_mouse(m),
            Event::Resize(_, _) => debug!(target: "narwhal::app", "terminal resized"),
            _ => {}
        }
    }
}
