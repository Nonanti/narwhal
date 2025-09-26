use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{
    Event, EventStream, KeyCode as CtKey, KeyEvent, KeyEventKind, KeyModifiers,
};
use futures::StreamExt;
use narwhal_config::ConnectionsFile;
use narwhal_core::ConnectionConfig;
use narwhal_history::Journal;
use narwhal_sql::Dialect;
use narwhal_tui::{
    render_root, translate_key_event, EditorBuffer, Pane, ResultView, RootLayout, SidebarView,
    Theme,
};
use narwhal_vim::{Action, Mode, Motion, Vim};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info};

use crate::commands::{parse, Command};
use crate::registry::DriverRegistry;
use crate::run::{spawn_query, ActiveCancel, RunContext, RunResult};
use crate::session::Session;
use crate::terminal::TerminalGuard;

const RUN_CHANNEL_CAPACITY: usize = 16;

pub struct App {
    registry: DriverRegistry,
    connections: ConnectionsFile,
    history: Option<Arc<Journal>>,
    session: Option<Session>,
    editor: EditorBuffer,
    result: Option<narwhal_core::QueryResult>,
    result_error: Option<String>,
    result_view: ResultView,
    vim: Vim,
    theme: Theme,
    focus: Pane,
    sidebar_index: usize,
    status_message: String,
    running: bool,
    cancel_slot: ActiveCancel,
    should_quit: bool,
    run_tx: mpsc::Sender<RunResult>,
    run_rx: mpsc::Receiver<RunResult>,
}

impl App {
    pub fn new(
        registry: DriverRegistry,
        connections: ConnectionsFile,
        history: Option<Arc<Journal>>,
    ) -> Self {
        let (run_tx, run_rx) = mpsc::channel(RUN_CHANNEL_CAPACITY);
        Self {
            registry,
            connections,
            history,
            session: None,
            editor: EditorBuffer::new(),
            result: None,
            result_error: None,
            result_view: ResultView::new(),
            vim: Vim::new(),
            theme: Theme::default(),
            focus: Pane::Editor,
            sidebar_index: 0,
            status_message: "ready".into(),
            running: false,
            cancel_slot: Arc::new(Mutex::new(None)),
            should_quit: false,
            run_tx,
            run_rx,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        let mut guard = TerminalGuard::enter()?;
        let mut events = EventStream::new();

        info!(target: "narwhal::app", "event loop started");
        self.draw(&mut guard)?;

        while !self.should_quit {
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
                Some(result) = self.run_rx.recv() => {
                    self.handle_run_result(result);
                }
            }
            self.draw(&mut guard)?;
        }

        info!(target: "narwhal::app", "event loop terminated");
        Ok(())
    }

    fn draw(&mut self, guard: &mut TerminalGuard) -> Result<()> {
        let sidebar_view = SidebarView {
            connections: &self.connections.connections,
            active_connection: self.session.as_ref().map(|s| s.config.id),
            schemas: self
                .session
                .as_ref()
                .map(|s| s.schemas.as_slice())
                .unwrap_or(&[]),
            selected_index: self.sidebar_index,
            focused: self.focus == Pane::Sidebar,
        };
        let connection_label = self
            .session
            .as_ref()
            .map(|s| s.config.name.clone())
            .unwrap_or_else(|| "(no connection)".into());
        let editor_title = self
            .session
            .as_ref()
            .map(|s| format!("editor · {}", s.driver.display_name()))
            .unwrap_or_else(|| "editor".into());

        let mode = self.vim.mode();
        let focus = self.focus;
        let status = self.status_message.clone();
        let running = self.running;
        let theme = self.theme;
        let result_error = self.result_error.clone();

        guard.terminal.draw(|frame| {
            let mut layout = RootLayout {
                mode,
                focus,
                connection_label: &connection_label,
                status_message: &status,
                running,
                theme: &theme,
                sidebar: sidebar_view,
                editor: &mut self.editor,
                editor_title: &editor_title,
                result_view: &mut self.result_view,
                result: self.result.as_ref(),
                result_error: result_error.as_deref(),
            };
            render_root(frame, frame.area(), &mut layout);
        })?;
        Ok(())
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
            Event::Resize(_, _) => debug!(target: "narwhal::app", "terminal resized"),
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if self.handle_global_key(key) {
            return;
        }
        match self.focus {
            Pane::Editor => self.handle_editor_key(key),
            Pane::Sidebar => self.handle_sidebar_key(key),
            Pane::Results => self.handle_results_key(key),
        }
    }

