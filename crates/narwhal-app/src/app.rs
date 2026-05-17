//! Terminal-bound application entry point.
//!
//! [`App`] owns a [`crate::core::AppCore`] and wires it to a real terminal:
//! it enters raw mode, reads crossterm events, drives the run-update channel,
//! and renders on every iteration. All non-IO behaviour lives in
//! [`crate::core::AppCore`].

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use narwhal_config::ConnectionsFile;
use narwhal_history::Journal;
use std::sync::Arc;
use tracing::{debug, info};

use crate::core::AppCore;
use crate::registry::DriverRegistry;
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

    pub async fn run(mut self) -> Result<()> {
        let mut guard = TerminalGuard::enter()?;
        let mut events = EventStream::new();

        info!(target: "narwhal::app", "event loop started");
        self.draw(&mut guard)?;

        while !self.core.should_quit() {
            tokio::select! {
                event = events.next() => {
                    match event {
                        Some(Ok(ev)) => self.handle_event(ev),
                        Some(Err(error)) => {
                            tracing::error!(target: "narwhal::app", error = %error, "event read failed");
                            break;
                        }
                        None => break,
                    }
                }
                Some(update) = self.core.run_rx.recv() => {
                    self.core.handle_run_update(update);
                }
            }
            self.draw(&mut guard)?;
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
            Event::Resize(_, _) => debug!(target: "narwhal::app", "terminal resized"),
            _ => {}
        }
    }
}
