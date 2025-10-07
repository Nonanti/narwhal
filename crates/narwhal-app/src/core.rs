//! Headless application state.
//!
//! [`AppCore`] owns every piece of state the UI depends on but contains no
//! terminal-IO logic. The render path takes a [`ratatui::Frame`] from the
//! caller, and key events come in as parsed crossterm [`KeyEvent`]s, so the
//! core is fully usable with `ratatui::backend::TestBackend` in tests.

use std::sync::Arc;

use crossterm::event::{KeyCode as CtKey, KeyEvent, KeyModifiers};
use narwhal_config::ConnectionsFile;
use narwhal_core::{ColumnHeader, ConnectionConfig, Row, TableKind, TableSchema};
use narwhal_history::Journal;
use narwhal_tui::{
    render_root, translate_key_event, CellPopup, EditorBuffer, ExplainPlanLine, Pane,
    ResultDisplay, ResultView, RootLayout, SidebarRow, SidebarRowKind, SidebarView, Theme,
};
use narwhal_vim::{Action, Mode, Vim};
use ratatui::layout::Rect;
use ratatui::Frame;
use tokio::sync::{mpsc, Mutex};
use tracing::debug;
use uuid::Uuid;

use crate::commands::{parse, Command, DumpTarget};
use crate::ddl::{build_dump, build_table_ddl};
use crate::explain::{parse as parse_plan, wrap_explain};
use crate::export::{export_rows, ExportFormat};
use crate::registry::DriverRegistry;
use crate::run::{spawn_run, ActiveCancel, RunContext, RunMode, RunRequest, RunUpdate};
use crate::session::Session;

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
    Explain {
        lines: Vec<ExplainPlanLine>,
        planning_time_ms: Option<f64>,
        execution_time_ms: Option<f64>,
    },
    TableDetail {
        schema: TableSchema,
    },
    Error {
        message: String,
        elapsed_ms: u64,
    },
}

/// One editor tab: a buffer + the most recent result it produced.
pub struct Tab {
    pub name: String,
    pub editor: EditorBuffer,
    pub result: ResultState,
    pub result_view: ResultView,
}

impl Tab {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            editor: EditorBuffer::new(),
            result: ResultState::Empty,
            result_view: ResultView::new(),
        }
    }
}

/// Internal entry in the rendered sidebar list.
#[derive(Debug, Clone)]
enum SidebarItem {
    Connection {
        #[allow(dead_code)]
        id: Uuid,
        name: String,
        driver: String,
        active: bool,
    },
    Schema {
        name: String,
    },
    Table {
        schema: String,
        name: String,
        kind: TableKind,
    },
}

/// Pure, IO-free application state and behaviour.
pub struct AppCore {
    registry: DriverRegistry,
    connections: ConnectionsFile,
    history: Option<Arc<Journal>>,
    session: Option<Session>,
    tabs: Vec<Tab>,
    active_tab: usize,
    next_tab_id: usize,
    vim: Vim,
    theme: Theme,
    focus: Pane,
    sidebar_items: Vec<SidebarItem>,
    sidebar_index: usize,
    status_message: String,
    running: bool,
    cancel_slot: ActiveCancel,
    should_quit: bool,
    run_tx: mpsc::Sender<RunUpdate>,
    pub(crate) run_rx: mpsc::Receiver<RunUpdate>,
}

impl AppCore {
    pub fn new(
        registry: DriverRegistry,
        connections: ConnectionsFile,
        history: Option<Arc<Journal>>,
    ) -> Self {
        let (run_tx, run_rx) = mpsc::channel(RUN_CHANNEL_CAPACITY);
        let mut this = Self::new_inner(registry, connections, history, run_tx, run_rx);
        this.rebuild_sidebar();
        this
    }

    fn new_inner(
        registry: DriverRegistry,
        connections: ConnectionsFile,
        history: Option<Arc<Journal>>,
        run_tx: mpsc::Sender<RunUpdate>,
        run_rx: mpsc::Receiver<RunUpdate>,
    ) -> Self {
        Self {
            registry,
            connections,
            history,
            session: None,
            tabs: vec![Tab::new("untitled")],
            active_tab: 0,
            next_tab_id: 2,
            vim: Vim::new(),
            theme: Theme::default(),
            focus: Pane::Editor,
            sidebar_items: Vec::new(),
            sidebar_index: 0,
            status_message: "ready".into(),
            running: false,
            cancel_slot: Arc::new(Mutex::new(None)),
            should_quit: false,
            run_tx,
            run_rx,
        }
    }