    fn handle_global_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                CtKey::Char('w') => {
                    self.focus = self.focus.cycle();
                    self.status_message = format!("focus → {}", self.focus.label());
                    return true;
                }
                CtKey::Char('c') if self.running => {
                    self.spawn_cancel();
                    return true;
                }
                CtKey::Char(';') => {
                    self.run_current_statement();
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    fn handle_editor_key(&mut self, key: KeyEvent) {
        let Some(logical) = translate_key_event(key) else {
            return;
        };
        let action = self.vim.handle(logical);
        self.apply_action(action);
    }

    fn handle_sidebar_key(&mut self, key: KeyEvent) {
        match key.code {
            CtKey::Char('j') | CtKey::Down if !self.connections.connections.is_empty() => {
                self.sidebar_index = (self.sidebar_index + 1) % self.connections.connections.len();
            }
            CtKey::Char('k') | CtKey::Up if !self.connections.connections.is_empty() => {
                let len = self.connections.connections.len();
                self.sidebar_index = (self.sidebar_index + len - 1) % len;
            }
            CtKey::Enter => {
                if let Some(conn) = self
                    .connections
                    .connections
                    .get(self.sidebar_index)
                    .cloned()
                {
                    self.open_connection(conn);
                }
            }
            _ => {}
        }
    }

    fn handle_results_key(&mut self, key: KeyEvent) {
        let row_count = self.result.as_ref().map(|r| r.rows.len()).unwrap_or(0);
        match key.code {
            CtKey::Char('j') | CtKey::Down => self.result_view.move_down(row_count),
            CtKey::Char('k') | CtKey::Up => self.result_view.move_up(),
            CtKey::Char('g') => self.result_view.state.select(Some(0)),
            CtKey::Char('G') if row_count > 0 => {
                self.result_view.state.select(Some(row_count - 1));
            }
            _ => {}
        }
    }

    fn apply_action(&mut self, action: Action) {
        match action {
            Action::Move { motion, count } => {
                self.editor.apply_motion(motion, count);
            }
            Action::InsertText(text) => {
                self.editor.insert_str(&text);
            }
            Action::DeleteChar => {
                self.editor.delete_char();
            }
            Action::EnterMode(mode) => {
                self.status_message = match mode {
                    Mode::Insert => "-- INSERT --".into(),
                    Mode::Normal => "ready".into(),
                    Mode::Command => ":".into(),
                    Mode::Visual => "-- VISUAL --".into(),
                    Mode::VisualLine => "-- V-LINE --".into(),
                };
            }
            Action::SubmitCommand(cmd) => self.execute_command(&cmd),
            Action::Pending => {
                if self.vim.mode() == Mode::Command {
                    self.status_message = format!(":{}", self.vim.command_buffer());
                }
            }
            Action::Operate { .. } => {
                // Operator+motion combinations are not yet wired to the buffer.
            }
        }
    }

    fn execute_command(&mut self, raw: &str) {
        match parse(raw) {
            Command::Quit => self.should_quit = true,
            Command::Open(name) => self.open_named(&name),
            Command::Close => self.close_session(),
            Command::Refresh => self.refresh_schema(),
            Command::Run => self.run_current_statement(),
            Command::Cancel => self.spawn_cancel(),
            Command::Help => {
                self.status_message =
                    "commands: open <name>, close, refresh, run, cancel, quit".into();
            }
            Command::Empty => {}
            Command::Unknown(text) => {
                self.status_message = format!("unknown command: {text}");
            }
        }
    }

    fn open_named(&mut self, name: &str) {
        let Some(config) = self
            .connections
            .connections
            .iter()
            .find(|c| c.name == name)
            .cloned()
        else {
            self.status_message = format!("connection not found: {name}");
            return;
        };
        self.open_connection(config);
    }

    fn open_connection(&mut self, config: ConnectionConfig) {
        let Ok(driver) = self.registry.get(&config.driver) else {
            self.status_message = format!("driver not registered: {}", config.driver);
            return;
        };
        let registry = self.registry.clone();
        let _ = registry;
        let label = config.name.clone();
        self.status_message = format!("connecting to {label}…");

        let driver = driver.clone();
        // Connection establishment is run on the runtime synchronously here,
        // because the UI does not yet expose an "opening" state distinct from
        // "ready". For sub-second connections this is unnoticeable; longer
        // handshakes will be moved off the UI thread when the dial-up UI
        // gains its own state machine.
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { Session::open(driver, config, None).await })
        });
        match result {
            Ok(mut session) => {
                if let Err(error) = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(session.refresh_schemas())
                }) {
                    debug!(target: "narwhal::app", error = %error, "schema refresh on connect failed");
                }
                self.status_message = format!(
                    "connected · {} · {}",
                    session.config.name,
                    session.driver.name()
                );
                self.session = Some(session);
                self.focus = Pane::Editor;
            }
            Err(error) => {
                self.status_message = format!("connect failed: {error}");
            }
        }
    }

    fn close_session(&mut self) {
        if self.session.take().is_some() {
            self.status_message = "connection closed".into();
        }
    }

    fn refresh_schema(&mut self) {
        let Some(session) = self.session.as_mut() else {
            self.status_message = "no active connection".into();
            return;
        };
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(session.refresh_schemas())
        });
        match result {
            Ok(()) => self.status_message = "schema refreshed".into(),
            Err(error) => self.status_message = format!("refresh failed: {error}"),
        }
    }

    fn run_current_statement(&mut self) {
        let Some(session) = self.session.as_ref() else {
            self.status_message = "no active connection".into();
            return;
        };
        if self.running {
            self.status_message = "a query is already running".into();
            return;
        }
        let Some(sql) = self.editor.statement_at_cursor(session.dialect()) else {
            self.status_message = "no statement under cursor".into();
            return;
        };
        let trimmed = sql.trim_end_matches(';').trim().to_owned();
        if trimmed.is_empty() {
            self.status_message = "no statement under cursor".into();
            return;
        }

        let ctx = RunContext {
            pool: session.pool.clone(),
            history: self.history.clone(),
            connection_id: session.config.id,
            connection_name: session.config.name.clone(),
            driver: session.driver.name().to_owned(),
        };
        self.running = true;
        self.result_error = None;
        self.status_message = "running…".into();
        spawn_query(ctx, trimmed, self.cancel_slot.clone(), self.run_tx.clone());
    }

    fn spawn_cancel(&mut self) {
        let slot = self.cancel_slot.clone();
        tokio::spawn(async move {
            let guard = slot.lock().await;
            if let Some(handle) = guard.as_ref() {
                if let Err(error) = handle.cancel().await {
                    tracing::warn!(target: "narwhal::app", error = %error, "cancel failed");
                }
            }
        });
        self.status_message = "cancellation requested".into();
    }

    fn handle_run_result(&mut self, result: RunResult) {
        self.running = false;
        match result.outcome {
            Ok(qr) => {
                let row_count = qr.rows.len();
                let affected = qr.rows_affected;
                self.result_view.reset();
                self.result_error = None;
                self.result = Some(qr);
                self.status_message = match affected {
                    Some(n) => format!("ok · {n} affected"),
                    None => format!("ok · {row_count} rows"),
                };
            }
            Err(narwhal_core::Error::Cancelled) => {
                self.result_error = Some("cancelled".into());
                self.status_message = "cancelled".into();
            }
            Err(error) => {
                self.result_error = Some(error.to_string());
                self.status_message = format!("error: {error}");
            }
        }
    }
}

// Keep these in scope so the surrounding code can rely on intra-doc links.
#[allow(dead_code)]
fn _dialect_hint() -> Dialect {
    Dialect::default()
}

#[allow(dead_code)]
fn _motion_hint() -> Motion {
    Motion::Left
}
