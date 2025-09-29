use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{
    Event, EventStream, KeyCode as CtKey, KeyEvent, KeyEventKind, KeyModifiers,
};
use futures::StreamExt;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ColumnHeader, ConnectionConfig, Row};
use narwhal_history::Journal;
use narwhal_tui::{
    render_root, translate_key_event, EditorBuffer, Pane, ResultDisplay, ResultView, RootLayout,
    SidebarView, Theme,
};
use narwhal_vim::{Action, Mode, Vim};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info};

use crate::commands::{parse, Command};
use crate::export::{export_rows, ExportFormat};
use crate::registry::DriverRegistry;
use crate::run::{spawn_run, ActiveCancel, RunContext, RunMode, RunRequest, RunUpdate};
use crate::session::Session;
use crate::terminal::TerminalGuard;

const RUN_CHANNEL_CAPACITY: usize = 128;

/// What the result pane is currently showing.
#[derive(Debug, Default)]
pub enum ResultState {
    #[default]
    Empty,
    Running {
        sql: String,
        index: usize,
        total: usize,
        columns: Vec<ColumnHeader>,
        rows: Vec<Row>,
        streaming: bool,
    },
    Affected {
        rows: u64,
        elapsed_ms: u64,
        index: usize,
        total: usize,
    },
    Rows {
        columns: Vec<ColumnHeader>,
        rows: Vec<Row>,
        elapsed_ms: u64,
        streamed: bool,
        index: usize,
        total: usize,
    },
    Error {
        message: String,
        elapsed_ms: u64,
    },
}

