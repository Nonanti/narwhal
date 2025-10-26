//! Headless application state.
//!
//! [`AppCore`] owns every piece of state the UI depends on but contains no
//! terminal-IO logic. The render path takes a [`ratatui::Frame`] from the
//! caller, and key events come in as parsed crossterm [`KeyEvent`]s, so the
//! core is fully usable with `ratatui::backend::TestBackend` in tests.

use std::sync::Arc;

use crossterm::event::{KeyCode as CtKey, KeyEvent, KeyModifiers};
use narwhal_config::{ConnectionsFile, CredentialStore, InMemoryStore};
use narwhal_core::{
    Column, ColumnHeader, ConnectionConfig, IsolationLevel, Row, TableKind, TableSchema,
};
use narwhal_history::Journal;
use narwhal_tui::{
    render_root, render_wizard, translate_key_event, CellEditView, CellPopup, CompletionItemView,
    CompletionPopupView, EditorBuffer, ExplainPlanLine, Pane, ResultDisplay, ResultView,
    RootLayout, SearchHighlight, SidebarRow, SidebarRowKind, SidebarView, Theme, WizardFieldView,
    WizardView,
};
use narwhal_vim::{Action, Mode, Vim};
use ratatui::layout::Rect;
use ratatui::Frame;
use tokio::sync::{mpsc, Mutex};
use tracing::debug;
use uuid::Uuid;

use crate::clipboard::{Clipboard, InMemoryClipboard};
use crate::commands::{parse, Command, DumpTarget, IsolationArg};
use crate::completion::{gather as gather_completions, Completion, CompletionKind};
use crate::ddl::{build_dump, build_table_ddl};
use crate::explain::{parse as parse_plan, wrap_explain};
use crate::export::{export_rows, ExportFormat};
use crate::registry::DriverRegistry;
use crate::run::{spawn_run, ActiveCancel, RunContext, RunMode, RunRequest, RunTarget, RunUpdate};
use crate::session::Session;
use crate::wizard::{ConnectionWizard, DRIVERS};
use narwhal_plugin::{
    CommandContext as PluginCommandContext, CommandOutcome as PluginCommandOutcome, Plugin,
    PluginError, PluginRegistry, PluginResult, SqlExecutor,
};
use narwhal_plugin_lua::LuaPlugin;

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
        /// Origin metadata for cell editing. `None` for ad-hoc statements;
        /// `Some` when the rows came from a sidebar table preview, so we
        /// know the schema/table/PK columns to build an UPDATE.
        source: Option<RowSource>,
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

/// Where a [`ResultState::Rows`] set originated. Populated only when the
/// rows came from a preview (sidebar `o` or analogous flow) so that the
/// edit path knows the table and primary key columns to target.
#[derive(Debug, Clone)]
pub struct RowSource {
    pub schema: String,
    pub table: String,
    pub columns: Vec<Column>,
    /// Offset of the first row in this page, relative to the unbounded
    /// `SELECT * FROM <table>`. Used by `:next` / `:prev`.
    pub offset: usize,
    /// Page size that produced this page. `:page-size` updates this for
    /// subsequent previews.
    pub limit: usize,
}

/// In-flight completion popup.
#[derive(Debug, Clone)]
pub struct CompletionState {
    /// Candidate list, already filtered and sorted.
    pub items: Vec<Completion>,
    /// Currently highlighted index.
    pub selected: usize,
    /// The prefix that produced [`items`]. Used to detect when the user
    /// keeps typing and the popup needs to refilter.
    pub prefix: String,
}

/// In-flight cell edit. `buffer` is what the user is currently typing;
/// `original` is the cell's textual representation when the edit opened
/// (used for the cancel path and as the default in the popup).
#[derive(Debug, Clone)]
pub struct CellEdit {
    pub column_name: String,
    pub column_type: String,
    pub row_index: usize,
    pub column_index: usize,
    pub original: String,
    pub buffer: String,
}

/// Search state attached to a result pane.
#[derive(Debug, Default)]
pub struct ResultSearch {
    pub query: String,
    pub matches: Vec<usize>,
    pub current: Option<usize>,
    /// `true` while the user is typing the pattern; `false` after Enter.
    pub editing: bool,
}

/// One editor tab: a buffer + the most recent result it produced.
pub struct Tab {
    pub name: String,
    pub editor: EditorBuffer,
    pub result: ResultState,
    pub result_view: ResultView,
    pub search: Option<ResultSearch>,
    pub editing: Option<CellEdit>,
    pub completion: Option<CompletionState>,
    /// Page size used by the next sidebar preview. Stored per-tab so a
    /// user paging through one table doesn't disturb another tab.
    pub page_size: usize,
    /// Pending row source to attach to the next `Rows` result. Populated
    /// by `preview_sidebar_selection` and consumed in `finish_run`.
    pub pending_source: Option<RowSource>,
}

impl Tab {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            editor: EditorBuffer::new(),
            result: ResultState::Empty,
            result_view: ResultView::new(),
            search: None,
            editing: None,
            completion: None,
            page_size: 100,
            pending_source: None,
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
    connections_path: Option<std::path::PathBuf>,
    credentials: Arc<dyn CredentialStore>,
    clipboard: Arc<dyn Clipboard>,
    plugins: Arc<PluginRegistry>,
    /// Shared handle the plugin SQL executor reads on every
    /// `narwhal.sql_run` call. Updated whenever a session opens or
    /// closes so scripts always target the currently-active
    /// connection.
    plugin_state: Arc<std::sync::Mutex<PluginConnectionState>>,
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
    /// One-shot warning carried over from a plugin (transform or command
    /// hook) so that the final 'done · N statement(s)' AllDone message
    /// doesn't overwrite it silently. Cleared after it bubbles up.
    plugin_warning: Option<String>,
    running: bool,
    cancel_slot: ActiveCancel,
    should_quit: bool,
    wizard: Option<ConnectionWizard>,
    wizard_error: Option<String>,
    run_tx: mpsc::Sender<RunUpdate>,
    pub(crate) run_rx: mpsc::Receiver<RunUpdate>,
}

impl AppCore {
    pub fn new(
        registry: DriverRegistry,
        connections: ConnectionsFile,
        history: Option<Arc<Journal>>,
    ) -> Self {
        Self::with_credentials(
            registry,
            connections,
            history,
            Arc::new(InMemoryStore::new()),
        )
    }

    /// Same as [`Self::new`] but lets the caller inject a credential store.
    /// Production builds pass a [`narwhal_config::KeyringStore`]; tests use
    /// [`InMemoryStore`].
    pub fn with_credentials(
        registry: DriverRegistry,
        connections: ConnectionsFile,
        history: Option<Arc<Journal>>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        Self::with_services(
            registry,
            connections,
            history,
            credentials,
            Arc::new(InMemoryClipboard::new()),
        )
    }