    fn rebuild_sidebar(&mut self) {
        let mut items = Vec::new();
        let active_id = self.session.as_ref().map(|s| s.config.id);
        for conn in &self.connections.connections {
            let active = Some(conn.id) == active_id;
            items.push(SidebarItem::Connection {
                id: conn.id,
                name: conn.name.clone(),
                driver: conn.driver.clone(),
                active,
            });
            if active {
                if let Some(session) = self.session.as_ref() {
                    for (schema, tables) in &session.schemas {
                        if !schema.name.is_empty() {
                            items.push(SidebarItem::Schema {
                                name: schema.name.clone(),
                            });
                        }
                        for table in tables {
                            items.push(SidebarItem::Table {
                                schema: table.schema.clone(),
                                name: table.name.clone(),
                                kind: table.kind,
                            });
                        }
                    }
                }
            }
        }
        self.sidebar_items = items;
        if self.sidebar_index >= self.sidebar_items.len() {
            self.sidebar_index = self.sidebar_items.len().saturating_sub(1);
        }
    }

    // ----- accessors -----

    pub fn status_message(&self) -> &str {
        &self.status_message
    }

    pub fn result(&self) -> &ResultState {
        &self.tab().result
    }

    pub fn editor(&self) -> &EditorBuffer {
        &self.tab().editor
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn active_tab(&self) -> usize {
        self.active_tab
    }

    fn tab(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    #[allow(dead_code)]
    fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }

    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    pub fn focus(&self) -> Pane {
        self.focus
    }

    pub fn mode(&self) -> Mode {
        self.vim.mode()
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    // ----- render -----

    fn editor_title_with_tabs(&self) -> String {
        let driver = self.session.as_ref().map(|s| s.driver.display_name());
        let base = match driver {
            Some(d) => format!("editor · {d}"),
            None => "editor".to_owned(),
        };
        if self.tabs.len() == 1 {
            return base;
        }
        let labels: Vec<String> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i == self.active_tab {
                    format!("[{}*] {}", i + 1, t.name)
                } else {
                    format!("[{}] {}", i + 1, t.name)
                }
            })
            .collect();
        format!("{base} · {}", labels.join("  "))
    }

    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let labels: Vec<String> = self.sidebar_items.iter().map(sidebar_label).collect();
        let rows: Vec<SidebarRow<'_>> = self
            .sidebar_items
            .iter()
            .zip(labels.iter())
            .map(|(item, label)| SidebarRow {
                depth: sidebar_depth(item),
                kind: sidebar_kind(item),
                label: label.as_str(),
            })
            .collect();
        let sidebar_view = SidebarView {
            items: &rows,
            selected_index: self.sidebar_index,
            focused: self.focus == Pane::Sidebar,
        };
        let connection_label = self
            .session
            .as_ref()
            .map(|s| s.config.name.clone())
            .unwrap_or_else(|| "(no connection)".into());
        let editor_title = self.editor_title_with_tabs();