pub struct App {
    registry: DriverRegistry,
    connections: ConnectionsFile,
    history: Option<Arc<Journal>>,
    session: Option<Session>,
    editor: EditorBuffer,
    result: ResultState,
    result_view: ResultView,
    vim: Vim,
    theme: Theme,
    focus: Pane,
    sidebar_index: usize,
    status_message: String,
    running: bool,
    cancel_slot: ActiveCancel,
    should_quit: bool,
    run_tx: mpsc::Sender<RunUpdate>,
    run_rx: mpsc::Receiver<RunUpdate>,
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
            result: ResultState::Empty,
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
                Some(update) = self.run_rx.recv() => {
                    self.handle_run_update(update);
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
        let result_display = display_from_state(&self.result);

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
                result: result_display,
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
                    self.dispatch_current_statement(RunMode::Execute);
                    return true;
                }
                CtKey::Char('s') => {
                    self.dispatch_current_statement(RunMode::Stream);
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
        let row_count = match &self.result {
            ResultState::Rows { rows, .. } | ResultState::Running { rows, .. } => rows.len(),
            _ => 0,
        };
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
            Action::Operate { .. } => {}
        }
    }

    fn execute_command(&mut self, raw: &str) {
        match parse(raw) {
            Command::Quit => self.should_quit = true,
            Command::Open(name) => self.open_named(&name),
            Command::Close => self.close_session(),
            Command::Refresh => self.refresh_schema(),
            Command::Run => self.dispatch_current_statement(RunMode::Execute),
            Command::RunAll => self.dispatch_all_statements(RunMode::Execute),
            Command::Stream => self.dispatch_current_statement(RunMode::Stream),
            Command::StreamAll => self.dispatch_all_statements(RunMode::Stream),
            Command::Cancel => self.spawn_cancel(),
            Command::Clear => {
                self.editor.clear();
                self.result = ResultState::Empty;
                self.result_view.reset();
                self.status_message = "buffer cleared".into();
            }
            Command::Export { format, path } => self.export_results(&format, &path),
            Command::Help => {
                self.status_message =
                    "open <name> · close · refresh · run · run-all · stream · stream-all · export <csv|json> <path> · cancel · quit"
                        .into();
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
        let label = config.name.clone();
        self.status_message = format!("connecting to {label}…");

        let driver = driver.clone();
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

    fn dispatch_current_statement(&mut self, mode: RunMode) {
        let Some(session) = self.session.as_ref() else {
            self.status_message = "no active connection".into();
            return;
        };
        let Some(sql) = self.editor.statement_at_cursor(session.dialect()) else {
            self.status_message = "no statement under cursor".into();
            return;
        };
        let trimmed = sql.trim().trim_end_matches(';').trim().to_owned();
        if trimmed.is_empty() {
            self.status_message = "no statement under cursor".into();
            return;
        }
        self.dispatch_batch(vec![trimmed], mode);
    }

    fn dispatch_all_statements(&mut self, mode: RunMode) {
        let Some(session) = self.session.as_ref() else {
            self.status_message = "no active connection".into();
            return;
        };
        let statements = self.editor.all_statements(session.dialect());
        if statements.is_empty() {
            self.status_message = "buffer contains no statements".into();
            return;
        }
        self.dispatch_batch(statements, mode);
    }

    fn dispatch_batch(&mut self, statements: Vec<String>, mode: RunMode) {
        if self.running {
            self.status_message = "a query is already running".into();
            return;
        }
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let ctx = RunContext {
            pool: session.pool.clone(),
            history: self.history.clone(),
            connection_id: session.config.id,
            connection_name: session.config.name.clone(),
            driver: session.driver.name().to_owned(),
        };
        let request = RunRequest { statements, mode };
        self.running = true;
        self.result_view.reset();
        self.result = ResultState::Running {
            sql: String::new(),
            index: 0,
            total: request.statements.len(),
            columns: Vec::new(),
            rows: Vec::new(),
            streaming: matches!(mode, RunMode::Stream),
        };
        self.status_message = match mode {
            RunMode::Execute => "executing…".into(),
            RunMode::Stream => "streaming…".into(),
        };
        spawn_run(ctx, request, self.cancel_slot.clone(), self.run_tx.clone());
    }

    fn export_results(&mut self, format: &str, path: &str) {
        let Some(format) = ExportFormat::from_token(format) else {
            self.status_message = format!("unknown export format: {format} (csv|json)");
            return;
        };
        let (columns, rows) = match &self.result {
            ResultState::Rows { columns, rows, .. } => (columns.as_slice(), rows.as_slice()),
            ResultState::Running { columns, rows, .. } if !columns.is_empty() => {
                (columns.as_slice(), rows.as_slice())
            }
            _ => {
                self.status_message = "no tabular result to export".into();
                return;
            }
        };
        let path = std::path::PathBuf::from(path);
        match export_rows(columns, rows, format, &path) {
            Ok(()) => {
                self.status_message = format!(
                    "exported {} rows to {} ({})",
                    rows.len(),
                    path.display(),
                    format.default_extension()
                );
            }
            Err(error) => {
                self.status_message = format!("export failed: {error}");
            }
        }
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

    fn handle_run_update(&mut self, update: RunUpdate) {
        match update {
            RunUpdate::StatementStarted { index, total, sql } => {
                let streaming = matches!(
                    &self.result,
                    ResultState::Running {
                        streaming: true,
                        ..
                    }
                );
                self.result = ResultState::Running {
                    sql: sql.clone(),
                    index,
                    total,
                    columns: Vec::new(),
                    rows: Vec::new(),
                    streaming,
                };
                self.result_view.reset();
                self.status_message = format!("running {index}/{total}: {}", truncate(&sql, 60));
            }
            RunUpdate::HeaderReady { columns: cols } => {
                if let ResultState::Running { columns, .. } = &mut self.result {
                    *columns = cols;
                }
            }
            RunUpdate::RowsAppended { rows: new_rows } => {
                if let ResultState::Running { rows, .. } = &mut self.result {
                    rows.extend(new_rows);
                }
            }
            RunUpdate::StatementFinished {
                elapsed_ms,
                rows_returned,
                rows_affected,
                streamed,
            } => {
                self.finalize_statement(elapsed_ms, rows_returned, rows_affected, streamed);
            }
            RunUpdate::Failed { error, elapsed_ms } => {
                self.result = ResultState::Error {
                    message: error,
                    elapsed_ms,
                };
                self.result_view.reset();
            }
            RunUpdate::AllDone {
                successes,
                failures,
            } => {
                self.running = false;
                self.status_message = if failures == 0 {
                    format!("done · {successes} statement(s)")
                } else {
                    format!("done · {successes} ok · {failures} failed")
                };
            }
        }
    }

    fn finalize_statement(
        &mut self,
        elapsed_ms: u64,
        rows_returned: usize,
        rows_affected: Option<u64>,
        streamed: bool,
    ) {
        let (columns, rows, index, total) = match std::mem::take(&mut self.result) {
            ResultState::Running {
                columns,
                rows,
                index,
                total,
                ..
            } => (columns, rows, index, total),
            other => {
                self.result = other;
                return;
            }
        };
        if columns.is_empty() {
            self.result = ResultState::Affected {
                rows: rows_affected.unwrap_or(0),
                elapsed_ms,
                index,
                total,
            };
        } else {
            self.result = ResultState::Rows {
                columns,
                rows,
                elapsed_ms,
                streamed,
                index,
                total,
            };
        }
        self.status_message = match rows_affected {
            Some(n) => format!("ok {index}/{total} · {n} affected · {elapsed_ms} ms"),
            None => format!("ok {index}/{total} · {rows_returned} rows · {elapsed_ms} ms"),
        };
    }
}

fn display_from_state(state: &ResultState) -> ResultDisplay<'_> {
    match state {
        ResultState::Empty => ResultDisplay::Empty,
        ResultState::Running {
            sql,
            index,
            total,
            columns,
            rows,
            ..
        } => ResultDisplay::Running {
            sql,
            index: *index,
            total: *total,
            columns,
            rows,
        },
        ResultState::Affected {
            rows,
            elapsed_ms,
            index,
            total,
        } => ResultDisplay::Affected {
            rows: *rows,
            elapsed_ms: *elapsed_ms,
            index: *index,
            total: *total,
        },
        ResultState::Rows {
            columns,
            rows,
            elapsed_ms,
            streamed,
            index,
            total,
        } => ResultDisplay::Rows {
            columns,
            rows,
            elapsed_ms: *elapsed_ms,
            streamed: *streamed,
            index: *index,
            total: *total,
        },
        ResultState::Error {
            message,
            elapsed_ms,
        } => ResultDisplay::Error {
            message,
            elapsed_ms: *elapsed_ms,
        },
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_owned()
    } else {
        let mut end = max.saturating_sub(1);
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}