    /// Inject every replaceable runtime service in one call. The binary
    /// passes [`narwhal_config::KeyringStore`] and
    /// [`crate::clipboard::ArboardClipboard`]; tests pass the in-memory
    /// variants.
    pub fn with_services(
        registry: DriverRegistry,
        connections: ConnectionsFile,
        history: Option<Arc<Journal>>,
        credentials: Arc<dyn CredentialStore>,
        clipboard: Arc<dyn Clipboard>,
    ) -> Self {
        let (run_tx, run_rx) = mpsc::channel(RUN_CHANNEL_CAPACITY);
        let mut this = Self::new_inner(
            registry,
            connections,
            history,
            credentials,
            clipboard,
            run_tx,
            run_rx,
        );
        this.rebuild_sidebar();
        this
    }

    /// Read-only accessor for the active clipboard. Mostly useful for
    /// tests that want to assert what was just yanked.
    pub fn clipboard(&self) -> Arc<dyn Clipboard> {
        Arc::clone(&self.clipboard)
    }

    #[allow(clippy::too_many_arguments)]
    fn new_inner(
        registry: DriverRegistry,
        connections: ConnectionsFile,
        history: Option<Arc<Journal>>,
        credentials: Arc<dyn CredentialStore>,
        clipboard: Arc<dyn Clipboard>,
        run_tx: mpsc::Sender<RunUpdate>,
        run_rx: mpsc::Receiver<RunUpdate>,
    ) -> Self {
        Self {
            registry,
            connections,
            connections_path: None,
            credentials,
            clipboard,
            plugins: {
                let mut reg = PluginRegistry::new();
                reg.reserve_builtins(crate::commands::BUILTIN_COMMAND_NAMES.iter().copied());
                Arc::new(reg)
            },
            plugin_state: Arc::new(std::sync::Mutex::new(PluginConnectionState::default())),
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
            plugin_warning: None,
            running: false,
            cancel_slot: Arc::new(Mutex::new(None)),
            should_quit: false,
            wizard: None,
            wizard_error: None,
            run_tx,
            run_rx,
        }
    }

