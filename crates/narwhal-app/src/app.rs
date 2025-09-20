//! Top-level application state and event loop.

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use narwhal_tui::{render_root, translate_key_event, RootLayout, Theme};
use narwhal_vim::{Action, Mode, Vim};
use tracing::{debug, info};

use crate::registry::DriverRegistry;
use crate::terminal::TerminalGuard;

pub struct App {
    pub registry: DriverRegistry,
    pub vim: Vim,
    pub theme: Theme,
    pub status_message: String,
    pub should_quit: bool,
}

impl App {
    pub fn new(registry: DriverRegistry) -> Self {
        Self {
            registry,
            vim: Vim::new(),
            theme: Theme::default(),
            status_message: "ready".into(),
            should_quit: false,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        let mut guard = TerminalGuard::enter()?;
        let mut events = EventStream::new();

        info!(target: "narwhal::app", "starting event loop");
        self.draw(&mut guard)?;

        while !self.should_quit {
            let Some(ev) = events.next().await else { break };
            let ev = ev?;
            self.handle_event(ev);
            self.draw(&mut guard)?;
        }

        info!(target: "narwhal::app", "event loop exited");
        Ok(())
    }

    fn draw(&self, guard: &mut TerminalGuard) -> Result<()> {
        guard.terminal.draw(|frame| {
            let view = RootLayout {
                mode: self.vim.mode(),
                connection_label: "(no connection)",
                status_message: &self.status_message,
                theme: &self.theme,
            };
            render_root(frame, frame.area(), &view);
        })?;
        Ok(())
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let Some(logical) = translate_key_event(key) else {
                    return;
                };
                let action = self.vim.handle(logical);
                self.apply_action(action);
            }
            Event::Resize(_, _) => {
                debug!(target: "narwhal::app", "terminal resized");
            }
            _ => {}
        }
    }

    fn apply_action(&mut self, action: Action) {
        match action {
            Action::SubmitCommand(cmd) => self.execute_command(&cmd),
            Action::EnterMode(Mode::Insert) => {
                self.status_message = "-- INSERT --".into();
            }
            Action::EnterMode(Mode::Normal) => {
                self.status_message = "ready".into();
            }
            Action::EnterMode(Mode::Command) => {
                self.status_message = ":".into();
            }
            Action::Pending => {
                if self.vim.mode() == Mode::Command {
                    self.status_message = format!(":{}", self.vim.command_buffer());
                }
            }
            other => {
                debug!(target: "narwhal::app", ?other, "action (not yet applied)");
            }
        }
    }

    fn execute_command(&mut self, cmd: &str) {
        match cmd.trim() {
            "q" | "quit" | "exit" => {
                self.should_quit = true;
            }
            "" => {}
            other => {
                self.status_message = format!("unknown command: {}", other);
            }
        }
    }
}
