use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use narwhal_tui::{render_root, translate_key_event, RootLayout, Theme};
use narwhal_vim::{Action, Mode, Vim};
use tracing::{debug, info};

use crate::registry::DriverRegistry;
use crate::terminal::TerminalGuard;

/// Top-level application state.
pub struct App {
    registry: DriverRegistry,
    vim: Vim,
    theme: Theme,
    status_message: String,
    should_quit: bool,
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

    pub fn registry(&self) -> &DriverRegistry {
        &self.registry
    }

    /// Run the event loop until the user requests an exit.
    pub async fn run(mut self) -> Result<()> {
        let mut guard = TerminalGuard::enter()?;
        let mut events = EventStream::new();

        info!(target: "narwhal::app", "event loop started");
        self.draw(&mut guard)?;

        while !self.should_quit {
            let Some(event) = events.next().await else {
                break;
            };
            self.handle_event(event?);
            self.draw(&mut guard)?;
        }

        info!(target: "narwhal::app", "event loop terminated");
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
                if let Some(logical) = translate_key_event(key) {
                    let action = self.vim.handle(logical);
                    self.apply_action(action);
                }
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
            Action::Pending if self.vim.mode() == Mode::Command => {
                self.status_message = format!(":{}", self.vim.command_buffer());
            }
            action => {
                debug!(target: "narwhal::app", ?action, "action received");
            }
        }
    }

    fn execute_command(&mut self, command: &str) {
        match command.trim() {
            "" => {}
            "q" | "quit" | "exit" => self.should_quit = true,
            other => self.status_message = format!("unknown command: {other}"),
        }
    }
}