    /// Inform the core where to persist new connections produced by the
    /// `:add` wizard. Called by [`crate::app::App::new`].
    pub fn set_connections_path(&mut self, path: std::path::PathBuf) {
        self.connections_path = Some(path);
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
        let transaction_badge = self.session.as_ref().and_then(|s| {
            let txn = s.transaction.as_ref()?;
            Some(if txn.savepoints.is_empty() {
                "TX".to_owned()
            } else {
                format!("TX·sp:{}", txn.savepoints.len())
            })
        });
        let editor_title = self.editor_title_with_tabs();

        let tab = &mut self.tabs[self.active_tab];
        let search_view = tab.search.as_ref().map(|s| SearchHighlight {
            matches: &s.matches,
            current: s.current,
        });
        let result_display = display_from_state(&tab.result, search_view.as_ref());
        let completion_item_views: Vec<CompletionItemView<'_>> = tab
            .completion
            .as_ref()
            .map(|s| {
                s.items
                    .iter()
                    .map(|c| CompletionItemView {
                        text: c.text.as_str(),
                        kind_glyph: match c.kind {
                            CompletionKind::Keyword => "K",
                            CompletionKind::Table => "T",
                            CompletionKind::Column => "C",
                        },
                        detail: c.detail.as_deref(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let completion_view = tab.completion.as_ref().map(|s| CompletionPopupView {
            items: &completion_item_views,
            selected: s.selected,
            anchor: (0, 0), // overwritten by render_root once it knows the editor rect
        });
        let mut layout = RootLayout {
            mode: self.vim.mode(),
            focus: self.focus,
            connection_label: &connection_label,
            status_message: &self.status_message,
            running: self.running,
            transaction_badge: transaction_badge.as_deref(),
            theme: &self.theme,
            sidebar: sidebar_view,
            editor: &mut tab.editor,
            editor_title: &editor_title,
            result_view: &mut tab.result_view,
            result: result_display,
            completion: completion_view,
        };
        render_root(frame, area, &mut layout);

        if let Some(wizard) = self.wizard.as_ref() {
            let view = WizardView {
                drivers: DRIVERS,
                driver_index: wizard.driver_index,
                fields: wizard
                    .fields
                    .iter()
                    .map(|f| WizardFieldView {
                        label: f.label,
                        value: &f.value,
                        secret: f.secret,
                    })
                    .collect(),
                focused: wizard.focused,
                error: self.wizard_error.as_deref(),
            };
            render_wizard(frame, area, &view, &self.theme);
        }
    }

    // ----- input -----

    pub fn handle_key(&mut self, key: KeyEvent) {
        if self.wizard.is_some() {
            self.handle_wizard_key(key);
            return;
        }
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
        // The completion popup is modal while it's open.
        if self.tabs[self.active_tab].completion.is_some() && self.handle_completion_key(key) {
            return;
        }
        // In insert mode, intercept a plain Tab so it triggers completion
        // instead of being forwarded to the vim layer.
        if self.vim.mode() == Mode::Insert && key.code == CtKey::Tab && key.modifiers.is_empty() {
            self.trigger_completion();
            return;
        }
        let Some(logical) = translate_key_event(key) else {
            return;
        };
        let action = self.vim.handle(logical);
        self.apply_action(action);
    }

    // ----- completion -----

    fn trigger_completion(&mut self) {
        let prefix = self.tabs[self.active_tab].editor.current_word_prefix();
        if prefix.is_empty() {
            // Empty prefix: behave like a plain insert (4 spaces).
            self.tabs[self.active_tab].editor.insert_str("    ");
            return;
        }
        let schemas = self
            .session
            .as_ref()
            .map(|s| s.schemas.as_slice())
            .unwrap_or(&[]);
        let items = gather_completions(&prefix, schemas, 50);
        if items.is_empty() {
            self.status_message = format!("no completions for '{prefix}'");
            return;
        }
        if items.len() == 1 {
            // Exactly one match: insert it without showing the popup.
            let only = items[0].text.clone();
            self.tabs[self.active_tab]
                .editor
                .replace_current_word_with(&only);
            self.status_message = format!("completed: {only}");
            return;
        }
        self.tabs[self.active_tab].completion = Some(CompletionState {
            items,
            selected: 0,
            prefix,
        });
        self.status_message =
            "completion: Tab/Shift-Tab cycles · Enter selects · Esc cancels".into();
    }

    /// Returns `true` when the key was consumed by the completion popup.
    fn handle_completion_key(&mut self, key: KeyEvent) -> bool {
        let Some(state) = self.tabs[self.active_tab].completion.as_mut() else {
            return false;
        };
        match key.code {
            CtKey::Esc => {
                self.tabs[self.active_tab].completion = None;
                self.status_message = "completion cancelled".into();
                true
            }
            CtKey::Enter => {
                let choice = state.items[state.selected].text.clone();
                self.tabs[self.active_tab]
                    .editor
                    .replace_current_word_with(&choice);
                self.tabs[self.active_tab].completion = None;
                self.status_message = format!("completed: {choice}");
                true
            }
            CtKey::Tab => {
                state.selected = (state.selected + 1) % state.items.len();
                true
            }
            CtKey::BackTab => {
                let len = state.items.len();
                state.selected = (state.selected + len - 1) % len;
                true
            }
            CtKey::Up => {
                let len = state.items.len();
                state.selected = (state.selected + len - 1) % len;
                true
            }
            CtKey::Down => {
                state.selected = (state.selected + 1) % state.items.len();
                true
            }
            // Any other key dismisses the popup and falls through to the
            // editor so the keystroke takes effect.
            _ => {
                self.tabs[self.active_tab].completion = None;
                false
            }
        }
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
        self.run_preview(&schema, &name, 0);
    }

    /// Dispatch a `SELECT * FROM schema.table LIMIT n OFFSET k` and attach
    /// the table's schema as the result's row source so cell edits and
    /// pagination work.
    fn run_preview(&mut self, schema: &str, table: &str, offset: usize) {
        let Some(session) = self.session.as_ref() else {
            self.status_message = "no active connection".into();
            return;
        };
        let dialect = session.dialect();
        let limit = self.tabs[self.active_tab].page_size;
        let pool = session.pool.clone();
        let schema_owned = schema.to_owned();
        let name_owned = table.to_owned();
        let described = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let mut conn = pool
                    .acquire()
                    .await
                    .map_err(|e| narwhal_core::Error::Connection(e.to_string()))?;
                conn.describe_table(&schema_owned, &name_owned).await
            })
        });
        let source = match described {
            Ok(ts) => Some(RowSource {
                schema: schema.to_owned(),
                table: table.to_owned(),
                columns: ts.columns,
                offset,
                limit,
            }),
            Err(error) => {
                debug!(target: "narwhal::app", error = %error, "describe_table for preview failed; rows will be read-only");
                None
            }
        };
        let sql = crate::ddl::preview_query_paged(schema, table, limit, offset, dialect);
        self.tabs[self.active_tab].pending_source = source;
        self.dispatch_batch(vec![sql], RunMode::Execute);
        self.focus = Pane::Results;
    }

    fn next_page(&mut self) {
        let Some((schema, table, offset)) = self.current_preview_target() else {
            self.status_message = "no preview to paginate; select a table first".into();
            return;
        };
        let limit = self.tabs[self.active_tab].page_size;
        self.run_preview(&schema, &table, offset + limit);
    }

    fn prev_page(&mut self) {
        let Some((schema, table, offset)) = self.current_preview_target() else {
            self.status_message = "no preview to paginate; select a table first".into();
            return;
        };
        if offset == 0 {
            self.status_message = "already on the first page".into();
            return;
        }
        let limit = self.tabs[self.active_tab].page_size;
        let new_offset = offset.saturating_sub(limit);
        self.run_preview(&schema, &table, new_offset);
    }

    fn set_page_size(&mut self, size: usize) {
        self.tabs[self.active_tab].page_size = size;
        self.status_message = format!("page size set to {size}");
    }

    fn current_preview_target(&self) -> Option<(String, String, usize)> {
        match &self.tabs[self.active_tab].result {
            ResultState::Rows {
                source: Some(s), ..
            } => Some((s.schema.clone(), s.table.clone(), s.offset)),
            _ => None,
        }
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
        if self.tabs[self.active_tab].editing.is_some() {
            self.handle_cell_edit_key(key);
            return;
        }
        if self.tabs[self.active_tab].result_view.popup.is_some() {
            if matches!(key.code, CtKey::Esc | CtKey::Char('q') | CtKey::Enter) {
                self.tabs[self.active_tab].result_view.popup = None;
            }
            return;
        }
        if let Some(search) = self.tabs[self.active_tab].search.as_mut() {
            if search.editing {
                match key.code {
                    CtKey::Esc => {
                        self.tabs[self.active_tab].search = None;
                        self.status_message = "search cancelled".into();
                    }
                    CtKey::Enter => {
                        search.editing = false;
                        self.refresh_search_matches();
                        self.jump_to_current_match();
                    }
                    CtKey::Backspace => {
                        search.query.pop();
                        self.refresh_search_matches();
                    }
                    CtKey::Char(c) => {
                        search.query.push(c);
                        self.refresh_search_matches();
                    }
                    _ => {}
                }
                return;
            }
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
            CtKey::Char('/') => self.start_search(),
            CtKey::Char('n') => self.advance_search(1),
            CtKey::Char('N') => self.advance_search(-1),
            CtKey::Esc if self.tabs[self.active_tab].search.take().is_some() => {
                self.status_message = "search cleared".into();
            }
            CtKey::Enter => self.open_cell_popup(),
            CtKey::Char('e') => self.start_cell_edit(),
            CtKey::Char('y') => self.yank_cell(),
            CtKey::Char('Y') => self.yank_row(),
            _ => {}
        }
    }

    // ----- yank -----

    fn yank_cell(&mut self) {
        let tab = &self.tabs[self.active_tab];
        let (rows, _columns) = match &tab.result {
            ResultState::Rows { rows, columns, .. }
            | ResultState::Running { rows, columns, .. } => (rows, columns),
            _ => {
                self.status_message = "no cell to yank".into();
                return;
            }
        };
        let row_idx = tab.result_view.state.selected().unwrap_or(0);
        let col_idx = tab.result_view.column_index;
        let Some(value) = rows.get(row_idx).and_then(|r| r.0.get(col_idx)) else {
            self.status_message = "no cell selected".into();
            return;
        };
        let text = match value {
            narwhal_core::Value::Null => String::new(),
            other => other.render(),
        };
        match self.clipboard.set_text(&text) {
            Ok(()) => {
                self.status_message = format!("yanked {} char(s) to clipboard", text.len());
            }
            Err(error) => {
                self.status_message = format!("yank failed: {error}");
            }
        }
    }

    fn yank_row(&mut self) {
        let tab = &self.tabs[self.active_tab];
        let rows = match &tab.result {
            ResultState::Rows { rows, .. } | ResultState::Running { rows, .. } => rows,
            _ => {
                self.status_message = "no row to yank".into();
                return;
            }
        };
        let row_idx = tab.result_view.state.selected().unwrap_or(0);
        let Some(row) = rows.get(row_idx) else {
            self.status_message = "no row selected".into();
            return;
        };
        let text = row
            .0
            .iter()
            .map(|v| match v {
                narwhal_core::Value::Null => String::new(),
                other => other.render(),
            })
            .collect::<Vec<_>>()
            .join("\t");
        match self.clipboard.set_text(&text) {
            Ok(()) => {
                self.status_message = format!("yanked row ({} cell(s)) to clipboard", row.0.len());
            }
            Err(error) => {
                self.status_message = format!("yank failed: {error}");
            }
        }
    }

    fn start_cell_edit(&mut self) {
        // Gather the data we need by value first, then mutate.
        let prepared = {
            let tab = &self.tabs[self.active_tab];
            let (columns, rows, source) = match &tab.result {
                ResultState::Rows {
                    columns,
                    rows,
                    source: Some(source),
                    ..
                } => (columns, rows, source),
                ResultState::Rows { source: None, .. } => {
                    self.status_message =
                        "this result is read-only (no row source); preview a table to edit".into();
                    return;
                }
                _ => {
                    self.status_message = "no editable cell here".into();
                    return;
                }
            };
            if columns.is_empty() || rows.is_empty() {
                self.status_message = "no rows to edit".into();
                return;
            }
            if !source.columns.iter().any(|c| c.primary_key) {
                self.status_message =
                    format!("{}: no primary key, cell edits are disabled", source.table);
                return;
            }
            let row_index = tab.result_view.state.selected().unwrap_or(0);
            let col_index = tab.result_view.column_index;
            let Some(row) = rows.get(row_index) else {
                self.status_message = "select a row first (j/k)".into();
                return;
            };
            let Some(column) = columns.get(col_index) else {
                self.status_message = "select a column first (h/l)".into();
                return;
            };
            let cell = row.0.get(col_index);
            let original = cell.map(|v| v.render()).unwrap_or_default();
            let buffer = if matches!(cell, Some(narwhal_core::Value::Null) | None) {
                String::new()
            } else {
                original.clone()
            };
            (
                column.name.clone(),
                column.data_type.clone(),
                row_index,
                col_index,
                original,
                buffer,
            )
        };
        let (column_name, column_type, row_index, column_index, original, buffer) = prepared;
        let tab = &mut self.tabs[self.active_tab];
        tab.editing = Some(CellEdit {
            column_name: column_name.clone(),
            column_type: column_type.clone(),
            row_index,
            column_index,
            original,
            buffer: buffer.clone(),
        });
        tab.result_view.edit = Some(CellEditView {
            column_name,
            column_type,
            row_index,
            buffer,
            error: None,
        });
        self.status_message = "edit: Enter saves · Esc cancels".into();
    }

    fn handle_cell_edit_key(&mut self, key: KeyEvent) {
        let Some(edit) = self.tabs[self.active_tab].editing.as_mut() else {
            return;
        };
        match key.code {
            CtKey::Esc => {
                self.tabs[self.active_tab].editing = None;
                self.tabs[self.active_tab].result_view.edit = None;
                self.status_message = "edit cancelled".into();
            }
            CtKey::Enter => self.commit_cell_edit(),
            CtKey::Backspace => {
                edit.buffer.pop();
                self.sync_edit_view();
            }
            CtKey::Char(c) => {
                edit.buffer.push(c);
                self.sync_edit_view();
            }
            _ => {}
        }
    }

    fn sync_edit_view(&mut self) {
        let tab = &mut self.tabs[self.active_tab];
        if let (Some(edit), Some(view)) = (tab.editing.as_ref(), tab.result_view.edit.as_mut()) {
            view.buffer = edit.buffer.clone();
            view.error = None;
        }
    }

    fn commit_cell_edit(&mut self) {
        let Some(edit) = self.tabs[self.active_tab].editing.clone() else {
            return;
        };
        let Some(session) = self.session.as_ref() else {
            self.status_message = "no active connection".into();
            return;
        };
        // Extract everything we need from the result before mutating state.
        let (columns, rows, source) = if let ResultState::Rows {
            columns,
            rows,
            source: Some(source),
            ..
        } = &self.tabs[self.active_tab].result
        {
            (columns.clone(), rows.clone(), source.clone())
        } else {
            self.status_message = "result is no longer editable".into();
            return;
        };
        let Some(row) = rows.get(edit.row_index).cloned() else {
            self.status_message = "row went away under the editor".into();
            return;
        };
        let new_value = crate::edit::parse_input(&edit.buffer);
        let column_order: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let dialect = session.dialect();
        let compiled = match crate::edit::build_update(
            &source.schema,
            &source.table,
            &source.columns,
            &edit.column_name,
            &new_value,
            &row,
            &column_order,
            dialect,
        ) {
            Ok(c) => c,
            Err(error) => {
                self.set_edit_error(error);
                return;
            }
        };
        // Execute the UPDATE on a pool connection (or the open transaction
        // if there is one). We do this synchronously via block_in_place
        // because the edit popup is modal; the UI is already paused.
        let target = match session.transaction.as_ref() {
            Some(txn) => RunTarget::Pinned(txn.conn.clone()),
            None => RunTarget::Pool(session.pool.clone()),
        };
        let sql = compiled.sql.clone();
        let params = compiled.params;
        let outcome = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                match target {
                    RunTarget::Pool(pool) => {
                        let mut conn = pool
                            .acquire()
                            .await
                            .map_err(|e| narwhal_core::Error::Connection(e.to_string()))?;
                        conn.execute(&sql, &params).await
                    }
                    RunTarget::Pinned(handle) => {
                        let mut guard = handle.lock().await;
                        guard.execute(&sql, &params).await
                    }
                }
            })
        });
        match outcome {
            Ok(qr) => {
                let affected = qr.rows_affected.unwrap_or(0);
                if affected != 1 {
                    self.set_edit_error(format!(
                        "refused: UPDATE matched {affected} rows (expected exactly 1)"
                    ));
                    return;
                }
                // Patch the in-memory cell with the parsed value so the
                // grid reflects the new state without re-fetching.
                if let ResultState::Rows { rows, .. } = &mut self.tabs[self.active_tab].result {
                    if let Some(row) = rows.get_mut(edit.row_index) {
                        if let Some(cell) = row.0.get_mut(edit.column_index) {
                            *cell = new_value;
                        }
                    }
                }
                self.tabs[self.active_tab].editing = None;
                self.tabs[self.active_tab].result_view.edit = None;
                self.status_message = format!("updated 1 row in {}", source.table);
            }
            Err(error) => {
                self.set_edit_error(error.to_string());
            }
        }
    }

    fn set_edit_error(&mut self, message: String) {
        if let Some(view) = self.tabs[self.active_tab].result_view.edit.as_mut() {
            view.error = Some(message.clone());
        }
        self.status_message = format!("edit failed: {message}");
    }

    fn start_search(&mut self) {
        if !matches!(
            self.tabs[self.active_tab].result,
            ResultState::Rows { .. } | ResultState::Running { .. }
        ) {
            self.status_message = "no result to search".into();
            return;
        }
        self.tabs[self.active_tab].search = Some(ResultSearch {
            query: String::new(),
            matches: Vec::new(),
            current: None,
            editing: true,
        });
        self.status_message = "search: ".into();
    }

    fn refresh_search_matches(&mut self) {
        let needle = match self.tabs[self.active_tab].search.as_ref() {
            Some(s) if !s.query.is_empty() => s.query.to_lowercase(),
            Some(_) => {
                if let Some(s) = self.tabs[self.active_tab].search.as_mut() {
                    s.matches.clear();
                    s.current = None;
                }
                self.status_message = "search: ".into();
                return;
            }
            None => return,
        };
        let matches = match &self.tabs[self.active_tab].result {
            ResultState::Rows { rows, .. } | ResultState::Running { rows, .. } => rows
                .iter()
                .enumerate()
                .filter_map(|(i, row)| {
                    row.0
                        .iter()
                        .any(|v| v.render().to_lowercase().contains(&needle))
                        .then_some(i)
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let total = matches.len();
        let search = self.tabs[self.active_tab].search.as_mut().unwrap();
        let query = search.query.clone();
        search.matches = matches;
        search.current = if total == 0 { None } else { Some(0) };
        self.status_message = if total == 0 {
            format!("search: {query} · no matches")
        } else {
            format!("search: {query} · 1/{total}")
        };
    }

    fn advance_search(&mut self, delta: i32) {
        let Some(search) = self.tabs[self.active_tab].search.as_mut() else {
            return;
        };
        if search.matches.is_empty() {
            return;
        }
        let len = search.matches.len() as i32;
        let current = search.current.unwrap_or(0) as i32;
        let next = (current + delta).rem_euclid(len) as usize;
        search.current = Some(next);
        let total = search.matches.len();
        let query = search.query.clone();
        self.status_message = format!("search: {query} · {}/{}", next + 1, total);
        self.jump_to_current_match();
    }

    fn jump_to_current_match(&mut self) {
        let Some(search) = self.tabs[self.active_tab].search.as_ref() else {
            return;
        };
        let Some(idx) = search.current.and_then(|c| search.matches.get(c).copied()) else {
            return;
        };
        self.tabs[self.active_tab]
            .result_view
            .state
            .select(Some(idx));
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
            Command::Add => self.start_wizard(),
            Command::NextPage => self.next_page(),
            Command::PrevPage => self.prev_page(),
            Command::PageSize(n) => self.set_page_size(n),
            Command::Begin(iso) => self.begin_transaction(iso),
            Command::Commit => self.commit_transaction(),
            Command::Rollback => self.rollback_transaction(),
            Command::Savepoint(name) => self.savepoint(&name),
            Command::Release(name) => self.release_savepoint(&name),
            Command::RollbackTo(name) => self.rollback_to_savepoint(&name),
            Command::Remove(name) => self.remove_connection(&name),
            Command::Forget(name) => self.forget_password(&name),
            Command::PluginLoad(path) => self.load_plugin(&path),
            Command::PluginList => self.list_plugins(),
            Command::NewTab => self.new_tab(),
            Command::CloseTab => self.close_tab(),
            Command::NextTab => self.cycle_tab(1),
            Command::PrevTab => self.cycle_tab(-1),
            Command::Help(None) => {
                self.status_message =
                    "open <name> · close · refresh · run · run-all · stream · stream-all · explain · export <csv|json> <path> · cancel · quit"
                        .into();
            }
            Command::Help(Some(name)) => {
                // Built-ins first — aliases (`o`, `q`, ...) resolve back
                // to their primary key before the lookup.
                let resolved = crate::commands::resolve_builtin_alias(&name);
                if let Some((_, desc)) = crate::commands::BUILTIN_COMMAND_DESCRIPTIONS
                    .iter()
                    .find(|(key, _)| *key == resolved)
                {
                    self.status_message = format!(":{name} — {desc}");
                } else if let Some(plugin) = self.plugins.plugin_for(&name) {
                    // Plugin command: pull the descriptor straight off
                    // the owning plugin instead of walking the full
                    // catalogue. plugin_for already located it.
                    let desc = plugin
                        .commands()
                        .into_iter()
                        .find(|cmd| cmd.name == name)
                        .map(|cmd| cmd.description)
                        .unwrap_or_else(|| "(no description)".into());
                    self.status_message = format!(":{name} — {desc}");
                } else {
                    self.status_message = format!("unknown command: {name}");
                }
            }
            Command::Empty => {}
            Command::Unknown(text) => {
                // Before reporting the command as unknown, give the
                // plugin registry a chance to claim it. The first whitespace
                // token is the command name; everything after is passed to
                // the handler verbatim.
                let (head, arg) = split_head_arg(&text);
                if self.plugins.plugin_for(head).is_some() {
                    self.dispatch_plugin(head, arg);
                } else {
                    self.status_message = format!("unknown command: {text}");
                }
            }
        }
    }

    // ----- plugins -----

    /// Read-only handle to the plugin registry, useful for tests.
    /// The `Arc` derefs transparently so callers can use `&PluginRegistry`
    /// methods without caring about the indirection.
    pub fn plugins(&self) -> &PluginRegistry {
        &self.plugins
    }

    /// Mutable handle so callers (binary or tests) can register plugins
    /// without going through the `:plug-load` command path.
    /// Uses `Arc::make_mut` so the clone-on-write only materialises when
    /// the caller actually mutates — dispatch paths that merely read pay
    /// a single ref-count bump.
    pub fn plugins_mut(&mut self) -> &mut PluginRegistry {
        Arc::make_mut(&mut self.plugins)
    }

    /// Register a freshly-built [`LuaPlugin`], wiring it into the SQL
    /// executor first so `narwhal.sql_run` works from inside the script.
    /// All host-driven registration paths (`:plug-load`,
    /// `auto_load_plugins`, integration tests) funnel through this so
    /// the executor injection is impossible to forget.
    pub fn register_lua_plugin(&mut self, plugin: LuaPlugin) -> PluginResult<usize> {
        let executor: Arc<dyn SqlExecutor> = Arc::new(AppPluginExecutor {
            state: self.plugin_state.clone(),
        });
        plugin.install_executor(executor)?;
        Arc::make_mut(&mut self.plugins).register(plugin)
    }

    /// Scan `dir` for top-level `*.lua` files and register each as a
    /// plugin. Returns the number of plugins that loaded successfully.
    /// Failures are accumulated into the status bar so the user notices
    /// at start-up; the rest of the directory keeps loading.
    ///
    /// Missing or unreadable directories are not an error — narwhal runs
    /// fine without any plugins.
    pub fn auto_load_plugins(&mut self, dir: &std::path::Path) -> usize {
        let entries = match std::fs::read_dir(dir) {
            Ok(it) => it,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return 0,
            Err(err) => {
                // Permission denied, ENOTDIR, etc. — the user almost
                // certainly wants to know; running without plugins is
                // also a valid choice, so we just warn rather than
                // abort startup.
                self.plugin_warning = Some(format!(
                    "plugin auto-load: cannot read {}: {err}",
                    dir.display()
                ));
                return 0;
            }
        };
        let mut paths: Vec<std::path::PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .and_then(|s| s.to_str())
                        .map(|s| s.eq_ignore_ascii_case("lua"))
                        .unwrap_or(false)
            })
            .collect();
        // Deterministic order so the registry index is reproducible.
        paths.sort();

        let mut loaded = 0usize;
        let mut failures: Vec<String> = Vec::new();
        for path in &paths {
            match LuaPlugin::from_path(path) {
                Ok(plugin) => match self.register_lua_plugin(plugin) {
                    Ok(_) => loaded += 1,
                    Err(e) => failures.push(format!("{}: {e}", path.display())),
                },
                Err(e) => failures.push(format!("{}: {e}", path.display())),
            }
        }

        if !failures.is_empty() {
            // Surface failures via the plugin_warning slot so they survive
            // the next status message rewrite.
            self.plugin_warning = Some(format!(
                "{} plugin(s) failed to load: {}",
                failures.len(),
                failures.join("; ")
            ));
        }
        if loaded > 0 {
            self.status_message = format!("auto-loaded {loaded} plugin(s) from {}", dir.display());
        }
        loaded
    }

    fn load_plugin(&mut self, path: &str) {
        let plugin = match LuaPlugin::from_path(path) {
            Ok(p) => p,
            Err(e) => {
                self.status_message = format!("plug-load failed: {e}");
                return;
            }
        };
        let name = plugin.name().to_owned();
        let cmd_count = plugin.commands().len();
        match self.register_lua_plugin(plugin) {
            Ok(_) => {
                self.status_message = format!("plugin '{name}' loaded ({cmd_count} command(s))");
            }
            Err(e) => {
                self.status_message = format!("plug-load failed: {e}");
            }
        }
    }

    fn list_plugins(&mut self) {
        let catalogue = self.plugins.catalogue();
        if catalogue.is_empty() {
            self.status_message = "no plugins loaded; use :plug-load <file.lua>".into();
            return;
        }
        let summary = catalogue
            .iter()
            .map(|(plugin, cmd)| format!("{}:{} — {}", plugin, cmd.name, cmd.description))
            .collect::<Vec<_>>()
            .join(" · ");
        self.status_message = summary;
    }

    fn dispatch_plugin(&mut self, command: &str, argument: &str) {
        let editor_text = self.tabs[self.active_tab].editor.entire_text();
        let ctx = PluginCommandContext::new(argument).with_editor_text(&editor_text);
        let plugins = Arc::clone(&self.plugins);
        let command_owned = command.to_owned();
        // Plugin dispatch is async by trait definition; bridge to the
        // synchronous command handler via block_in_place + the current
        // Tokio handle, the same pattern used elsewhere in AppCore.
        let outcome = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async move { plugins.dispatch(&command_owned, ctx).await })
        });
        match outcome {
            Ok(PluginCommandOutcome::Status { message }) => {
                self.status_message = message;
            }
            Ok(PluginCommandOutcome::InsertSql { sql, append }) => {
                if !append {
                    self.tabs[self.active_tab].editor.clear();
                }
                self.tabs[self.active_tab].editor.insert_str(&sql);
                self.status_message = format!("plugin inserted {} char(s) of SQL", sql.len());
            }
            Ok(PluginCommandOutcome::Silent) => {}
            Err(PluginError::Unknown(name)) => {
                self.status_message = format!("unknown command: {name}");
            }
            Err(error) => {
                self.status_message = format!("plugin error: {error}");
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
        let password = match self.credentials.get(config.id) {
            Ok(secret) => secret,
            Err(error) => {
                debug!(target: "narwhal::app", error = %error, "keyring lookup failed; continuing without password");
                None
            }
        };
        self.open_connection_with_password(config, password);
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
                self.status_message = format!("connect failed: {error}");
            }
        }
    }

    fn close_session(&mut self) {
        if self.session.take().is_some() {
            let mut state = self.plugin_state.lock().expect("plugin_state poisoned");
            state.pool = None;
            state.in_transaction = false;
            drop(state);
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
        let target = match session.transaction.as_ref() {
            Some(txn) => RunTarget::Pinned(txn.conn.clone()),
            None => RunTarget::Pool(session.pool.clone()),
        };
        let ctx = RunContext {
            target,
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

    fn start_wizard(&mut self) {
        self.wizard = Some(ConnectionWizard::new());
        self.wizard_error = None;
        self.status_message = "add: Tab moves · ←/→ driver · Enter saves · Esc cancels".into();
    }

    // ----- transactions -----

    fn begin_transaction(&mut self, isolation: Option<IsolationArg>) {
        if self.running {
            self.status_message = "a query is already running".into();
            return;
        }
        let Some(session) = self.session.as_mut() else {
            self.status_message = "no active connection".into();
            return;
        };
        if session.transaction.is_some() {
            self.status_message = "a transaction is already open".into();
            return;
        }
        let iso = isolation.map(map_isolation);
        let pool = session.pool.clone();
        let result: std::result::Result<narwhal_pool::PooledConnection, narwhal_core::Error> =
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async move {
                    let mut conn = pool
                        .acquire()
                        .await
                        .map_err(|e| narwhal_core::Error::Connection(e.to_string()))?;
                    match iso {
                        Some(level) => conn.begin_with(level).await?,
                        None => conn.begin().await?,
                    }
                    Ok(conn)
                })
            });
        match result {
            Ok(conn) => {
                session.transaction = Some(crate::session::TxnHandle {
                    conn: Arc::new(tokio::sync::Mutex::new(conn)),
                    savepoints: Vec::new(),
                    isolation: iso,
                });
                // Mark the plugin executor so `narwhal.sql_run` refuses
                // to run while a transaction is open — a fresh pool
                // connection wouldn't see the uncommitted state and the
                // user would silently get wrong answers.
                self.plugin_state
                    .lock()
                    .expect("plugin_state poisoned")
                    .in_transaction = true;
                self.status_message = match iso {
                    Some(level) => format!("transaction started ({})", isolation_label(level)),
                    None => "transaction started".into(),
                };
            }
            Err(error) => {
                self.status_message = format!("begin failed: {error}");
            }
        }
    }

    fn commit_transaction(&mut self) {
        self.end_transaction(true);
    }

    fn rollback_transaction(&mut self) {
        self.end_transaction(false);
    }

    /// Finish an open transaction. `commit == true` invokes `commit()`,
    /// otherwise `rollback()`. Either way the pinned connection is
    /// returned to the pool.
    fn end_transaction(&mut self, commit: bool) {
        if self.running {
            self.status_message = "a query is already running".into();
            return;
        }
        let Some(session) = self.session.as_mut() else {
            self.status_message = "no active connection".into();
            return;
        };
        let Some(txn) = session.transaction.take() else {
            self.status_message = "no open transaction".into();
            return;
        };
        let conn_arc = txn.conn;
        let outcome = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                // Reclaim the pinned connection. If callers held extra Arc
                // clones we error out rather than silently leak state.
                let mutex = match Arc::try_unwrap(conn_arc) {
                    Ok(m) => m,
                    Err(arc) => {
                        // A run worker is still holding the lock; wait for
                        // it to drop and try again. In practice
                        // dispatch_batch only locks while running.
                        drop(arc);
                        return Err(narwhal_core::Error::Connection(
                            "transaction connection still in use".into(),
                        ));
                    }
                };
                let mut conn = mutex.into_inner();
                if commit {
                    conn.commit().await?;
                } else {
                    conn.rollback().await?;
                }
                Ok::<(), narwhal_core::Error>(())
            })
        });
        // Whatever happened to the underlying transaction, the host-side
        // pinned-connection state is gone (we already `take()`d the txn
        // out of `session.transaction` above). Clear the plugin-side
        // flag so subsequent `sql_run` calls work again.
        self.plugin_state
            .lock()
            .expect("plugin_state poisoned")
            .in_transaction = false;
        match outcome {
            Ok(()) => {
                self.status_message = if commit {
                    "transaction committed".into()
                } else {
                    "transaction rolled back".into()
                };
            }
            Err(error) => {
                self.status_message = if commit {
                    format!("commit failed: {error}")
                } else {
                    format!("rollback failed: {error}")
                };
            }
        }
    }

    fn savepoint(&mut self, name: &str) {
        self.with_txn_conn(
            |conn, name| Box::pin(async move { conn.savepoint(name).await }),
            name,
            |session, name| {
                if let Some(txn) = session.transaction.as_mut() {
                    txn.savepoints.push(name.to_owned());
                }
            },
            |name| format!("savepoint '{name}' established"),
            |name, error| format!("savepoint '{name}' failed: {error}"),
        );
    }

    fn release_savepoint(&mut self, name: &str) {
        self.with_txn_conn(
            |conn, name| Box::pin(async move { conn.release_savepoint(name).await }),
            name,
            |session, name| {
                if let Some(txn) = session.transaction.as_mut() {
                    if let Some(pos) = txn.savepoints.iter().position(|s| s == name) {
                        txn.savepoints.truncate(pos);
                    }
                }
            },
            |name| format!("savepoint '{name}' released"),
            |name, error| format!("release '{name}' failed: {error}"),
        );
    }

    fn rollback_to_savepoint(&mut self, name: &str) {
        self.with_txn_conn(
            |conn, name| Box::pin(async move { conn.rollback_to_savepoint(name).await }),
            name,
            |session, name| {
                if let Some(txn) = session.transaction.as_mut() {
                    if let Some(pos) = txn.savepoints.iter().position(|s| s == name) {
                        // Everything after the savepoint is unwound.
                        txn.savepoints.truncate(pos + 1);
                    }
                }
            },
            |name| format!("rolled back to savepoint '{name}'"),
            |name, error| format!("rollback-to '{name}' failed: {error}"),
        );
    }

    /// Lock the pinned transaction connection and run `op` on it. Used by
    /// `:savepoint`, `:release` and `:rollback-to` which all need the same
    /// guarding boilerplate. Statement execution (`:run`/`:run-all`) goes
    /// through `dispatch_batch` instead since that path streams updates
    /// back through `RunUpdate`.
    fn with_txn_conn<F, S, OkF, ErrF>(
        &mut self,
        op: F,
        name: &str,
        on_success: S,
        ok_msg: OkF,
        err_msg: ErrF,
    ) where
        F: for<'a> FnOnce(
            &'a mut dyn narwhal_core::Connection,
            &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = narwhal_core::Result<()>> + Send + 'a>,
        >,
        S: FnOnce(&mut Session, &str),
        OkF: FnOnce(&str) -> String,
        ErrF: FnOnce(&str, &narwhal_core::Error) -> String,
    {
        if self.running {
            self.status_message = "a query is already running".into();
            return;
        }
        let Some(session) = self.session.as_mut() else {
            self.status_message = "no active connection".into();
            return;
        };
        let Some(txn) = session.transaction.as_ref() else {
            self.status_message = "no open transaction".into();
            return;
        };
        let conn_arc = txn.conn.clone();
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let mut guard = conn_arc.lock().await;
                op(&mut **guard, name).await
            })
        });
        match result {
            Ok(()) => {
                on_success(session, name);
                self.status_message = ok_msg(name);
            }
            Err(error) => {
                self.status_message = err_msg(name, &error);
            }
        }
    }

    fn remove_connection(&mut self, name: &str) {
        let Some(pos) = self
            .connections
            .connections
            .iter()
            .position(|c| c.name == name)
        else {
            self.status_message = format!("remove: no connection named '{name}'");
            return;
        };
        let removed = self.connections.connections.remove(pos);
        if let Some(path) = self.connections_path.as_ref() {
            if let Err(error) = self.connections.save(path) {
                // Restore in-memory state so we don't drift from disk.
                self.connections.connections.insert(pos, removed);
                self.status_message = format!("remove failed: {error}");
                return;
            }
        }
        if let Err(error) = self.credentials.delete(removed.id) {
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
        self.status_message = format!("removed connection '{name}'");
    }

    fn forget_password(&mut self, name: &str) {
        let Some(config) = self.connections.connections.iter().find(|c| c.name == name) else {
            self.status_message = format!("forget: no connection named '{name}'");
            return;
        };
        match self.credentials.delete(config.id) {
            Ok(()) => {
                self.status_message = format!("forgot password for '{name}'");
            }
            Err(error) => {
                self.status_message = format!("forget failed: {error}");
            }
        }
    }

    fn cancel_wizard(&mut self) {
        if self.wizard.take().is_some() {
            self.wizard_error = None;
            self.status_message = "add cancelled".into();
        }
    }

    fn commit_wizard(&mut self) {
        let Some(wizard) = self.wizard.as_ref() else {
            return;
        };
        match wizard.build() {
            Err(error) => {
                self.wizard_error = Some(error);
            }
            Ok(built) => {
                if self
                    .connections
                    .connections
                    .iter()
                    .any(|c| c.name == built.config.name)
                {
                    self.wizard_error = Some(format!(
                        "a connection named '{}' already exists",
                        built.config.name
                    ));
                    return;
                }
                let connection_id = built.config.id;
                let secret = built.password.clone();
                self.connections.connections.push(built.config.clone());
                if let Some(path) = self.connections_path.as_ref() {
                    if let Err(error) = self.connections.save(path) {
                        self.wizard_error = Some(format!("could not save: {error}"));
                        // Roll back the in-memory entry so the on-disk file
                        // remains the source of truth.
                        self.connections.connections.pop();
                        return;
                    }
                }
                if let Some(secret) = secret {
                    if let Err(error) = self.credentials.set(connection_id, &secret) {
                        // The connection is still saved; just warn the user
                        // that the secret didn't make it to the keyring.
                        self.wizard_error = Some(format!(
                            "saved, but storing the password in the keyring failed: {error}"
                        ));
                    }
                }
                self.wizard = None;
                self.wizard_error = None;
                self.rebuild_sidebar();
                let name = built.config.name.clone();
                self.status_message = format!("connection '{name}' saved");
                // Pre-select the new connection in the sidebar.
                if let Some(idx) = self.sidebar_items.iter().position(|i| match i {
                    SidebarItem::Connection { name: n, .. } => n == &name,
                    _ => false,
                }) {
                    self.sidebar_index = idx;
                }
            }
        }
    }

    fn handle_wizard_key(&mut self, key: KeyEvent) {
        let Some(wizard) = self.wizard.as_mut() else {
            return;
        };
        match key.code {
            CtKey::Esc => self.cancel_wizard(),
            CtKey::Tab | CtKey::Down => wizard.next_focus(),
            CtKey::BackTab | CtKey::Up => wizard.prev_focus(),
            CtKey::Left if wizard.focused == 0 => wizard.cycle_driver(-1),
            CtKey::Right if wizard.focused == 0 => wizard.cycle_driver(1),
            CtKey::Enter => self.commit_wizard(),
            CtKey::Backspace => wizard.pop_char(),
            CtKey::Char(c) => {
                if wizard.focused == 0 {
                    // Allow first-letter shortcuts on the driver row.
                    if let Some(idx) = DRIVERS.iter().position(|d| d.starts_with(c)) {
                        wizard.driver_index = idx;
                        wizard.cycle_driver(0);
                    }
                } else {
                    wizard.push_char(c);
                }
            }
            _ => {}
        }
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
                let base = if failures == 0 {
                    format!("done · {successes} statement(s)")
                } else {
                    format!("done · {successes} ok · {failures} failed")
                };
                self.status_message = match self.plugin_warning.take() {
                    Some(warning) => format!("{base} · {warning}"),
                    None => base,
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

    /// Run every loaded plugin's `transform_result` hook over the rows
    /// just produced by a row-returning statement. EXPLAIN output and
    /// `Affected`-only results skip this path because mutating them
    /// would defeat the purpose of those views.
    ///
    /// Failures from a transform are reported to the status bar but the
    /// original (untransformed) rows are still surfaced — a misbehaving
    /// plugin shouldn't be able to hide query data from the user.
    fn apply_plugin_transforms(
        &mut self,
        columns: Vec<ColumnHeader>,
        rows: Vec<Row>,
        elapsed_ms: u64,
    ) -> (Vec<ColumnHeader>, Vec<Row>) {
        if self.plugins.plugins().is_empty() {
            return (columns, rows);
        }
        let plugins = Arc::clone(&self.plugins);
        let mut qr = narwhal_core::QueryResult {
            columns,
            rows,
            rows_affected: None,
            elapsed_ms,
        };
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { plugins.transform_result(&mut qr).await })
        });
        if let Err(errors) = result {
            // The registry already ran every plugin's transform regardless
            // of intermediate failures (see PluginRegistry::transform_result),
            // so `qr` reflects whatever each transform managed in place.
            // Surface every failure at once — the AllDone status message
            // would otherwise clobber it.
            self.plugin_warning = Some(format!("plugin transform failed: {errors}"));
        }
        (qr.columns, qr.rows)
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
            // Take the pending row source (set by preview_sidebar_selection)
            // and attach it to the result so cell edits can target the
            // originating table.
            let source = self.tabs[self.active_tab].pending_source.take();
            let (columns, rows) = self.apply_plugin_transforms(columns, rows, elapsed_ms);
            self.tabs[self.active_tab].result = ResultState::Rows {
                columns,
                rows,
                elapsed_ms,
                streamed,
                index,
                total,
                source,
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

fn display_from_state<'a>(
    state: &'a ResultState,
    search: Option<&'a SearchHighlight<'a>>,
) -> ResultDisplay<'a> {
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
            source: _,
        } => ResultDisplay::Rows {
            columns,
            rows,
            elapsed_ms: *elapsed_ms,
            streamed: *streamed,
            index: *index,
            total: *total,
            search,
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

fn map_isolation(arg: IsolationArg) -> IsolationLevel {
    match arg {
        IsolationArg::ReadUncommitted => IsolationLevel::ReadUncommitted,
        IsolationArg::ReadCommitted => IsolationLevel::ReadCommitted,
        IsolationArg::RepeatableRead => IsolationLevel::RepeatableRead,
        IsolationArg::Serializable => IsolationLevel::Serializable,
    }
}

fn isolation_label(level: IsolationLevel) -> &'static str {
    match level {
        IsolationLevel::ReadUncommitted => "read-uncommitted",
        IsolationLevel::ReadCommitted => "read-committed",
        IsolationLevel::RepeatableRead => "repeatable-read",
        IsolationLevel::Serializable => "serializable",
    }
}