        let tab = &mut self.tabs[self.active_tab];
        let result_display = display_from_state(&tab.result);
        let mut layout = RootLayout {
            mode: self.vim.mode(),
            focus: self.focus,
            connection_label: &connection_label,
            status_message: &self.status_message,
            running: self.running,
            theme: &self.theme,
            sidebar: sidebar_view,
            editor: &mut tab.editor,
            editor_title: &editor_title,
            result_view: &mut tab.result_view,
            result: result_display,
        };
        render_root(frame, area, &mut layout);
    }

    // ----- input -----

    pub fn handle_key(&mut self, key: KeyEvent) {
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
                CtKey::Tab => {
                    self.cycle_tab(1);
                    return true;
                }
                CtKey::BackTab => {
                    self.cycle_tab(-1);
                    return true;
                }
                CtKey::Char('t') => {
                    self.new_tab();
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
            CtKey::Char('j') | CtKey::Down if !self.sidebar_items.is_empty() => {
                self.sidebar_index = (self.sidebar_index + 1) % self.sidebar_items.len();
            }
            CtKey::Char('k') | CtKey::Up if !self.sidebar_items.is_empty() => {
                let len = self.sidebar_items.len();
                self.sidebar_index = (self.sidebar_index + len - 1) % len;
            }
            CtKey::Enter => self.activate_sidebar_selection(),
            CtKey::Char('o') => self.preview_sidebar_selection(),
            _ => {}
        }
    }

    fn preview_sidebar_selection(&mut self) {
        let Some(item) = self.sidebar_items.get(self.sidebar_index).cloned() else {
            return;
        };
        let SidebarItem::Table { schema, name, .. } = item else {
            self.status_message = "select a table to preview".into();
            return;
        };
        let Some(session) = self.session.as_ref() else {
            self.status_message = "no active connection".into();
            return;
        };
        let dialect = session.dialect();
        let sql = crate::ddl::preview_query(&schema, &name, 100, dialect);
        self.dispatch_batch(vec![sql], RunMode::Execute);
        self.focus = Pane::Results;
    }

    fn activate_sidebar_selection(&mut self) {
        let Some(item) = self.sidebar_items.get(self.sidebar_index).cloned() else {
            return;
        };
        match item {
            SidebarItem::Connection { name, .. } => self.open_named(&name),
            SidebarItem::Schema { .. } => {}
            SidebarItem::Table { schema, name, .. } => {
                self.describe_table_into_result(&schema, &name);
            }
        }
    }

    fn describe_table_into_result(&mut self, schema: &str, name: &str) {
        let Some(session) = self.session.as_ref() else {
            self.status_message = "no active connection".into();
            return;
        };
        let pool = session.pool.clone();
        let schema_owned = schema.to_owned();
        let name_owned = name.to_owned();
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let mut conn = pool
                    .acquire()
                    .await
                    .map_err(|e| narwhal_core::Error::Connection(e.to_string()))?;
                conn.describe_table(&schema_owned, &name_owned).await
            })
        });
        match result {
            Ok(ts) => {
                self.tabs[self.active_tab].result_view.reset();
                self.status_message = format!(
                    "{}.{}: {} cols·{} idx·{} fk",
                    ts.table.schema,
                    ts.table.name,
                    ts.columns.len(),
                    ts.indexes.len(),
                    ts.foreign_keys.len()
                );
                self.tabs[self.active_tab].result = ResultState::TableDetail { schema: ts };
            }
            Err(error) => {
                self.status_message = format!("describe failed: {error}");
            }
        }
    }

    fn handle_results_key(&mut self, key: KeyEvent) {
        if self.tabs[self.active_tab].result_view.popup.is_some() {
            if matches!(key.code, CtKey::Esc | CtKey::Char('q') | CtKey::Enter) {
                self.tabs[self.active_tab].result_view.popup = None;
            }
            return;
        }
        let (row_count, col_count) = match &self.tabs[self.active_tab].result {
            ResultState::Rows { rows, columns, .. }
            | ResultState::Running { rows, columns, .. } => (rows.len(), columns.len()),
            _ => (0, 0),
        };
        match key.code {
            CtKey::Char('j') | CtKey::Down => {
                self.tabs[self.active_tab].result_view.move_down(row_count)
            }
            CtKey::Char('k') | CtKey::Up => self.tabs[self.active_tab].result_view.move_up(),
            CtKey::Char('h') | CtKey::Left => self.tabs[self.active_tab].result_view.move_left(),
            CtKey::Char('l') | CtKey::Right => {
                self.tabs[self.active_tab].result_view.move_right(col_count)
            }
            CtKey::Char('g') => self.tabs[self.active_tab].result_view.state.select(Some(0)),
            CtKey::Char('G') if row_count > 0 => {
                self.tabs[self.active_tab]
                    .result_view
                    .state
                    .select(Some(row_count - 1));
            }
            CtKey::Enter => self.open_cell_popup(),
            _ => {}
        }
    }

    fn open_cell_popup(&mut self) {
        let Some(row_index) = self.tabs[self.active_tab].result_view.state.selected() else {
            self.status_message = "select a row first (j/k)".into();
            return;
        };
        let col_index = self.tabs[self.active_tab].result_view.column_index;
        let (columns, rows) = match &self.tabs[self.active_tab].result {
            ResultState::Rows { rows, columns, .. } => (columns, rows),
            ResultState::Running { rows, columns, .. } => (columns, rows),
            _ => return,
        };
        let Some(column) = columns.get(col_index) else {
            return;
        };
        let Some(row) = rows.get(row_index) else {
            return;
        };
        let Some(value) = row.0.get(col_index) else {
            return;
        };
        self.tabs[self.active_tab].result_view.popup = Some(CellPopup {
            column_name: column.name.clone(),
            column_type: column.data_type.clone(),
            value_text: value.render(),
            row_index,
        });
    }

    fn apply_action(&mut self, action: Action) {
        match action {
            Action::Move { motion, count } => {
                self.tabs[self.active_tab]
                    .editor
                    .apply_motion(motion, count);
            }
            Action::InsertText(text) => {
                self.tabs[self.active_tab].editor.insert_str(&text);
            }
            Action::DeleteChar => {
                self.tabs[self.active_tab].editor.delete_char();
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

    /// Execute a command exactly as if the user submitted it from command-line
    /// mode. Useful from tests.
    pub fn execute_command(&mut self, raw: &str) {
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
                self.tabs[self.active_tab].editor.clear();
                self.tabs[self.active_tab].result = ResultState::Empty;
                self.tabs[self.active_tab].result_view.reset();
                self.status_message = "buffer cleared".into();
            }
            Command::Explain => self.dispatch_explain(),
            Command::Export { format, path } => self.export_results(&format, &path),
            Command::DumpSchema { target } => self.dump_schema(target),
            Command::NewTab => self.new_tab(),
            Command::CloseTab => self.close_tab(),
            Command::NextTab => self.cycle_tab(1),
            Command::PrevTab => self.cycle_tab(-1),
            Command::Help => {
                self.status_message =
                    "open <name> · close · refresh · run · run-all · stream · stream-all · explain · export <csv|json> <path> · cancel · quit"
                        .into();
            }
            Command::Empty => {}
            Command::Unknown(text) => {
                self.status_message = format!("unknown command: {text}");
            }
        }
    }

    /// Insert raw text into the editor buffer. Used by tests to seed
    /// statements without simulating individual key presses.
    pub fn insert_into_editor(&mut self, text: &str) {
        self.tabs[self.active_tab].editor.insert_str(text);
    }

    // ----- session management -----

    fn open_named(&mut self, target: &str) {
        if target.contains("://") || target.starts_with("sqlite:") {
            match narwhal_config::parse_url(target) {
                Ok(parsed) => {
                    self.open_connection_with_password(parsed.config, parsed.password);
                }
                Err(error) => {
                    self.status_message = format!("invalid url: {error}");
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
            self.status_message = format!("connection not found: {target}");
            return;
        };
        self.open_connection(config);
    }

    fn open_connection(&mut self, config: ConnectionConfig) {
        self.open_connection_with_password(config, None);
    }

    fn open_connection_with_password(
        &mut self,
        config: ConnectionConfig,
        password: Option<String>,
    ) {
        let Ok(driver) = self.registry.get(&config.driver) else {
            self.status_message = format!("driver not registered: {}", config.driver);
            return;
        };
        let label = config.name.clone();
        self.status_message = format!("connecting to {label}…");

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
                self.status_message = format!(
                    "connected · {} · {}",
                    session.config.name,
                    session.driver.name()
                );
                self.session = Some(session);
                self.rebuild_sidebar();
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
            self.rebuild_sidebar();
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
            Ok(()) => {
                self.status_message = "schema refreshed".into();
                self.rebuild_sidebar();
            }
            Err(error) => self.status_message = format!("refresh failed: {error}"),
        }
    }

    // ----- dispatch -----

    fn dispatch_current_statement(&mut self, mode: RunMode) {
        let Some(session) = self.session.as_ref() else {
            self.status_message = "no active connection".into();
            return;
        };
        let Some(sql) = self.tabs[self.active_tab]
            .editor
            .statement_at_cursor(session.dialect())
        else {
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
        let statements = self.tabs[self.active_tab]
            .editor
            .all_statements(session.dialect());
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
        self.tabs[self.active_tab].result_view.reset();
        self.tabs[self.active_tab].result = ResultState::Running {
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

    fn new_tab(&mut self) {
        let name = format!("untitled-{}", self.next_tab_id);
        self.next_tab_id += 1;
        self.tabs.push(Tab::new(name));
        self.active_tab = self.tabs.len() - 1;
        self.status_message = format!("tab {} opened", self.active_tab + 1);
        self.focus = Pane::Editor;
    }

    fn close_tab(&mut self) {
        if self.tabs.len() == 1 {
            self.status_message = "last tab; use :q to quit".into();
            return;
        }
        self.tabs.remove(self.active_tab);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        self.status_message = format!("tab closed; now on {}", self.active_tab + 1);
    }

    fn cycle_tab(&mut self, delta: i32) {
        if self.tabs.len() <= 1 {
            return;
        }
        let len = self.tabs.len() as i32;
        let next = ((self.active_tab as i32) + delta).rem_euclid(len) as usize;
        self.active_tab = next;
        self.status_message = format!(
            "tab {} of {} · {}",
            self.active_tab + 1,
            self.tabs.len(),
            self.tabs[self.active_tab].name
        );
    }

    fn dump_schema(&mut self, target: DumpTarget) {
        let Some(session) = self.session.as_ref() else {
            self.status_message = "no active connection".into();
            return;
        };
        let dialect = session.dialect();
        let pool = session.pool.clone();
        let schemas: Vec<(String, String)> = session
            .schemas
            .iter()
            .flat_map(|(schema, tables)| {
                tables
                    .iter()
                    .map(move |t| (schema.name.clone(), t.name.clone()))
            })
            .collect();

        let names: Vec<(String, String)> = match target {
            DumpTarget::Current => {
                if let ResultState::TableDetail { schema } = &self.tabs[self.active_tab].result {
                    vec![(schema.table.schema.clone(), schema.table.name.clone())]
                } else {
                    self.status_message =
                        "dump-schema: select a table in the sidebar or pass a name".into();
                    return;
                }
            }
            DumpTarget::All => schemas,
            DumpTarget::Named(ref name) => {
                if let Some(pair) = schemas.iter().find(|(_, t)| t == name).cloned() {
                    vec![pair]
                } else {
                    self.status_message = format!("dump-schema: table not found: {name}");
                    return;
                }
            }
        };

        if names.is_empty() {
            self.status_message = "dump-schema: nothing to dump".into();
            return;
        }

        let collected: std::result::Result<Vec<_>, narwhal_core::Error> =
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async move {
                    let mut conn = pool
                        .acquire()
                        .await
                        .map_err(|e| narwhal_core::Error::Connection(e.to_string()))?;
                    let mut out = Vec::with_capacity(names.len());
                    for (schema, table) in names {
                        out.push(conn.describe_table(&schema, &table).await?);
                    }
                    Ok(out)
                })
            });
        match collected {
            Ok(tables) => {
                let ddl = if tables.len() == 1 {
                    build_table_ddl(&tables[0], dialect)
                } else {
                    build_dump(&tables, dialect)
                };
                self.tabs[self.active_tab].editor.clear();
                self.tabs[self.active_tab].editor.insert_str(&ddl);
                self.status_message = format!(
                    "dump-schema: wrote {} table(s) into the editor buffer",
                    tables.len()
                );
                self.focus = Pane::Editor;
            }
            Err(error) => {
                self.status_message = format!("dump-schema failed: {error}");
            }
        }
    }

    fn dispatch_explain(&mut self) {
        let Some(session) = self.session.as_ref() else {
            self.status_message = "no active connection".into();
            return;
        };
        if session.driver.name() != "postgres" {
            self.status_message = "explain is only supported on postgres for now".into();
            return;
        }
        let Some(sql) = self.tabs[self.active_tab]
            .editor
            .statement_at_cursor(session.dialect())
        else {
            self.status_message = "no statement under cursor".into();
            return;
        };
        let trimmed = sql.trim().trim_end_matches(';').trim().to_owned();
        if trimmed.is_empty() {
            self.status_message = "no statement under cursor".into();
            return;
        }
        self.dispatch_batch(vec![wrap_explain(&trimmed)], RunMode::Execute);
        self.status_message = "explaining…".into();
    }

    fn export_results(&mut self, format: &str, path: &str) {
        let Some(format) = ExportFormat::from_token(format) else {
            self.status_message = format!("unknown export format: {format} (csv|json)");
            return;
        };
        let (columns, rows) = match &self.tabs[self.active_tab].result {
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

    // ----- run-loop integration -----

    /// Receive the next [`RunUpdate`] from the worker channel.
    pub async fn recv_run_update(&mut self) -> Option<RunUpdate> {
        self.run_rx.recv().await
    }

    pub fn handle_run_update(&mut self, update: RunUpdate) {
        match update {
            RunUpdate::StatementStarted { index, total, sql } => {
                let streaming = matches!(
                    &self.tabs[self.active_tab].result,
                    ResultState::Running {
                        streaming: true,
                        ..
                    }
                );
                self.tabs[self.active_tab].result = ResultState::Running {
                    sql: sql.clone(),
                    index,
                    total,
                    columns: Vec::new(),
                    rows: Vec::new(),
                    streaming,
                };
                self.tabs[self.active_tab].result_view.reset();
                self.status_message = format!("running {index}/{total}: {}", truncate(&sql, 60));
            }
            RunUpdate::HeaderReady { columns: cols } => {
                if let ResultState::Running { columns, .. } = &mut self.tabs[self.active_tab].result
                {
                    *columns = cols;
                }
            }
            RunUpdate::RowsAppended { rows: new_rows } => {
                if let ResultState::Running { rows, .. } = &mut self.tabs[self.active_tab].result {
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
                self.tabs[self.active_tab].result = ResultState::Error {
                    message: error,
                    elapsed_ms,
                };
                self.tabs[self.active_tab].result_view.reset();
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

    /// Drive the worker channel to completion. Useful from tests after
    /// dispatching a batch: pumps every [`RunUpdate`] until `AllDone`.
    pub async fn drain_run_updates(&mut self) {
        while self.running {
            match self.recv_run_update().await {
                Some(update) => self.handle_run_update(update),
                None => break,
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
        let (columns, rows, index, total) =
            match std::mem::take(&mut self.tabs[self.active_tab].result) {
                ResultState::Running {
                    columns,
                    rows,
                    index,
                    total,
                    ..
                } => (columns, rows, index, total),
                other => {
                    self.tabs[self.active_tab].result = other;
                    return;
                }
            };
        if columns.is_empty() {
            self.tabs[self.active_tab].result = ResultState::Affected {
                rows: rows_affected.unwrap_or(0),
                elapsed_ms,
                index,
                total,
            };
        } else if is_explain_result(&columns) {
            match extract_explain_plan(&rows) {
                Ok(plan) => {
                    self.tabs[self.active_tab].result = ResultState::Explain {
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
                    self.status_message = format!("explain ok · {elapsed_ms} ms");
                    return;
                }
                Err(error) => {
                    self.tabs[self.active_tab].result = ResultState::Error {
                        message: format!("explain parse failed: {error}"),
                        elapsed_ms,
                    };
                    return;
                }
            }
        } else {
            self.tabs[self.active_tab].result = ResultState::Rows {
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

fn is_explain_result(columns: &[ColumnHeader]) -> bool {
    columns.len() == 1 && columns[0].name.eq_ignore_ascii_case("QUERY PLAN")
}

fn extract_explain_plan(rows: &[Row]) -> Result<crate::explain::ExplainPlan, String> {
    let row = rows
        .first()
        .ok_or_else(|| "empty explain result".to_owned())?;
    let value = row
        .0
        .first()
        .ok_or_else(|| "explain row missing column".to_owned())?;
    let json_text = match value {
        narwhal_core::Value::Json(v) => v.to_string(),
        narwhal_core::Value::String(s) | narwhal_core::Value::Unknown(s) => s.clone(),
        other => other.render(),
    };
    parse_plan(&json_text)
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
        ResultState::Explain {
            lines,
            planning_time_ms,
            execution_time_ms,
        } => ResultDisplay::Explain {
            lines,
            planning_time_ms: *planning_time_ms,
            execution_time_ms: *execution_time_ms,
        },
        ResultState::TableDetail { schema } => ResultDisplay::TableDetail { schema },
        ResultState::Error {
            message,
            elapsed_ms,
        } => ResultDisplay::Error {
            message,
            elapsed_ms: *elapsed_ms,
        },
    }
}

fn sidebar_label(item: &SidebarItem) -> String {
    match item {
        SidebarItem::Connection { name, driver, .. } => format!("{name} ({driver})"),
        SidebarItem::Schema { name } => name.clone(),
        SidebarItem::Table { name, .. } => name.clone(),
    }
}

fn sidebar_depth(item: &SidebarItem) -> u8 {
    match item {
        SidebarItem::Connection { .. } => 0,
        SidebarItem::Schema { .. } => 1,
        SidebarItem::Table { .. } => 2,
    }
}

fn sidebar_kind(item: &SidebarItem) -> SidebarRowKind {
    match item {
        SidebarItem::Connection { active: true, .. } => SidebarRowKind::ActiveConnection,
        SidebarItem::Connection { .. } => SidebarRowKind::Connection,
        SidebarItem::Schema { .. } => SidebarRowKind::Schema,
        SidebarItem::Table { kind, .. } => match kind {
            TableKind::Table => SidebarRowKind::Table,
            TableKind::View => SidebarRowKind::View,
            TableKind::MaterializedView => SidebarRowKind::MaterializedView,
            TableKind::SystemTable => SidebarRowKind::SystemTable,
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