/// Shared state read by every plugin SQL executor on every
/// `narwhal.sql_run` call. Owned by [`AppCore`] inside an
/// `Arc<std::sync::Mutex<_>>` so:
///
/// * opening/closing a session can retarget plugin SQL transparently
///   without rebuilding plugin objects;
/// * `:begin`/`:commit`/`:rollback` can flip the in-transaction flag so
///   the executor refuses to run during a pinned transaction (a fresh
///   pool connection wouldn't see uncommitted state — silent
///   correctness bug otherwise);
/// * the plain `std::sync::Mutex` is fine because every access is short
///   (clone the pool out, drop the guard) and never spans an `.await`.
#[derive(Default)]
pub(crate) struct PluginConnectionState {
    pub(crate) pool: Option<narwhal_pool::Pool>,
    pub(crate) in_transaction: bool,
}

/// SQL executor injected into every Lua plugin loaded by AppCore.
///
/// Reads [`PluginConnectionState`] on every call so the script always
/// targets the *currently active* connection. Refuses to run while a
/// `:begin` transaction is open — see the doc-comment on
/// [`PluginConnectionState`] for why.
///
/// ### Memory footprint
///
/// `narwhal.sql_run` materialises the whole result set in memory before
/// returning to Lua. Scripts that query unbounded tables can OOM the
/// process; recommend `LIMIT` in the user-facing docs. Streaming
/// support is a future addition.
struct AppPluginExecutor {
    state: Arc<std::sync::Mutex<PluginConnectionState>>,
}

#[async_trait::async_trait]
impl SqlExecutor for AppPluginExecutor {
    async fn run(&self, sql: &str) -> PluginResult<narwhal_core::QueryResult> {
        // Grab a snapshot of the state and drop the guard *before* we
        // touch any async API.
        let (pool, in_tx) = {
            let guard = self
                .state
                .lock()
                .map_err(|e| PluginError::Runtime(format!("plugin state poisoned: {e}")))?;
            (guard.pool.clone(), guard.in_transaction)
        };
        if in_tx {
            return Err(PluginError::Runtime(
                "narwhal.sql_run is unavailable while a :begin transaction is open".into(),
            ));
        }
        let pool = pool.ok_or_else(|| PluginError::Runtime("no active connection".into()))?;
        let mut conn = pool
            .acquire()
            .await
            .map_err(|e| PluginError::Runtime(format!("could not acquire connection: {e}")))?;
        conn.execute(sql, &[])
            .await
            .map_err(|e| PluginError::Runtime(format!("execute: {e}")))
    }
}

/// Split a `:`-line command's raw text into `(head, rest)` where `head`
/// is the first whitespace-delimited token and `rest` is everything after
/// it (with leading whitespace stripped). Mirrors the parser's tokeniser
/// but stays available for the plugin dispatch path which receives the
/// already-rejected `Command::Unknown` payload.
fn split_head_arg(text: &str) -> (&str, &str) {
    let trimmed = text.trim_start();
    match trimmed.find(char::is_whitespace) {
        Some(idx) => (&trimmed[..idx], trimmed[idx..].trim_start()),
        None => (trimmed, ""),
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
