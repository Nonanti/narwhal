//! Headless application state.
//!
//! [`AppCore`] owns every piece of state the UI depends on but contains no
//! terminal-IO logic. The render path takes a [`ratatui::Frame`] from the
//! caller, and key events come in as parsed crossterm [`KeyEvent`]s, so the
//! core is fully usable with `ratatui::backend::TestBackend` in tests.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode as CtKey, KeyEvent, KeyModifiers};
use narwhal_config::{ConnectionsFile, CredentialStore, InMemoryStore};
use narwhal_core::{
    Column, ColumnHeader, ConnectionConfig, IsolationLevel, Row, TableKind, TableSchema,
};
use narwhal_history::{HistoryEntry, Journal};
use narwhal_tui::{
    render_help_modal, render_history_modal, render_root, render_row_detail, render_snippets_modal,
    render_wizard, translate_key_event, CellEditView, CellPopup, CompletionItemView,
    CompletionPopupView, EditorBuffer, EditorSearchHighlight, ExplainPlanLine, HistoryModalState,
    HistoryRow, LayoutRegions, Pane, ResultDisplay, ResultView, RootLayout, RowDetailView,
    SearchHighlight, SidebarRow, SidebarRowKind, SidebarView, SnippetsModalState, SortDir,
    StatusBarView, Theme, WizardFieldView, WizardView,
};
use narwhal_vim::{Action, Mode, SearchDirection, Vim};
use ratatui::layout::Rect;
use ratatui::Frame;
use tokio::sync::{mpsc, Mutex};
use tracing::debug;
use uuid::Uuid;

use crate::clipboard::{Clipboard, InMemoryClipboard};
use crate::commands::{parse, Command, DumpTarget, IsolationArg};
use crate::completion::{detect_context, gather as gather_completions, Completion, CompletionKind};
use crate::ddl::{build_dump, build_table_ddl};
use crate::explain::{parse as parse_plan, wrap_explain};
use crate::export::{export_rows, ExportFormat};
use crate::registry::DriverRegistry;
use crate::run::{spawn_run, ActiveCancel, RunContext, RunMode, RunRequest, RunTarget, RunUpdate};
use crate::session::Session;
use crate::snippets::SnippetStore;
use crate::wizard::{ConnectionWizard, DRIVERS};
use narwhal_plugin::{
    CommandContext as PluginCommandContext, CommandOutcome as PluginCommandOutcome, Plugin,
    PluginError, PluginRegistry, PluginResult, SqlExecutor,
};
use narwhal_plugin_lua::LuaPlugin;

const RUN_CHANNEL_CAPACITY: usize = 128;

/// Three-slot (plus optional fourth) status bar.
///
/// - **Left** (rendered by the TUI): mode (NOR/INS/CMD) + focused pane.
/// - **Center**: connection name + driver — set once on `:open`,
///   cleared on `:close`; sticky across transient messages.
/// - **Right**: last transient message — the field every other
///   piece of code writes through.
/// - **Optional fourth**: open transaction's isolation level,
///   rendered between center and right when present.
#[derive(Debug, Default, Clone)]
pub struct StatusBar {
    /// Center slot — set once on connect, cleared on disconnect.
    pub connection: Option<String>,
    /// Right slot — last transient message.
    pub message: String,
    /// Optional fourth slot — open transaction's isolation level.
    pub transaction: Option<String>,
}

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
        /// Moment the stream task was spawned. Used to compute elapsed
        /// time in the streaming title bar.
        started_at: Instant,
        /// Instant of the last redraw triggered by a chunk. Throttles
        /// renders to ≤10 Hz so a fast-arriving stream does not drown
        /// the redraw loop.
        last_render: Instant,
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
        /// Best-effort table name extracted from the SQL (single-table
        /// `SELECT * FROM x`). Used by `:export insert` to generate
        /// INSERT statements. `None` for multi-table queries, computed
        /// expressions, etc.
        source_table: Option<crate::export::QualifiedName>,
    },
    Explain {
        lines: Vec<ExplainPlanLine>,
        planning_time_ms: Option<f64>,
        execution_time_ms: Option<f64>,
    },
    TableDetail {
        schema: TableSchema,
    },
    /// Stream was cancelled by the user (F4 / Ctrl-C). Shows
    /// how many rows were received before cancellation.
    Cancelled {
        rows_so_far: usize,
        elapsed_ms: u64,
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

/// Editor search state, separate from the result pane search.
/// Per-tab so each editor pane carries its own needle and highlight state.
#[derive(Debug, Clone, Default)]
pub struct EditorSearchState {
    /// The literal substring needle.
    pub needle: String,
    /// Direction of the search that opened the prompt.
    pub direction: SearchDirection,
    /// Whether the search prompt is currently open for editing.
    pub prompt_open: bool,
    /// Cursor position saved when `/` or `?` was pressed, restored on Esc.
    pub saved_cursor: Option<(usize, usize)>,
    /// All match positions as `(line_idx, byte_col)` pairs.
    pub matches: Vec<(usize, usize)>,
    /// Index into `matches` for the current match (where the cursor sits).
    pub current: Option<usize>,
    /// Whether matches are highlighted in the editor.
    pub highlight: bool,
}

/// In-flight row detail modal. `R` (or Shift+Enter) opens it from the
/// result pane; Esc / `R` / Shift+Enter dismisses it.
#[derive(Debug, Clone)]
pub struct RowDetailState {
    /// Original row index in the full result set.
    pub row_index: usize,
    pub columns: Vec<ColumnHeader>,
    pub values: Vec<narwhal_core::Value>,
    pub selected_column: usize,
    pub scroll_offset: u16,
}

/// Bundle of per-statement results produced by a multi-statement batch.
/// When the dispatch pipeline produces N result sets the user can cycle
/// through them with `]r` / `[r` (or Ctrl-PgDown / Ctrl-PgUp); the
/// active tab's state — scroll, sort, filter — is preserved across
/// switches.
///
/// The common case (single result) has `states.len() == 1` and the
/// strip is not rendered; behaviour is byte-for-byte identical to the
/// pre-bundle world.
///
/// `states` and `views` are kept in parallel arrays so the render path
/// can borrow from `states` immutably while mutating `views` — they
/// live in separate allocations, satisfying the borrow checker.
#[derive(Debug)]
pub struct ResultBundle {
    /// One `ResultState` per statement in the batch.
    pub states: Vec<ResultState>,
    /// One `ResultView` per statement (scroll, sort, filter, etc.).
    pub views: Vec<ResultView>,
    /// Index of the currently-visible result.
    pub active: usize,
}

impl ResultBundle {
    /// Construct a single-result bundle. No tab strip renders.
    pub fn single(state: ResultState, view: ResultView) -> Self {
        Self {
            states: vec![state],
            views: vec![view],
            active: 0,
        }
    }

    /// Construct a multi-result bundle from parallel vectors.
    /// `active` starts at 0.
    pub fn multi(states: Vec<ResultState>, views: Vec<ResultView>) -> Self {
        assert!(
            !states.is_empty(),
            "ResultBundle must contain at least one entry"
        );
        assert_eq!(
            states.len(),
            views.len(),
            "states and views must have the same length"
        );
        Self {
            states,
            views,
            active: 0,
        }
    }

    /// Whether the bundle contains more than one result (and thus a
    /// tab strip should be rendered).
    pub fn is_multi(&self) -> bool {
        self.states.len() > 1
    }

    /// Total number of results in the bundle.
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// Whether the bundle has no results. (Always false in practice
    /// since we guarantee at least one entry, but required by clippy.)
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Read-only access to the active result view.
    pub fn active(&self) -> &ResultView {
        &self.views[self.active]
    }

    /// Mutable access to the active result view.
    pub fn active_mut(&mut self) -> &mut ResultView {
        &mut self.views[self.active]
    }

    /// Read-only access to the active result state.
    pub fn active_state(&self) -> &ResultState {
        &self.states[self.active]
    }

    /// Mutable access to the active result state.
    pub fn active_state_mut(&mut self) -> &mut ResultState {
        &mut self.states[self.active]
    }

    /// Advance to the next result (wraps).
    pub fn next(&mut self) {
        if self.states.len() > 1 {
            self.active = (self.active + 1) % self.states.len();
        }
    }

    /// Go to the previous result (wraps).
    pub fn prev(&mut self) {
        if self.states.len() > 1 {
            self.active = self.active.checked_sub(1).unwrap_or(self.states.len() - 1);
        }
    }

    /// Reset every `ResultView` in the bundle.
    pub fn reset_all(&mut self) {
        for view in &mut self.views {
            view.reset();
        }
    }
}

impl Default for ResultBundle {
    fn default() -> Self {
        Self::single(ResultState::Empty, ResultView::new())
    }
}

/// One editor tab: a buffer + the most recent result it produced.
pub struct Tab {
    pub name: String,
    pub editor: EditorBuffer,
    pub results: ResultBundle,
    pub search: Option<ResultSearch>,
    pub editing: Option<CellEdit>,
    pub completion: Option<CompletionState>,
    /// Per-tab editor search state (separate from result pane search).
    pub editor_search: EditorSearchState,
    /// Page size used by the next sidebar preview. Stored per-tab so a
    /// user paging through one table doesn't disturb another tab.
    pub page_size: usize,
    /// Pending row source to attach to the next `Rows` result. Populated
    /// by `preview_sidebar_selection` and consumed in `finish_run`.
    pub pending_source: Option<RowSource>,
    /// When `Some`, the row detail modal is open on the result pane.
    /// Sits at the same layer as the cell popup; only one of them
    /// should be open at a time.
    pub row_detail: Option<RowDetailState>,
}

impl Tab {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            editor: EditorBuffer::new(),
            results: ResultBundle::default(),
            search: None,
            editing: None,
            completion: None,
            editor_search: EditorSearchState::default(),
            page_size: 100,
            pending_source: None,
            row_detail: None,
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

/// State for the Ctrl+R history modal.
#[derive(Debug, Clone)]
pub struct HistoryState {
    /// All entries loaded from the journal.
    pub entries: Vec<HistoryEntry>,
    /// Current filter string (case-insensitive substring).
    pub filter: String,
    /// Index into the filtered subset.
    pub selected: usize,
}

impl HistoryState {
    /// Return the subset of entries matching the current filter.
    pub fn visible_entries(&self) -> Vec<&HistoryEntry> {
        if self.filter.is_empty() {
            self.entries.iter().collect()
        } else {
            let needle = self.filter.to_lowercase();
            self.entries
                .iter()
                .filter(|e| e.sql.to_lowercase().contains(&needle))
                .collect()
        }
    }
}

/// State for the `:snippets` modal.
#[derive(Debug, Clone)]
pub struct SnippetsModal {
    /// Sorted list of snippet names.
    pub entries: Vec<String>,
    /// Index of the currently selected entry.
    pub selected: usize,
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
    history_journal: Option<Arc<Journal>>,
    /// When `Some`, the Ctrl+R history modal is open.
    history_state: Option<HistoryState>,
    /// Persistent snippet store.
    snippet_store: SnippetStore,
    /// When `Some`, the `:snippets` modal is open.
    snippets_modal: Option<SnippetsModal>,
    session: Option<Session>,
    tabs: Vec<Tab>,
    active_tab: usize,
    next_tab_id: usize,
    vim: Vim,
    theme: Theme,
    focus: Pane,
    sidebar_items: Vec<SidebarItem>,
    sidebar_index: usize,
    status: StatusBar,
    /// One-shot warning carried over from a plugin (transform or command
    /// hook) so that the final 'done · N statement(s)' AllDone message
    /// doesn't overwrite it silently. Cleared after it bubbles up.
    plugin_warning: Option<String>,
    running: bool,
    /// Tab index that owns the in-flight run. Set to `Some(active_tab)`
    /// when `dispatch_batch` fires; cleared to `None` on `AllDone`.
    /// All `handle_run_update` / `finalize_statement` mutations target
    /// this tab, not `active_tab`, so a mid-run tab switch cannot
    /// corrupt a different tab's results.  (Bug K1-A fix.)
    run_tab: Option<usize>,
    cancel_slot: ActiveCancel,
    should_quit: bool,
    wizard: Option<ConnectionWizard>,
    wizard_error: Option<String>,
    help_open: bool,
    /// Pending leader key for result-tab cycling. `]` followed by
    /// `r` cycles forward; `[` followed by `r` cycles backward.
    pending_result_leader: Option<char>,
    /// Collects per-statement results during a multi-statement batch.
    /// Populated by `finalize_statement`; consumed and turned into a
    /// `ResultBundle` by the `AllDone` handler.
    pending_result_entries_states: Vec<ResultState>,
    pending_result_entries_views: Vec<ResultView>,
    last_layout: LayoutRegions,
    run_tx: mpsc::Sender<RunUpdate>,
    pub(crate) run_rx: mpsc::Receiver<RunUpdate>,
    /// Handle to the in-flight debounced schema refresh task.
    /// Aborting it cancels the pending timer; a new task replaces it
    /// on every `schedule_schema_refresh` call.
    refresh_task: Option<tokio::task::AbortHandle>,
    /// Shared flag set by `schedule_schema_refresh` and consumed by
    /// the debounce timer task to know whether a refresh is still
    /// pending.
    refresh_pending: Arc<AtomicBool>,
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
            history_journal: history,
            history_state: None,
            snippet_store: SnippetStore::new(SnippetStore::default_root()),
            snippets_modal: None,
            session: None,
            tabs: vec![Tab::new("untitled")],
            active_tab: 0,
            next_tab_id: 2,
            vim: Vim::new(),
            theme: Theme::default(),
            focus: Pane::Editor,
            sidebar_items: Vec::new(),
            sidebar_index: 0,
            status: StatusBar {
                message: "ready".into(),
                ..Default::default()
            },
            plugin_warning: None,
            running: false,
            run_tab: None,
            cancel_slot: Arc::new(Mutex::new(None)),
            should_quit: false,
            wizard: None,
            wizard_error: None,
            help_open: false,
            pending_result_leader: None,
            pending_result_entries_states: Vec::new(),
            pending_result_entries_views: Vec::new(),
            last_layout: LayoutRegions::default(),
            run_tx,
            run_rx,
            refresh_task: None,
            refresh_pending: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Inform the core where to persist new connections produced by the
    /// `:add` wizard. Called by [`crate::app::App::new`].
    pub fn set_connections_path(&mut self, path: std::path::PathBuf) {
        self.connections_path = Some(path);
    }

    /// Override the snippet store root directory. Used by tests to
    /// avoid polluting the user's real config.
    #[doc(hidden)]
    pub fn set_snippet_store_root(&mut self, root: std::path::PathBuf) {
        self.snippet_store = SnippetStore::new(root);
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
        &self.status.message
    }

    /// Read-only accessor for the full [`StatusBar`] struct.
    pub fn status_bar(&self) -> &StatusBar {
        &self.status
    }

    pub fn result(&self) -> &ResultState {
        self.tab().results.active_state()
    }

    /// Test helper: is the completion popup currently open on the
    /// active tab? Used by integration tests that drive the editor and
    /// want to assert auto-trigger fired without depending on a
    /// specific status-bar message.
    #[doc(hidden)]
    pub fn editor_completion_is_open(&self) -> bool {
        self.tabs[self.active_tab].completion.is_some()
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

    /// Borrow the active tab.
    ///
    /// Indexing is sound because two invariants are upheld at every
    /// `&mut self` entry point:
    /// - `self.tabs` always contains at least one element. `close_tab`
    ///   early-returns when `tabs.len() == 1`.
    /// - `self.active_tab < self.tabs.len()`. `close_tab` clamps after
    ///   removal; `cycle_tab` uses `rem_euclid(len)`.
    ///
    /// See `active_tab_invariant_holds_after_close` in the test suite.
    fn tab(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    /// Mutable counterpart to [`Self::tab`]. Same invariants apply.
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

    /// Tab index that owns the in-flight run.  Falls back to
    /// `active_tab` when no run is in progress (defensive default).
    fn run_tab_index(&self) -> usize {
        self.run_tab.unwrap_or(self.active_tab)
    }

    /// Read-only accessor for the most recent layout regions computed
    /// during render. Used by tests to determine where to click.
    #[doc(hidden)]
    pub fn last_layout(&self) -> &narwhal_tui::LayoutRegions {
        &self.last_layout
    }

    pub fn mode(&self) -> Mode {
        self.vim.mode()
    }

    /// Read-only accessor for the vim command buffer. Used by tests to
    /// assert prompt Tab-completion results.
    #[doc(hidden)]
    pub fn command_buffer(&self) -> &str {
        self.vim.command_buffer()
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// Whether a debounced schema-refresh timer is currently pending.
    /// Useful in tests to verify that non-DDL statements don't schedule
    /// a refresh.
    #[doc(hidden)]
    pub fn refresh_task(&self) -> Option<&tokio::task::AbortHandle> {
        self.refresh_task.as_ref()
    }

    pub fn help_open(&self) -> bool {
        self.help_open
    }

    /// Whether the history modal is currently open.
    #[doc(hidden)]
    pub fn history_is_open(&self) -> bool {
        self.history_state.is_some()
    }

    /// Read-only accessor for the history modal state (for tests).
    #[doc(hidden)]
    pub fn history_state(&self) -> Option<&HistoryState> {
        self.history_state.as_ref()
    }

    /// Whether the row detail modal is currently open on the active tab.
    #[doc(hidden)]
    pub fn row_detail_is_open(&self) -> bool {
        self.tabs[self.active_tab].row_detail.is_some()
    }

    /// Whether the snippets modal is currently open.
    #[doc(hidden)]
    pub fn snippets_modal_is_open(&self) -> bool {
        self.snippets_modal.is_some()
    }

    /// Read-only accessor for the snippets modal state (for tests).
    #[doc(hidden)]
    pub fn snippets_modal(&self) -> Option<&SnippetsModal> {
        self.snippets_modal.as_ref()
    }

    /// Read-only accessor for the snippet store (for tests).
    #[doc(hidden)]
    pub fn snippet_store(&self) -> &SnippetStore {
        &self.snippet_store
    }

    /// Read-only accessor for the row detail modal state (for tests).
    #[doc(hidden)]
    pub fn row_detail_state(&self) -> Option<&RowDetailState> {
        self.tabs[self.active_tab].row_detail.as_ref()
    }

    /// Open the help modal. Primarily for tests; the UI path goes
    /// through `handle_key(F1)` or `handle_key(?)`.
    pub fn open_help(&mut self) {
        self.help_open = true;
    }

    fn toggle_help(&mut self) {
        self.help_open = !self.help_open;
    }

    // ----- history modal -----

    /// Open the Ctrl+R history modal. Reads the 200 most recent
    /// entries from the journal synchronously via
    /// `block_in_place + Handle::current().block_on`, the same
    /// sync→async bridge used elsewhere in AppCore.
    pub fn open_history(&mut self) {
        let entries = if let Some(j) = &self.history_journal {
            let j = Arc::clone(j);
            match tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async { j.recent(200) })
            }) {
                Ok(e) => e,
                Err(err) => {
                    self.status.message = format!("history read failed: {err}");
                    return;
                }
            }
        } else {
            self.status.message = "history disabled".into();
            return;
        };
        if entries.is_empty() {
            self.status.message = "no history entries".into();
            return;
        }
        self.history_state = Some(HistoryState {
            entries,
            filter: String::new(),
            selected: 0,
        });
        self.status.message =
            "history: type to filter · Enter insert · Shift-Enter run · Esc close".into();
    }

    fn close_history(&mut self) {
        self.history_state = None;
    }

    /// Handle key events while the history modal is open.
    fn handle_history_key(&mut self, key: KeyEvent) {
        let Some(state) = self.history_state.as_mut() else {
            return;
        };
        match key.code {
            CtKey::Esc => {
                self.close_history();
                self.status.message = "history closed".into();
            }
            CtKey::Up | CtKey::Char('k')
                if key.modifiers.is_empty() || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let visible = state.visible_entries();
                if !visible.is_empty() {
                    state.selected = (state.selected + visible.len() - 1) % visible.len();
                }
            }
            CtKey::Down | CtKey::Char('j')
                if key.modifiers.is_empty() || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let visible = state.visible_entries();
                if !visible.is_empty() {
                    state.selected = (state.selected + 1) % visible.len();
                }
            }
            CtKey::Enter => {
                let sql = {
                    let visible = state.visible_entries();
                    visible.get(state.selected).map(|e| e.sql.clone())
                };
                if let Some(sql) = sql {
                    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
                    self.close_history();
                    self.tabs[self.active_tab].editor.insert_str(&sql);
                    if shift {
                        self.dispatch_current_statement(RunMode::Execute);
                    } else {
                        self.status.message =
                            format!("inserted {} char(s) from history", sql.len());
                    }
                } else {
                    self.close_history();
                }
            }
            CtKey::Backspace => {
                state.filter.pop();
                state.selected = 0;
            }
            CtKey::Char(c) => {
                state.filter.push(c);
                state.selected = 0;
            }
            _ => {}
        }
    }

    // ----- snippets modal -----

    /// Open the `:snippets` modal. Reads the snippet list from the store.
    fn open_snippets_modal(&mut self) {
        match self.snippet_store.list() {
            Ok(entries) => {
                if entries.is_empty() {
                    self.status.message = "no saved snippets; use :save <name> first".into();
                    return;
                }
                self.snippets_modal = Some(SnippetsModal {
                    entries,
                    selected: 0,
                });
                self.status.message = "snippets: ↑↓/jk navigate · Enter load · Esc close".into();
            }
            Err(error) => {
                self.status.message = format!("snippets: could not list: {error}");
            }
        }
    }

    fn close_snippets_modal(&mut self) {
        self.snippets_modal = None;
    }

    /// Handle key events while the snippets modal is open.
    fn handle_snippets_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.snippets_modal.as_mut() else {
            return;
        };
        match key.code {
            CtKey::Esc => {
                self.close_snippets_modal();
                self.status.message = "snippets closed".into();
            }
            CtKey::Up | CtKey::Char('k')
                if (key.modifiers.is_empty() || key.modifiers.contains(KeyModifiers::CONTROL))
                    && !modal.entries.is_empty() =>
            {
                modal.selected = (modal.selected + modal.entries.len() - 1) % modal.entries.len();
            }
            CtKey::Down | CtKey::Char('j')
                if (key.modifiers.is_empty() || key.modifiers.contains(KeyModifiers::CONTROL))
                    && !modal.entries.is_empty() =>
            {
                modal.selected = (modal.selected + 1) % modal.entries.len();
            }
            CtKey::Enter => {
                let name = self
                    .snippets_modal
                    .as_ref()
                    .and_then(|m| m.entries.get(m.selected).cloned());
                self.close_snippets_modal();
                if let Some(name) = name {
                    self.load_snippet_by_name(&name);
                } else {
                    self.status.message = "snippets closed".into();
                }
            }
            _ => {}
        }
    }

    /// Load a snippet by name into a new editor tab.
    fn load_snippet_by_name(&mut self, name: &str) {
        match self.snippet_store.load(name) {
            Ok(sql) => {
                self.new_tab();
                self.tabs[self.active_tab].editor.insert_str(&sql);
                self.tabs[self.active_tab].name = name.to_owned();
                self.status.message = format!("loaded snippet '{name}' ({} char(s))", sql.len());
            }
            Err(error) => {
                self.status.message = format!("load failed: {error}");
            }
        }
    }

    /// Save the current editor buffer as a named snippet.
    fn save_snippet(&mut self, name: &str) {
        let sql = self.tabs[self.active_tab].editor.entire_text();
        if sql.trim().is_empty() {
            self.status.message = "editor is empty; nothing to save".into();
            return;
        }
        match self.snippet_store.save(name, &sql) {
            Ok(()) => {
                self.status.message = format!("saved snippet '{name}'");
            }
            Err(error) => {
                self.status.message = format!("save failed: {error}");
            }
        }
    }

    /// Remove a named snippet.
    fn remove_snippet(&mut self, name: &str) {
        match self.snippet_store.remove(name) {
            Ok(()) => {
                self.status.message = format!("removed snippet '{name}'");
            }
            Err(error) => {
                self.status.message = format!("rm-snippet failed: {error}");
            }
        }
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
        let editor_title = self.editor_title_with_tabs();

        let tab = &mut self.tabs[self.active_tab];
        let search_view = tab.search.as_ref().map(|s| SearchHighlight {
            matches: &s.matches,
            current: s.current,
        });
        // Extract result state and view via the active index to avoid
        // overlapping borrows on `tab.results`.
        let active_idx = tab.results.active;
        let result_display =
            display_from_state(&tab.results.states[active_idx], search_view.as_ref());
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
        let editor_search_view =
            if tab.editor_search.highlight && !tab.editor_search.needle.is_empty() {
                Some(EditorSearchHighlight {
                    matches: &tab.editor_search.matches,
                    needle_len: tab.editor_search.needle.len(),
                    current: tab.editor_search.current,
                })
            } else {
                None
            };
        let result_count = tab.results.len();
        let mut layout = RootLayout {
            mode: self.vim.mode(),
            focus: self.focus,
            status_bar: StatusBarView {
                connection: self.status.connection.as_deref(),
                message: &self.status.message,
                transaction: self.status.transaction.as_deref(),
            },
            running: self.running,
            theme: &self.theme,
            sidebar: sidebar_view,
            editor: &mut tab.editor,
            editor_title: &editor_title,
            result_view: &mut tab.results.views[active_idx],
            result: result_display,
            completion: completion_view,
            editor_search: editor_search_view,
            result_count,
            active_result: active_idx,
        };
        self.last_layout = render_root(frame, area, &mut layout);

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

        if self.help_open {
            render_help_modal(frame, area, &self.theme);
        }

        if let Some(state) = self.history_state.as_ref() {
            let visible_data: Vec<(String, String, String)> = state
                .visible_entries()
                .iter()
                .map(|e| {
                    let ts = e.timestamp.format("%Y-%m-%d %H:%M:%S").to_string();
                    let conn = e.connection_name.as_deref().unwrap_or("<local>").to_owned();
                    (ts, conn, e.sql.clone())
                })
                .collect();
            let modal_state = HistoryModalState {
                total: state.entries.len(),
                visible: visible_data
                    .iter()
                    .map(|(ts, conn, sql)| HistoryRow {
                        timestamp: ts.as_str(),
                        connection: conn.as_str(),
                        sql: sql.as_str(),
                    })
                    .collect(),
                filter: &state.filter,
                selected: state.selected,
            };
            render_history_modal(frame, area, &modal_state, &self.theme);
        }

        // Snippets modal.
        if let Some(modal) = self.snippets_modal.as_ref() {
            let modal_state = SnippetsModalState {
                entries: modal.entries.iter().map(String::as_str).collect(),
                selected: modal.selected,
            };
            render_snippets_modal(frame, area, &modal_state, &self.theme);
        }

        // Row detail modal — same layer as cell popup, rendered on
        // top of the result pane.
        if let Some(state) = self.tabs[self.active_tab].row_detail.as_ref() {
            let view = RowDetailView {
                columns: &state.columns,
                values: &state.values,
                selected_column: state.selected_column,
                scroll_offset: state.scroll_offset,
                row_index: state.row_index,
            };
            render_row_detail(frame, area, &view, &self.theme);
        }
    }

    // ----- input -----

    pub fn handle_key(&mut self, key: KeyEvent) {
        if self.wizard.is_some() {
            self.handle_wizard_key(key);
            return;
        }
        // When the help modal is open, it intercepts Esc / ? / F1 to
        // close and silently consumes every other key so the user
        // doesn't accidentally trigger an action behind the overlay.
        if self.help_open {
            match key.code {
                CtKey::Esc | CtKey::F(1) => {
                    self.help_open = false;
                }
                CtKey::Char('?') if key.modifiers.is_empty() => {
                    self.help_open = false;
                }
                _ => {
                    // consumed but no-op
                }
            }
            return;
        }
        // When the history modal is open, it intercepts all keys.
        if self.history_state.is_some() {
            self.handle_history_key(key);
            return;
        }
        // When the snippets modal is open, it intercepts all keys.
        if self.snippets_modal.is_some() {
            self.handle_snippets_key(key);
            return;
        }
        if self.handle_global_key(key) {
            return;
        }
        // Pending result-tab leader: `]` or `[` was pressed, waiting
        // for `r` to complete the sequence. Any other key cancels.
        if let Some(leader) = self.pending_result_leader.take() {
            if key.code == CtKey::Char('r') && key.modifiers.is_empty() {
                match leader {
                    ']' => self.cycle_result_tab(1),
                    '[' => self.cycle_result_tab(-1),
                    _ => {}
                }
            }
            return;
        }
        match self.focus {
            Pane::Editor => self.handle_editor_key(key),
            Pane::Sidebar => self.handle_sidebar_key(key),
            Pane::Results => self.handle_results_key(key),
        }
    }

    /// Route a crossterm [`MouseEvent`] through the same handlers the
    /// keyboard path uses. `LayoutRegions` from the most recent render
    /// provides the hit-test rects.
    pub fn handle_mouse(&mut self, event: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};

        let pos = (event.column, event.row);

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.handle_left_click(pos);
            }
            MouseEventKind::ScrollUp => {
                self.handle_scroll(pos, -1);
            }
            MouseEventKind::ScrollDown => {
                self.handle_scroll(pos, 1);
            }
            // Up, Moved, Drag are no-ops for now.
            _ => {}
        }
    }

    fn handle_left_click(&mut self, pos: (u16, u16)) {
        let layout = self.last_layout.clone();

        // Priority: completion popup > sidebar tables > result headers/rows > pane focus.
        for (rect, item_index) in &layout.completion_items {
            if rect.contains(ratatui::layout::Position::new(pos.0, pos.1)) {
                self.accept_completion_at(*item_index);
                return;
            }
        }

        for (rect, sidebar_idx) in &layout.sidebar_tables {
            if rect.contains(ratatui::layout::Position::new(pos.0, pos.1)) {
                self.click_sidebar_table(*sidebar_idx);
                return;
            }
        }

        for (rect, result_idx) in &layout.result_tabs {
            if rect.contains(ratatui::layout::Position::new(pos.0, pos.1)) {
                self.click_result_tab(*result_idx);
                return;
            }
        }

        for (rect, col_idx) in &layout.result_headers {
            if rect.contains(ratatui::layout::Position::new(pos.0, pos.1)) {
                // Sort cycle action: move column focus and toggle sort.
                self.tabs[self.active_tab].results.active_mut().column_index = *col_idx;
                self.toggle_sort();
                return;
            }
        }

        for (rect, row_idx) in &layout.result_rows {
            if rect.contains(ratatui::layout::Position::new(pos.0, pos.1)) {
                self.tabs[self.active_tab]
                    .results
                    .active_mut()
                    .state
                    .select(Some(*row_idx));
                self.focus = Pane::Results;
                self.status.message = format!("focus → {}", Pane::Results.label());
                return;
            }
        }

        // Fall through to pane focus change.
        if layout
            .sidebar
            .contains(ratatui::layout::Position::new(pos.0, pos.1))
        {
            self.focus = Pane::Sidebar;
            self.status.message = format!("focus → {}", Pane::Sidebar.label());
        } else if layout
            .editor
            .contains(ratatui::layout::Position::new(pos.0, pos.1))
        {
            self.focus = Pane::Editor;
            self.status.message = format!("focus → {}", Pane::Editor.label());
        } else if layout
            .results
            .contains(ratatui::layout::Position::new(pos.0, pos.1))
        {
            self.focus = Pane::Results;
            self.status.message = format!("focus → {}", Pane::Results.label());
        }
    }

    fn handle_scroll(&mut self, pos: (u16, u16), delta: i32) {
        let layout = &self.last_layout;

        if layout
            .results
            .contains(ratatui::layout::Position::new(pos.0, pos.1))
        {
            let row_count = match self.tabs[self.active_tab].results.active_state() {
                ResultState::Rows { rows, .. } | ResultState::Running { rows, .. } => rows.len(),
                _ => return,
            };
            if delta > 0 {
                self.tabs[self.active_tab]
                    .results
                    .active_mut()
                    .move_down(row_count);
            } else {
                self.tabs[self.active_tab].results.active_mut().move_up();
            }
        } else if layout
            .editor
            .contains(ratatui::layout::Position::new(pos.0, pos.1))
        {
            // Editor scroll: move cursor line offset without changing column.
            let height = layout.editor.height.saturating_sub(2) as usize; // subtract borders
            if height == 0 {
                return;
            }
            let buf = &mut self.tabs[self.active_tab].editor;
            if delta > 0 {
                // Scroll down: move cursor down
                buf.apply_motion(narwhal_vim::Motion::Down, 1);
                buf.ensure_visible(height);
            } else {
                buf.apply_motion(narwhal_vim::Motion::Up, 1);
                buf.ensure_visible(height);
            }
        }
    }

    /// Accept the completion item at `index`, replicating the Tab/Enter
    /// path from `handle_completion_key`.
    fn accept_completion_at(&mut self, index: usize) {
        let Some(state) = self.tabs[self.active_tab].completion.as_mut() else {
            return;
        };
        if index >= state.items.len() {
            return;
        }
        state.selected = index;
        let choice = state.items[index].text.clone();
        self.tabs[self.active_tab]
            .editor
            .replace_current_word_with(&choice);
        self.tabs[self.active_tab].completion = None;
        self.status.message = format!("completed: {choice}");
    }

    /// Click on a sidebar table row: navigate the sidebar to that index
    /// and inject a preview query.
    fn click_sidebar_table(&mut self, sidebar_idx: usize) {
        let Some(item) = self.sidebar_items.get(sidebar_idx).cloned() else {
            return;
        };
        let SidebarItem::Table { schema, name, .. } = item else {
            return;
        };
        self.sidebar_index = sidebar_idx;
        self.inject_table_preview(&schema, &name);
    }

    /// Click on a result tab: switch to that result index.
    fn click_result_tab(&mut self, result_idx: usize) {
        let bundle = &mut self.tabs[self.active_tab].results;
        if result_idx < bundle.len() && bundle.is_multi() {
            bundle.active = result_idx;
            let total = bundle.len();
            self.status.message = format!("result {} of {total}", result_idx + 1);
        }
    }

    /// Inject a `SELECT * FROM <schema>.<table> LIMIT 100;` into the
    /// editor and dispatch it. This mirrors the keyboard-driven
    /// `preview_sidebar_selection` but injects the SQL into the editor
    /// so the user can see what ran.
    fn inject_table_preview(&mut self, schema: &str, table: &str) {
        let dialect = self
            .session
            .as_ref()
            .map(|s| s.dialect())
            .unwrap_or(narwhal_sql::Dialect::Generic);
        let sql =
            crate::ddl::preview_query(schema, table, self.tabs[self.active_tab].page_size, dialect);
        self.tabs[self.active_tab].editor.clear();
        self.tabs[self.active_tab].editor.insert_str(&sql);
        self.dispatch_current_statement(RunMode::Execute);
    }

    fn handle_global_key(&mut self, key: KeyEvent) -> bool {
        // Terminal-agnostic function keys first. Most terminal emulators
        // forward F-keys and Alt-Enter as distinct events, while Ctrl +
        // punctuation (Ctrl-;, Ctrl-/) is frequently swallowed by the
        // VT100-style key encoding before it ever reaches the program.
        match key.code {
            CtKey::F(1) => {
                self.toggle_help();
                return true;
            }
            CtKey::F(5) => {
                self.dispatch_current_statement(RunMode::Execute);
                return true;
            }
            CtKey::F(6) => {
                self.dispatch_all_statements(RunMode::Execute);
                return true;
            }
            CtKey::F(7) => {
                self.dispatch_current_statement(RunMode::Stream);
                return true;
            }
            CtKey::F(4) if self.running => {
                self.spawn_cancel();
                return true;
            }
            CtKey::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.dispatch_current_statement(RunMode::Execute);
                return true;
            }
            _ => {}
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                CtKey::Char('w') => {
                    self.focus = self.focus.cycle();
                    self.status.message = format!("focus → {}", self.focus.label());
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
                CtKey::Char(' ')
                    if self.focus == Pane::Editor && self.vim.mode() == Mode::Insert =>
                {
                    // Ctrl-Space is the IDE-standard completion trigger
                    // and survives most terminal key-encoding layers.
                    // Only fires when the editor pane is focused and
                    // we're in insert mode — in normal mode it would
                    // collide with the vim layer's leader.
                    self.trigger_completion();
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
                CtKey::Char('r') => {
                    self.open_history();
                    return true;
                }
                CtKey::PageDown => {
                    self.cycle_result_tab(1);
                    return true;
                }
                CtKey::PageUp => {
                    self.cycle_result_tab(-1);
                    return true;
                }
                _ => {}
            }
        }
        // ? opens help in normal mode when the editor pane is NOT focused.
        // In the editor pane, ? is reserved for reverse search (plan 06-06).
        if key.code == CtKey::Char('?')
            && key.modifiers.is_empty()
            && self.vim.mode() == Mode::Normal
            && self.focus != Pane::Editor
        {
            self.toggle_help();
            return true;
        }
        false
    }

    fn handle_editor_key(&mut self, key: KeyEvent) {
        // The editor search prompt is modal: characters build the needle,
        // Enter accepts, Esc cancels and restores the cursor.
        if self.tabs[self.active_tab].editor_search.prompt_open {
            self.handle_editor_search_key(key);
            return;
        }
        // The completion popup is modal while it's open: Tab cycles,
        // Enter accepts, Esc closes. Plain character keys fall through
        // so the user can keep typing and the popup refreshes against
        // the new prefix on the way out.
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

        // After every insert-mode keystroke, refresh the completion
        // popup against the new word prefix. Two thresholds:
        // - prefix.len() >= 2 opens or refreshes the popup;
        // - prefix.len() < 2 closes any open popup so the user can
        //   type short words without a flashing list.
        // Silent: no status spam, no '4-space' fallback — manual Tab
        // / Ctrl-Space still handle those cases.
        if self.vim.mode() == Mode::Insert {
            self.maybe_auto_complete();
        }
    }

    /// Build a column-name lookup map from the session's schema cache.
    ///
    /// Keys are lowercased table names; values are `(schema_name, columns)`
    /// tuples so each column completion can carry the schema as its detail
    /// string. Returns an empty map when no session is active.
    fn column_cache(&self) -> std::collections::HashMap<String, (String, Vec<ColumnHeader>)> {
        let Some(session) = self.session.as_ref() else {
            return std::collections::HashMap::new();
        };
        let mut map = std::collections::HashMap::new();
        for (schema, tables) in &session.schemas {
            for table in tables {
                let key = table.name.to_ascii_lowercase();
                // Only insert if not already present (first schema wins).
                map.entry(key)
                    .or_insert_with(|| (schema.name.clone(), Vec::new()));
            }
        }
        // Merge any cached column data from the session.
        for (table_lower, (schema_name, cols)) in &session.column_cache {
            map.insert(table_lower.clone(), (schema_name.clone(), cols.clone()));
        }
        map
    }

    /// Refresh-or-close the completion popup based on the current word
    /// prefix. Called after every insert-mode keystroke. See
    /// [`Self::trigger_completion`] for the manual (Tab / Ctrl-Space)
    /// variant that handles the empty-prefix and no-matches cases
    /// explicitly.
    fn maybe_auto_complete(&mut self) {
        let prefix = self.tabs[self.active_tab].editor.current_word_prefix();
        if prefix.len() < 2 {
            self.tabs[self.active_tab].completion = None;
            return;
        }
        let schemas = self
            .session
            .as_ref()
            .map(|s| s.schemas.as_slice())
            .unwrap_or(&[]);
        let buffer_text = self.tabs[self.active_tab].editor.entire_text();
        let offset = self.tabs[self.active_tab].editor.cursor_byte_offset();
        let context = detect_context(&buffer_text, offset);
        let columns = self.column_cache();
        let items = gather_completions(&prefix, schemas, &context, &columns, 50);
        if items.is_empty() {
            self.tabs[self.active_tab].completion = None;
            return;
        }
        // Preserve the user's current selection across keystrokes when
        // possible — a brand-new popup starts at index 0.
        let selected = self.tabs[self.active_tab]
            .completion
            .as_ref()
            .map(|c| c.selected.min(items.len() - 1))
            .unwrap_or(0);
        self.tabs[self.active_tab].completion = Some(CompletionState {
            items,
            selected,
            prefix,
        });
    }

    // ----- editor search -----

    /// Open the editor search prompt (`/` for forward, `?` for backward).
    fn open_editor_search(&mut self, direction: SearchDirection) {
        let tab = &mut self.tabs[self.active_tab];
        tab.editor_search.saved_cursor = Some(tab.editor.cursor());
        tab.editor_search.direction = direction;
        tab.editor_search.prompt_open = true;
        tab.editor_search.needle.clear();
        tab.editor_search.matches.clear();
        tab.editor_search.current = None;
        let prompt_char = match direction {
            SearchDirection::Forward => '/',
            SearchDirection::Backward => '?',
        };
        self.status.message = format!("{prompt_char}");
    }

    /// Handle a key event while the editor search prompt is open.
    fn handle_editor_search_key(&mut self, key: KeyEvent) {
        match key.code {
            CtKey::Esc => {
                let tab = &mut self.tabs[self.active_tab];
                if let Some((row, col)) = tab.editor_search.saved_cursor.take() {
                    tab.editor.set_cursor(row, col);
                }
                tab.editor_search.prompt_open = false;
                tab.editor_search.needle.clear();
                tab.editor_search.matches.clear();
                tab.editor_search.current = None;
                tab.editor_search.highlight = false;
                self.status.message = "search cancelled".into();
            }
            CtKey::Enter => {
                let tab = &mut self.tabs[self.active_tab];
                tab.editor_search.prompt_open = false;
                tab.editor_search.highlight = true;
                // Set current to whatever match the cursor is on.
                self.sync_editor_search_current();
                let count = self.tabs[self.active_tab].editor_search.matches.len();
                let needle = self.tabs[self.active_tab].editor_search.needle.clone();
                if count == 0 {
                    self.status.message = format!("/{needle} · no matches");
                } else {
                    let idx = self.tabs[self.active_tab]
                        .editor_search
                        .current
                        .map(|i| i + 1)
                        .unwrap_or(1);
                    self.status.message = format!("/{needle} · {idx}/{count}");
                }
            }
            CtKey::Backspace => {
                self.tabs[self.active_tab].editor_search.needle.pop();
                self.refresh_editor_search_matches();
                self.jump_to_editor_search_match();
                let needle = self.tabs[self.active_tab].editor_search.needle.clone();
                let prompt_char = match self.tabs[self.active_tab].editor_search.direction {
                    SearchDirection::Forward => '/',
                    SearchDirection::Backward => '?',
                };
                self.status.message = format!("{prompt_char}{needle}");
            }
            CtKey::Char(c) => {
                self.tabs[self.active_tab].editor_search.needle.push(c);
                self.refresh_editor_search_matches();
                self.jump_to_editor_search_match();
                let needle = self.tabs[self.active_tab].editor_search.needle.clone();
                let prompt_char = match self.tabs[self.active_tab].editor_search.direction {
                    SearchDirection::Forward => '/',
                    SearchDirection::Backward => '?',
                };
                self.status.message = format!("{prompt_char}{needle}");
            }
            _ => {}
        }
    }

    /// Recompute all match positions for the current needle.
    fn refresh_editor_search_matches(&mut self) {
        let needle = self.tabs[self.active_tab].editor_search.needle.clone();
        if needle.is_empty() {
            self.tabs[self.active_tab].editor_search.matches.clear();
            self.tabs[self.active_tab].editor_search.current = None;
            return;
        }
        let text = self.tabs[self.active_tab].editor.entire_text();
        let matches = find_all(&text, &needle);
        self.tabs[self.active_tab].editor_search.matches = matches;
        self.sync_editor_search_current();
    }

    /// Jump the cursor to the best match given the current direction
    /// and saved cursor position.
    fn jump_to_editor_search_match(&mut self) {
        let tab = &self.tabs[self.active_tab];
        if tab.editor_search.matches.is_empty() {
            return;
        }
        let (cur_row, cur_col) = tab
            .editor_search
            .saved_cursor
            .unwrap_or_else(|| tab.editor.cursor());
        let direction = tab.editor_search.direction;
        let cursor_byte = row_col_to_offset(&tab.editor, cur_row, cur_col);

        let idx = match direction {
            SearchDirection::Forward => tab
                .editor_search
                .matches
                .iter()
                .position(|&(l, c)| {
                    let m_byte = row_col_to_offset(&tab.editor, l, c);
                    m_byte > cursor_byte
                })
                .or({
                    // Wrap around.
                    if !tab.editor_search.matches.is_empty() {
                        Some(0)
                    } else {
                        None
                    }
                }),
            SearchDirection::Backward => {
                // Find the last match before the cursor.
                let mut best: Option<usize> = None;
                for (i, &(l, c)) in tab.editor_search.matches.iter().enumerate() {
                    let m_byte = row_col_to_offset(&tab.editor, l, c);
                    if m_byte < cursor_byte {
                        best = Some(i);
                    } else {
                        break;
                    }
                }
                best.or_else(|| {
                    // Wrap around to the last match.
                    if !tab.editor_search.matches.is_empty() {
                        Some(tab.editor_search.matches.len() - 1)
                    } else {
                        None
                    }
                })
            }
        };

        if let Some(i) = idx {
            let (row, col) = self.tabs[self.active_tab].editor_search.matches[i];
            self.tabs[self.active_tab].editor.set_cursor(row, col);
            self.tabs[self.active_tab].editor_search.current = Some(i);
        }
    }

    /// Set `current` to the index of the match the cursor currently sits on.
    fn sync_editor_search_current(&mut self) {
        let tab = &self.tabs[self.active_tab];
        let (cur_row, cur_col) = tab.editor.cursor();
        let needle_len = tab.editor_search.needle.len();
        if needle_len == 0 {
            self.tabs[self.active_tab].editor_search.current = None;
            return;
        }
        let current = tab
            .editor_search
            .matches
            .iter()
            .position(|&(l, c)| l == cur_row && c <= cur_col && cur_col < c + needle_len)
            .or_else(|| {
                tab.editor_search
                    .matches
                    .iter()
                    .position(|&(l, c)| l == cur_row && c == cur_col)
            });
        self.tabs[self.active_tab].editor_search.current = current;
    }

    /// Repeat the editor search in the original or reverse direction.
    fn repeat_editor_search(&mut self, reverse: bool) {
        let tab = &self.tabs[self.active_tab];
        if tab.editor_search.needle.is_empty() {
            self.status.message = "no previous search".into();
            return;
        }
        if tab.editor_search.matches.is_empty() {
            self.status.message = format!("/{} · no matches", tab.editor_search.needle);
            return;
        }
        let direction = tab.editor_search.direction;
        let go_forward = match (direction, reverse) {
            (SearchDirection::Forward, false) => true,
            (SearchDirection::Forward, true) => false,
            (SearchDirection::Backward, false) => false,
            (SearchDirection::Backward, true) => true,
        };

        let count = tab.editor_search.matches.len();
        let cur = tab.editor_search.current.unwrap_or(0);
        let next = if go_forward {
            (cur + 1) % count
        } else {
            (cur + count - 1) % count
        };

        let (row, col) = self.tabs[self.active_tab].editor_search.matches[next];
        self.tabs[self.active_tab].editor.set_cursor(row, col);
        self.tabs[self.active_tab].editor_search.current = Some(next);
        let needle = self.tabs[self.active_tab].editor_search.needle.clone();
        self.status.message = format!("/{needle} · {}/{count}", next + 1);
    }

    // ----- substitute -----

    /// Execute a substitute command (`:s/old/new/[g][c]` or `:%s/old/new/[g][c]`).
    fn execute_substitute(
        &mut self,
        range: crate::commands::SubstituteRange,
        pattern: &str,
        replacement: &str,
        global: bool,
        confirm: bool,
    ) {
        if confirm {
            // TODO(v1.1): implement interactive confirm mode with y/n/a/q.
            // For v1, execute all replacements and report via status message.
            self.status.message = "confirm flag not yet supported; replacing all matches".into();
        }

        let total_replacements = match range {
            crate::commands::SubstituteRange::CurrentLine => {
                let row = self.tabs[self.active_tab].editor.cursor_row();
                let line = self.tabs[self.active_tab].editor.get_line(row).to_owned();
                let (new_line, count) = if global {
                    replace_all(&line, pattern, replacement)
                } else {
                    replace_first(&line, pattern, replacement)
                };
                if count > 0 {
                    self.tabs[self.active_tab]
                        .editor
                        .replace_line(row, &new_line);
                }
                count
            }
            crate::commands::SubstituteRange::WholeBuffer => {
                let line_count = self.tabs[self.active_tab].editor.line_count();
                let mut total = 0usize;
                for row in 0..line_count {
                    let line = self.tabs[self.active_tab].editor.get_line(row).to_owned();
                    let (new_line, count) = if global {
                        replace_all(&line, pattern, replacement)
                    } else {
                        replace_first(&line, pattern, replacement)
                    };
                    if count > 0 {
                        self.tabs[self.active_tab]
                            .editor
                            .replace_line(row, &new_line);
                    }
                    total += count;
                }
                total
            }
        };

        if total_replacements == 0 {
            self.status.message = format!("{pattern} not found");
        } else {
            self.status.message = format!("{total_replacements} replacement(s) made");
        }
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
        let buffer_text = self.tabs[self.active_tab].editor.entire_text();
        let offset = self.tabs[self.active_tab].editor.cursor_byte_offset();
        let context = detect_context(&buffer_text, offset);
        let columns = self.column_cache();
        let items = gather_completions(&prefix, schemas, &context, &columns, 50);
        if items.is_empty() {
            self.status.message = format!("no completions for '{prefix}'");
            return;
        }
        if items.len() == 1 {
            // Exactly one match: insert it without showing the popup.
            let only = items[0].text.clone();
            self.tabs[self.active_tab]
                .editor
                .replace_current_word_with(&only);
            self.status.message = format!("completed: {only}");
            return;
        }
        self.tabs[self.active_tab].completion = Some(CompletionState {
            items,
            selected: 0,
            prefix,
        });
        self.status.message = "completion: ↑↓ cycles · Tab/Enter accepts · Esc cancels".into();
    }

    /// Returns `true` when the key was consumed by the completion popup.
    ///
    /// Bindings inside the popup follow the IDE convention used by
    /// IntelliJ / DataGrip / VS Code so the muscle memory transfers:
    /// - Tab / Enter: accept the selected completion
    /// - ↑ / ↓: move the highlight
    /// - Shift-Tab: previous highlight (kept for keyboards without
    ///   arrow access in vim-aware terminal multiplexers)
    /// - Esc: dismiss the popup; the editor stays in insert mode and
    ///   the originally typed prefix is preserved
    fn handle_completion_key(&mut self, key: KeyEvent) -> bool {
        let Some(state) = self.tabs[self.active_tab].completion.as_mut() else {
            return false;
        };
        match key.code {
            CtKey::Esc => {
                self.tabs[self.active_tab].completion = None;
                self.status.message = "completion cancelled".into();
                true
            }
            CtKey::Enter | CtKey::Tab => {
                let choice = state.items[state.selected].text.clone();
                self.tabs[self.active_tab]
                    .editor
                    .replace_current_word_with(&choice);
                self.tabs[self.active_tab].completion = None;
                self.status.message = format!("completed: {choice}");
                true
            }
            CtKey::BackTab | CtKey::Up => {
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
            CtKey::Char('d') => self.ddl_sidebar_selection(),
            _ => {}
        }
    }

    fn preview_sidebar_selection(&mut self) {
        let Some(item) = self.sidebar_items.get(self.sidebar_index).cloned() else {
            return;
        };
        let SidebarItem::Table { schema, name, .. } = item else {
            self.status.message = "select a table to preview".into();
            return;
        };
        self.run_preview(&schema, &name, 0);
    }

    /// Pressing `d` with a sidebar table focused fetches the DDL and
    /// injects it into the editor at the cursor. No auto-run — the
    /// user inspects and decides.
    fn ddl_sidebar_selection(&mut self) {
        let Some(item) = self.sidebar_items.get(self.sidebar_index).cloned() else {
            return;
        };
        let SidebarItem::Table { schema, name, .. } = item else {
            self.status.message = "select a table to fetch DDL".into();
            return;
        };
        self.inject_ddl(&schema, &name);
    }

    fn inject_ddl(&mut self, schema: &str, name: &str) {
        let Some(session) = self.session.as_ref() else {
            self.status.message = "no active connection".into();
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
                conn.fetch_ddl(&schema_owned, &name_owned).await
            })
        });
        match result {
            Ok(ddl) => {
                self.tabs[self.active_tab].editor.insert_str(&ddl);
                self.status.message = format!("injected DDL for {schema}.{name}");
                self.focus = Pane::Editor;
            }
            Err(e) => {
                self.status.message = format!("DDL fetch failed: {e}");
            }
        }
    }

    /// Dispatch a `SELECT * FROM schema.table LIMIT n OFFSET k` and attach
    /// the table's schema as the result's row source so cell edits and
    /// pagination work.
    fn run_preview(&mut self, schema: &str, table: &str, offset: usize) {
        let Some(session) = self.session.as_ref() else {
            self.status.message = "no active connection".into();
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
            Ok(ts) => {
                let columns = ts.columns.clone();
                // Cache column names for completion.
                if let Some(session) = self.session.as_mut() {
                    session.column_cache.insert(
                        table.to_ascii_lowercase(),
                        (
                            schema.to_owned(),
                            columns
                                .iter()
                                .map(|c| ColumnHeader {
                                    name: c.name.clone(),
                                    data_type: c.data_type.clone(),
                                })
                                .collect(),
                        ),
                    );
                }
                Some(RowSource {
                    schema: schema.to_owned(),
                    table: table.to_owned(),
                    columns,
                    offset,
                    limit,
                })
            }
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
            self.status.message = "no preview to paginate; select a table first".into();
            return;
        };
        let limit = self.tabs[self.active_tab].page_size;
        self.run_preview(&schema, &table, offset + limit);
    }

    fn prev_page(&mut self) {
        let Some((schema, table, offset)) = self.current_preview_target() else {
            self.status.message = "no preview to paginate; select a table first".into();
            return;
        };
        if offset == 0 {
            self.status.message = "already on the first page".into();
            return;
        }
        let limit = self.tabs[self.active_tab].page_size;
        let new_offset = offset.saturating_sub(limit);
        self.run_preview(&schema, &table, new_offset);
    }

    fn set_page_size(&mut self, size: usize) {
        self.tabs[self.active_tab].page_size = size;
        self.status.message = format!("page size set to {size}");
    }

    fn current_preview_target(&self) -> Option<(String, String, usize)> {
        match self.tabs[self.active_tab].results.active_state() {
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
            self.status.message = "no active connection".into();
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
                let col_count = ts.columns.len();
                let idx_count = ts.indexes.len();
                let fk_count = ts.foreign_keys.len();
                let table_schema = ts.table.schema.clone();
                let table_name = ts.table.name.clone();
                let columns = ts.columns.clone();
                // Cache column names for completion.
                if let Some(session) = self.session.as_mut() {
                    session.column_cache.insert(
                        table_name.to_ascii_lowercase(),
                        (
                            table_schema.clone(),
                            columns
                                .iter()
                                .map(|c| ColumnHeader {
                                    name: c.name.clone(),
                                    data_type: c.data_type.clone(),
                                })
                                .collect(),
                        ),
                    );
                }
                self.tabs[self.active_tab].results.active_mut().reset();
                self.status.message = format!(
                    "{}.{}: {} cols·{} idx·{} fk",
                    table_schema, table_name, col_count, idx_count, fk_count
                );
                *self.tabs[self.active_tab].results.active_state_mut() =
                    ResultState::TableDetail { schema: ts };
            }
            Err(error) => {
                self.status.message = format!("describe failed: {error}");
            }
        }
    }

    fn handle_results_key(&mut self, key: KeyEvent) {
        // Row detail modal: sits at the same layer as the cell popup.
        // When open, it intercepts navigation and dismiss keys.
        if self.tabs[self.active_tab].row_detail.is_some() {
            self.handle_row_detail_key(key);
            return;
        }
        if self.tabs[self.active_tab].editing.is_some() {
            self.handle_cell_edit_key(key);
            return;
        }
        if self.tabs[self.active_tab].results.active().popup.is_some() {
            if matches!(key.code, CtKey::Esc | CtKey::Char('q') | CtKey::Enter) {
                self.tabs[self.active_tab].results.active_mut().popup = None;
            }
            return;
        }
        // Filter prompt editing: modal — consumes keys before any
        // other result-pane handler.
        if self.tabs[self.active_tab]
            .results
            .active()
            .filter_prompt_open
        {
            match key.code {
                CtKey::Esc => {
                    let rv = self.tabs[self.active_tab].results.active_mut();
                    rv.filter.clear();
                    rv.filter_prompt_open = false;
                    self.status.message = "filter cleared".into();
                }
                CtKey::Enter => {
                    let filter_text = self.tabs[self.active_tab].results.active().filter.clone();
                    self.tabs[self.active_tab]
                        .results
                        .active_mut()
                        .filter_prompt_open = false;
                    self.status.message = if filter_text.is_empty() {
                        "filter closed".into()
                    } else {
                        format!("filter: {filter_text}")
                    };
                }
                CtKey::Backspace => {
                    self.tabs[self.active_tab].results.active_mut().filter.pop();
                }
                CtKey::Char(c) => {
                    self.tabs[self.active_tab]
                        .results
                        .active_mut()
                        .filter
                        .push(c);
                }
                _ => {}
            }
            return;
        }
        if let Some(search) = self.tabs[self.active_tab].search.as_mut() {
            if search.editing {
                match key.code {
                    CtKey::Esc => {
                        self.tabs[self.active_tab].search = None;
                        self.status.message = "search cancelled".into();
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

        // Compute visible row count (after filter/sort) for navigation.
        let (visible_count, col_count) = match self.tabs[self.active_tab].results.active_state() {
            ResultState::Rows { rows, columns, .. } => {
                let vis = self.tabs[self.active_tab]
                    .results
                    .active()
                    .visible_rows(columns, rows);
                (vis.len(), columns.len())
            }
            ResultState::Running { rows, columns, .. } => (rows.len(), columns.len()),
            _ => (0, 0),
        };

        match key.code {
            CtKey::Char('j') | CtKey::Down => self.tabs[self.active_tab]
                .results
                .active_mut()
                .move_down(visible_count),
            CtKey::Char('k') | CtKey::Up => {
                self.tabs[self.active_tab].results.active_mut().move_up()
            }
            CtKey::Char('h') | CtKey::Left => {
                self.tabs[self.active_tab].results.active_mut().move_left()
            }
            CtKey::Char('l') | CtKey::Right => self.tabs[self.active_tab]
                .results
                .active_mut()
                .move_right(col_count),
            CtKey::Char('g') => self.tabs[self.active_tab]
                .results
                .active_mut()
                .state
                .select(Some(0)),
            CtKey::Char('G') if visible_count > 0 => {
                self.tabs[self.active_tab]
                    .results
                    .active_mut()
                    .state
                    .select(Some(visible_count - 1));
            }
            CtKey::Char('s') => self.toggle_sort(),
            CtKey::Char('/') => self.open_filter_prompt(),
            CtKey::Char('n') => self.advance_search(1),
            CtKey::Char('N') => self.advance_search(-1),
            CtKey::Esc => {
                let had_search = self.tabs[self.active_tab].search.take().is_some();
                let had_filter = !self.tabs[self.active_tab]
                    .results
                    .active()
                    .filter
                    .is_empty();
                if had_search {
                    self.status.message = "search cleared".into();
                }
                if had_filter {
                    let rv = self.tabs[self.active_tab].results.active_mut();
                    rv.filter.clear();
                    rv.filter_prompt_open = false;
                    self.status.message = "filter cleared".into();
                }
            }
            CtKey::Enter => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.open_row_detail();
                } else {
                    self.open_cell_popup();
                }
            }
            CtKey::Char('R') => self.open_row_detail(),
            CtKey::Char('e') => self.start_cell_edit(),
            CtKey::Char('y') => self.yank_cell(),
            CtKey::Char('Y') => self.yank_row(),
            CtKey::Char(']') => {
                self.pending_result_leader = Some(']');
            }
            CtKey::Char('[') => {
                self.pending_result_leader = Some('[');
            }
            _ => {}
        }
    }

    // ----- yank -----

    /// Translate the current TableState selection (which is an index
    /// into the visible/rendered rows) to the original row index in
    /// the full result set. Returns `None` when there are no rows.
    fn selected_original_row(&self) -> Option<usize> {
        let tab = &self.tabs[self.active_tab];
        let vis_selected = tab.results.active().state.selected()?;
        tab.results
            .active()
            .visible_indices
            .get(vis_selected)
            .copied()
    }

    fn yank_cell(&mut self) {
        let tab = &self.tabs[self.active_tab];
        let (rows, _columns) = match tab.results.active_state() {
            ResultState::Rows { rows, columns, .. }
            | ResultState::Running { rows, columns, .. } => (rows, columns),
            _ => {
                self.status.message = "no cell to yank".into();
                return;
            }
        };
        let row_idx = self.selected_original_row().unwrap_or(0);
        let col_idx = tab.results.active().column_index;
        let Some(value) = rows.get(row_idx).and_then(|r| r.0.get(col_idx)) else {
            self.status.message = "no cell selected".into();
            return;
        };
        let text = match value {
            narwhal_core::Value::Null => String::new(),
            other => other.render(),
        };
        match self.clipboard.set_text(&text) {
            Ok(()) => {
                self.status.message = format!("yanked {} char(s) to clipboard", text.len());
            }
            Err(error) => {
                self.status.message = format!("yank failed: {error}");
            }
        }
    }

    fn yank_row(&mut self) {
        let tab = &self.tabs[self.active_tab];
        let rows = match tab.results.active_state() {
            ResultState::Rows { rows, .. } | ResultState::Running { rows, .. } => rows,
            _ => {
                self.status.message = "no row to yank".into();
                return;
            }
        };
        let row_idx = self.selected_original_row().unwrap_or(0);
        let Some(row) = rows.get(row_idx) else {
            self.status.message = "no row selected".into();
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
                self.status.message = format!("yanked row ({} cell(s)) to clipboard", row.0.len());
            }
            Err(error) => {
                self.status.message = format!("yank failed: {error}");
            }
        }
    }

    fn start_cell_edit(&mut self) {
        // Gather the data we need by value first, then mutate.
        let prepared = {
            let tab = &self.tabs[self.active_tab];
            let (columns, rows, source) = match tab.results.active_state() {
                ResultState::Rows {
                    columns,
                    rows,
                    source: Some(source),
                    ..
                } => (columns, rows, source),
                ResultState::Rows { source: None, .. } => {
                    self.status.message =
                        "this result is read-only (no row source); preview a table to edit".into();
                    return;
                }
                _ => {
                    self.status.message = "no editable cell here".into();
                    return;
                }
            };
            if columns.is_empty() || rows.is_empty() {
                self.status.message = "no rows to edit".into();
                return;
            }
            if !source.columns.iter().any(|c| c.primary_key) {
                self.status.message =
                    format!("{}: no primary key, cell edits are disabled", source.table);
                return;
            }
            let row_index = self.selected_original_row().unwrap_or(0);
            let col_index = tab.results.active().column_index;
            let Some(row) = rows.get(row_index) else {
                self.status.message = "select a row first (j/k)".into();
                return;
            };
            let Some(column) = columns.get(col_index) else {
                self.status.message = "select a column first (h/l)".into();
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
        tab.results.active_mut().edit = Some(CellEditView {
            column_name,
            column_type,
            row_index,
            buffer,
            error: None,
        });
        self.status.message = "edit: Enter saves · Esc cancels".into();
    }

    fn handle_cell_edit_key(&mut self, key: KeyEvent) {
        let Some(edit) = self.tabs[self.active_tab].editing.as_mut() else {
            return;
        };
        match key.code {
            CtKey::Esc => {
                self.tabs[self.active_tab].editing = None;
                self.tabs[self.active_tab].results.active_mut().edit = None;
                self.status.message = "edit cancelled".into();
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
        if let (Some(edit), Some(view)) =
            (tab.editing.as_ref(), tab.results.active_mut().edit.as_mut())
        {
            view.buffer = edit.buffer.clone();
            view.error = None;
        }
    }

    fn commit_cell_edit(&mut self) {
        let Some(edit) = self.tabs[self.active_tab].editing.clone() else {
            return;
        };
        let Some(session) = self.session.as_ref() else {
            self.status.message = "no active connection".into();
            return;
        };
        // Extract everything we need from the result before mutating state.
        let (columns, rows, source) = if let ResultState::Rows {
            columns,
            rows,
            source: Some(source),
            ..
        } = self.tabs[self.active_tab].results.active_state()
        {
            (columns.clone(), rows.clone(), source.clone())
        } else {
            self.status.message = "result is no longer editable".into();
            return;
        };
        let Some(row) = rows.get(edit.row_index).cloned() else {
            self.status.message = "row went away under the editor".into();
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
                if let ResultState::Rows { rows, .. } =
                    self.tabs[self.active_tab].results.active_state_mut()
                {
                    if let Some(row) = rows.get_mut(edit.row_index) {
                        if let Some(cell) = row.0.get_mut(edit.column_index) {
                            *cell = new_value;
                        }
                    }
                }
                self.tabs[self.active_tab].editing = None;
                self.tabs[self.active_tab].results.active_mut().edit = None;
                self.status.message = format!("updated 1 row in {}", source.table);
            }
            Err(error) => {
                self.set_edit_error(error.to_string());
            }
        }
    }

    fn set_edit_error(&mut self, message: String) {
        if let Some(view) = self.tabs[self.active_tab]
            .results
            .active_mut()
            .edit
            .as_mut()
        {
            view.error = Some(message.clone());
        }
        self.status.message = format!("edit failed: {message}");
    }

    #[allow(dead_code)]
    fn start_search(&mut self) {
        if !matches!(
            self.tabs[self.active_tab].results.active_state(),
            ResultState::Rows { .. } | ResultState::Running { .. }
        ) {
            self.status.message = "no result to search".into();
            return;
        }
        self.tabs[self.active_tab].search = Some(ResultSearch {
            query: String::new(),
            matches: Vec::new(),
            current: None,
            editing: true,
        });
        self.status.message = "search: ".into();
    }

    fn toggle_sort(&mut self) {
        // Streaming guard.
        if self.running {
            self.status.message = "sort/filter unavailable while streaming".into();
            return;
        }
        if !matches!(
            self.tabs[self.active_tab].results.active_state(),
            ResultState::Rows { .. }
        ) {
            self.status.message = "no result to sort".into();
            return;
        }
        let col = self.tabs[self.active_tab].results.active().column_index;
        let view = self.tabs[self.active_tab].results.active_mut();
        let next = match view.sort {
            Some((c, SortDir::Asc)) if c == col => Some((col, SortDir::Desc)),
            Some((c, SortDir::Desc)) if c == col => None,
            _ => Some((col, SortDir::Asc)),
        };
        view.sort = next;
        let msg = match view.sort {
            Some((c, SortDir::Asc)) => format!("sort: column {} ascending", c + 1),
            Some((c, SortDir::Desc)) => format!("sort: column {} descending", c + 1),
            None => "sort: cleared".into(),
        };
        self.status.message = msg;
    }

    fn open_filter_prompt(&mut self) {
        // Streaming guard.
        if self.running {
            self.status.message = "sort/filter unavailable while streaming".into();
            return;
        }
        if !matches!(
            self.tabs[self.active_tab].results.active_state(),
            ResultState::Rows { .. }
        ) {
            self.status.message = "no result to filter".into();
            return;
        }
        self.tabs[self.active_tab]
            .results
            .active_mut()
            .filter_prompt_open = true;
        self.status.message = "filter: type to filter, Enter accepts, Esc clears".into();
    }

    fn refresh_search_matches(&mut self) {
        let needle = match self.tabs[self.active_tab].search.as_ref() {
            Some(s) if !s.query.is_empty() => s.query.to_lowercase(),
            Some(_) => {
                if let Some(s) = self.tabs[self.active_tab].search.as_mut() {
                    s.matches.clear();
                    s.current = None;
                }
                self.status.message = "search: ".into();
                return;
            }
            None => return,
        };
        let matches = match self.tabs[self.active_tab].results.active_state() {
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
        self.status.message = if total == 0 {
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
        self.status.message = format!("search: {query} · {}/{}", next + 1, total);
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
            .results
            .active_mut()
            .state
            .select(Some(idx));
    }

    fn open_cell_popup(&mut self) {
        let Some(row_index) = self.selected_original_row() else {
            self.status.message = "select a row first (j/k)".into();
            return;
        };
        let col_index = self.tabs[self.active_tab].results.active().column_index;
        let (columns, rows) = match self.tabs[self.active_tab].results.active_state() {
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
        self.tabs[self.active_tab].results.active_mut().popup = Some(CellPopup {
            column_name: column.name.clone(),
            column_type: column.data_type.clone(),
            value_text: value.render(),
            row_index,
        });
    }

    // ----- row detail modal -----

    fn open_row_detail(&mut self) {
        let tab = &self.tabs[self.active_tab];
        // Don't open if another modal at the same layer is already open.
        if tab.row_detail.is_some() || tab.results.active().popup.is_some() || tab.editing.is_some()
        {
            return;
        }
        // Compute visible rows to map selected index → original row index.
        // This avoids depending on `visible_indices` being populated by
        // a prior render pass.
        let Some(vis_selected) = tab.results.active().state.selected() else {
            self.status.message = "no row selected".into();
            return;
        };
        let (columns, rows) = match tab.results.active_state() {
            ResultState::Rows { columns, rows, .. } => (columns.clone(), rows.clone()),
            ResultState::Running { columns, rows, .. } => (columns.clone(), rows.clone()),
            _ => {
                self.status.message = "no result to inspect".into();
                return;
            }
        };
        let visible = tab.results.active().visible_rows(&columns, &rows);
        let Some(&row_idx) = visible.get(vis_selected) else {
            self.status.message = "no row selected".into();
            return;
        };
        let Some(row) = rows.get(row_idx) else {
            return;
        };
        self.tabs[self.active_tab].row_detail = Some(RowDetailState {
            row_index: row_idx,
            columns,
            values: row.0.clone(),
            selected_column: 0,
            scroll_offset: 0,
        });
    }

    fn handle_row_detail_key(&mut self, key: KeyEvent) {
        let Some(state) = self.tabs[self.active_tab].row_detail.as_mut() else {
            return;
        };
        let col_count = state.columns.len().saturating_sub(1);
        match key.code {
            CtKey::Up | CtKey::Char('k') => {
                state.selected_column = state.selected_column.saturating_sub(1);
                state.scroll_offset = 0;
            }
            CtKey::Down | CtKey::Char('j') => {
                if state.selected_column < col_count {
                    state.selected_column += 1;
                }
                state.scroll_offset = 0;
            }
            CtKey::PageUp => {
                let page = 10usize; // approximate page size
                state.selected_column = state.selected_column.saturating_sub(page);
                state.scroll_offset = 0;
            }
            CtKey::PageDown => {
                let page = 10usize;
                state.selected_column = (state.selected_column + page).min(col_count);
                state.scroll_offset = 0;
            }
            CtKey::Char('g') => {
                state.selected_column = 0;
                state.scroll_offset = 0;
            }
            CtKey::Char('G') => {
                state.selected_column = col_count;
                state.scroll_offset = 0;
            }
            CtKey::Esc | CtKey::Char('R') => {
                self.tabs[self.active_tab].row_detail = None;
                self.status.message = "row detail closed".into();
            }
            CtKey::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.tabs[self.active_tab].row_detail = None;
                self.status.message = "row detail closed".into();
            }
            _ => {}
        }
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
                self.status.message = match mode {
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
                    self.status.message = format!(":{}", self.vim.command_buffer());
                }
            }
            Action::PromptComplete => self.complete_prompt(),
            Action::OpenSearch(dir) => self.open_editor_search(dir),
            Action::RepeatSearch => self.repeat_editor_search(false),
            Action::RepeatSearchReverse => self.repeat_editor_search(true),
            Action::Operate { .. } => {}
        }
    }

    // ----- prompt tab-completion -----

    /// Complete the last token in the `:`-prompt buffer against the
    /// universe appropriate for the current command head.
    ///
    /// - `:open <pref>`, `:remove <pref>`, `:rm <pref>`, `:forget <pref>`
    ///   → connection names from `ConnectionsFile`
    /// - `:help <pref>` → built-in command names ∪ plugin command names
    /// - `:export <pref>` → `csv` | `json`
    /// - bare `:` (empty buffer) → no completion (too noisy)
    /// - any other head → no-op
    fn complete_prompt(&mut self) {
        let buf = self.vim.command_buffer().to_owned();
        let parts: Vec<&str> = buf.split_whitespace().collect();
        let head = parts.first().copied().unwrap_or("");

        // Identify which universe to complete from.
        let universe: Vec<String> = match head {
            "open" | "o" | "remove" | "rm" | "forget" => self
                .connections
                .connections
                .iter()
                .map(|c| c.name.clone())
                .collect(),
            "help" | "h" => {
                let mut v: Vec<String> = crate::commands::BUILTIN_COMMAND_NAMES
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect();
                v.extend(
                    self.plugins
                        .catalogue()
                        .into_iter()
                        .map(|(_, cmd)| cmd.name),
                );
                v
            }
            "export" => vec!["csv".into(), "json".into(), "insert".into()],
            "save" | "load" | "rm-snippet" | "rmsnippet" => {
                self.snippet_store.list().unwrap_or_default()
            }
            _ => return,
        };

        // The token being completed is the last whitespace-separated word;
        // if the buffer ends with whitespace we are starting a fresh token
        // (empty prefix).
        let prefix = if buf.ends_with(char::is_whitespace) {
            String::new()
        } else {
            parts.last().copied().unwrap_or("").to_owned()
        };

        // When the prefix is the command head itself (user typed
        // `:open` with no trailing space), we are not completing an
        // argument yet — skip so the command head isn't replaced.
        if prefix == head && !buf.ends_with(char::is_whitespace) {
            return;
        }

        let matches: Vec<&str> = universe
            .iter()
            .filter(|name| name.to_lowercase().starts_with(&prefix.to_lowercase()))
            .map(String::as_str)
            .collect();

        match matches.as_slice() {
            [] => {
                self.status.message = format!("no completions for {prefix:?}");
            }
            [only] => {
                self.vim.replace_command_token(only);
                self.status.message = format!(":{}", self.vim.command_buffer());
            }
            many => {
                let lcp = longest_common_prefix(many);
                if lcp.len() > prefix.len() {
                    self.vim.replace_command_token(&lcp);
                }
                self.status.message = many.join(" ");
            }
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
                *self.tabs[self.active_tab].results.active_state_mut() = ResultState::Empty;
                self.tabs[self.active_tab].results.active_mut().reset();
                self.status.message = "buffer cleared".into();
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
            Command::History => self.open_history(),
            Command::NewTab => self.new_tab(),
            Command::CloseTab => self.close_tab(),
            Command::NextTab => self.cycle_tab(1),
            Command::PrevTab => self.cycle_tab(-1),
            Command::Help(None) => {
                self.status.message =
                    "open <name> · close · refresh · run · run-all · stream · stream-all · explain · export <csv|json|insert> <path> · cancel · quit"
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
                    self.status.message = format!(":{name} — {desc}");
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
                    self.status.message = format!(":{name} — {desc}");
                } else {
                    self.status.message = format!("unknown command: {name}");
                }
            }
            Command::Substitute {
                range,
                pattern,
                replacement,
                global,
                confirm,
            } => self.execute_substitute(range, &pattern, &replacement, global, confirm),
            Command::NoHlSearch => {
                self.tabs[self.active_tab].editor_search.highlight = false;
                self.tabs[self.active_tab].editor_search.needle.clear();
                self.tabs[self.active_tab].editor_search.matches.clear();
                self.tabs[self.active_tab].editor_search.current = None;
                self.status.message = "search highlight cleared".into();
            }
            Command::SaveSnippet { name } => self.save_snippet(&name),
            Command::LoadSnippet { name } => self.load_snippet_by_name(&name),
            Command::RemoveSnippet { name } => self.remove_snippet(&name),
            Command::ListSnippets => self.open_snippets_modal(),
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
                    self.status.message = format!("unknown command: {text}");
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
            self.status.message = format!("auto-loaded {loaded} plugin(s) from {}", dir.display());
        }
        loaded
    }

    fn load_plugin(&mut self, path: &str) {
        let plugin = match LuaPlugin::from_path(path) {
            Ok(p) => p,
            Err(e) => {
                self.status.message = format!("plug-load failed: {e}");
                return;
            }
        };
        let name = plugin.name().to_owned();
        let cmd_count = plugin.commands().len();
        match self.register_lua_plugin(plugin) {
            Ok(_) => {
                self.status.message = format!("plugin '{name}' loaded ({cmd_count} command(s))");
            }
            Err(e) => {
                self.status.message = format!("plug-load failed: {e}");
            }
        }
    }

    fn list_plugins(&mut self) {
        let catalogue = self.plugins.catalogue();
        if catalogue.is_empty() {
            self.status.message = "no plugins loaded; use :plug-load <file.lua>".into();
            return;
        }
        let summary = catalogue
            .iter()
            .map(|(plugin, cmd)| format!("{}:{} — {}", plugin, cmd.name, cmd.description))
            .collect::<Vec<_>>()
            .join(" · ");
        self.status.message = summary;
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
                self.status.message = message;
            }
            Ok(PluginCommandOutcome::InsertSql { sql, append }) => {
                if !append {
                    self.tabs[self.active_tab].editor.clear();
                }
                self.tabs[self.active_tab].editor.insert_str(&sql);
                self.status.message = format!("plugin inserted {} char(s) of SQL", sql.len());
            }
            Ok(PluginCommandOutcome::Silent) => {}
            Err(PluginError::Unknown(name)) => {
                self.status.message = format!("unknown command: {name}");
            }
            Err(PluginError::Timeout { elapsed_secs }) => {
                let plugin_name = self
                    .plugins
                    .plugin_for(command)
                    .map(|p| p.name().to_owned())
                    .unwrap_or_else(|| command.to_owned());
                self.status.message =
                    format!("plugin {plugin_name}: timed out after {elapsed_secs:.1}s");
            }
            Err(error) => {
                self.status.message = format!("plugin error: {error}");
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
            self.status.message = format!("driver not registered: {}", config.driver);
            return;
        };
        let label = config.name.clone();
        self.status.message = format!("connecting to {label}…");

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
                    let mut state = self.plugin_state.lock().expect("plugin_state poisoned");
                    state.pool = Some(session.pool.clone());
                    state.in_transaction = false;
                }
                self.session = Some(session);
                self.rebuild_sidebar();
                self.focus = Pane::Editor;
            }
            Err(error) => {
                self.status.message = format!("connect failed: {error}");
            }
        }
    }

    fn close_session(&mut self) {
        if self.session.take().is_some() {
            let mut state = self.plugin_state.lock().expect("plugin_state poisoned");
            state.pool = None;
            state.in_transaction = false;
            drop(state);
            self.status.connection = None;
            self.status.transaction = None;
            self.status.message = "connection closed".into();
            self.rebuild_sidebar();
        }
    }

    fn refresh_schema(&mut self) {
        let Some(session) = self.session.as_mut() else {
            self.status.message = "no active connection".into();
            return;
        };
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(session.refresh_schemas())
        });
        match result {
            Ok(()) => {
                self.rebuild_sidebar();
                let table_count = self.count_sidebar_tables();
                self.status.message = format!("schema refreshed · {table_count} tables");
            }
            Err(error) => self.status.message = format!("refresh failed: {error}"),
        }
    }

    /// Count the number of tables currently shown in the sidebar.
    fn count_sidebar_tables(&self) -> usize {
        self.sidebar_items
            .iter()
            .filter(|item| matches!(item, SidebarItem::Table { .. }))
            .count()
    }

    /// Schedule a debounced schema refresh. Each call resets the 200ms
    /// timer; the refresh fires once the timer expires without being
    /// rescheduled. A migration with 50 DDL statements fires exactly
    /// one refresh.
    fn schedule_schema_refresh(&mut self) {
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
                tokio::time::sleep(Duration::from_millis(200)).await;
                if pending.swap(false, Ordering::Acquire) {
                    let _ = tx.send(RunUpdate::SchemaRefresh).await;
                }
            })
            .abort_handle(),
        );
    }

    // ----- dispatch -----

    fn dispatch_current_statement(&mut self, mode: RunMode) {
        let Some(session) = self.session.as_ref() else {
            self.status.message = "no active connection".into();
            return;
        };
        let Some(sql) = self.tabs[self.active_tab]
            .editor
            .statement_at_cursor(session.dialect())
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

    fn dispatch_all_statements(&mut self, mode: RunMode) {
        let Some(session) = self.session.as_ref() else {
            self.status.message = "no active connection".into();
            return;
        };
        let statements = self.tabs[self.active_tab]
            .editor
            .all_statements(session.dialect());
        if statements.is_empty() {
            self.status.message = "buffer contains no statements".into();
            return;
        }
        self.dispatch_batch(statements, mode);
    }

    fn dispatch_batch(&mut self, statements: Vec<String>, mode: RunMode) {
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

    fn start_wizard(&mut self) {
        self.wizard = Some(ConnectionWizard::new());
        self.wizard_error = None;
        self.status.message = "add: Tab moves · ←/→ driver · Enter saves · Esc cancels".into();
    }

    // ----- transactions -----

    fn begin_transaction(&mut self, isolation: Option<IsolationArg>) {
        if self.running {
            self.status.message = "a query is already running".into();
            return;
        }
        let Some(session) = self.session.as_mut() else {
            self.status.message = "no active connection".into();
            return;
        };
        if session.transaction.is_some() {
            self.status.message = "a transaction is already open".into();
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
                self.status.transaction = iso.map(|level| isolation_label(level).to_owned());
                self.status.message = match iso {
                    Some(level) => format!("transaction started ({})", isolation_label(level)),
                    None => "transaction started".into(),
                };
            }
            Err(error) => {
                self.status.message = format!("begin failed: {error}");
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
            self.status.message = "a query is already running".into();
            return;
        }
        let Some(session) = self.session.as_mut() else {
            self.status.message = "no active connection".into();
            return;
        };
        let Some(txn) = session.transaction.take() else {
            self.status.message = "no open transaction".into();
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
        self.status.transaction = None;
        match outcome {
            Ok(()) => {
                self.status.message = if commit {
                    "transaction committed".into()
                } else {
                    "transaction rolled back".into()
                };
            }
            Err(error) => {
                self.status.message = if commit {
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
            self.status.message = "a query is already running".into();
            return;
        }
        let Some(session) = self.session.as_mut() else {
            self.status.message = "no active connection".into();
            return;
        };
        let Some(txn) = session.transaction.as_ref() else {
            self.status.message = "no open transaction".into();
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
                self.status.message = ok_msg(name);
            }
            Err(error) => {
                self.status.message = err_msg(name, &error);
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
        self.status.message = format!("removed connection '{name}'");
    }

    fn forget_password(&mut self, name: &str) {
        let Some(config) = self.connections.connections.iter().find(|c| c.name == name) else {
            self.status.message = format!("forget: no connection named '{name}'");
            return;
        };
        match self.credentials.delete(config.id) {
            Ok(()) => {
                self.status.message = format!("forgot password for '{name}'");
            }
            Err(error) => {
                self.status.message = format!("forget failed: {error}");
            }
        }
    }

    fn cancel_wizard(&mut self) {
        if self.wizard.take().is_some() {
            self.wizard_error = None;
            self.status.message = "add cancelled".into();
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
                self.status.message = format!("connection '{name}' saved");
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
        if self.running {
            self.status.message = "cannot open a new tab while a query is running".into();
            return;
        }
        let name = format!("untitled-{}", self.next_tab_id);
        self.next_tab_id += 1;
        self.tabs.push(Tab::new(name));
        self.active_tab = self.tabs.len() - 1;
        self.status.message = format!("tab {} opened", self.active_tab + 1);
        self.focus = Pane::Editor;
    }

    fn close_tab(&mut self) {
        if self.running {
            self.status.message = "cannot close a tab while a query is running".into();
            return;
        }
        if self.tabs.len() == 1 {
            self.status.message = "last tab; use :q to quit".into();
            return;
        }
        self.tabs.remove(self.active_tab);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        self.status.message = format!("tab closed; now on {}", self.active_tab + 1);
    }

    fn cycle_tab(&mut self, delta: i32) {
        if self.running {
            self.status.message = "cannot switch tabs while a query is running".into();
            return;
        }
        if self.tabs.len() <= 1 {
            return;
        }
        let len = self.tabs.len() as i32;
        let next = ((self.active_tab as i32) + delta).rem_euclid(len) as usize;
        self.active_tab = next;
        self.status.message = format!(
            "tab {} of {} · {}",
            self.active_tab + 1,
            self.tabs.len(),
            self.tabs[self.active_tab].name
        );
    }

    /// Cycle through the per-statement results inside the active tab's
    /// [`ResultBundle`]. `delta` +1 goes forward, −1 goes backward.
    /// Does nothing when the bundle has only one result.
    fn cycle_result_tab(&mut self, delta: i32) {
        let bundle = &mut self.tabs[self.active_tab].results;
        if !bundle.is_multi() {
            return;
        }
        match delta {
            1 => bundle.next(),
            -1 => bundle.prev(),
            _ => {}
        }
        let active = bundle.active;
        let total = bundle.states.len();
        self.status.message = format!("result {} of {total}", active + 1);
    }

    fn dump_schema(&mut self, target: DumpTarget) {
        let Some(session) = self.session.as_ref() else {
            self.status.message = "no active connection".into();
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
                if let ResultState::TableDetail { schema } =
                    self.tabs[self.active_tab].results.active_state()
                {
                    vec![(schema.table.schema.clone(), schema.table.name.clone())]
                } else {
                    self.status.message =
                        "dump-schema: select a table in the sidebar or pass a name".into();
                    return;
                }
            }
            DumpTarget::All => schemas,
            DumpTarget::Named(ref name) => {
                if let Some(pair) = schemas.iter().find(|(_, t)| t == name).cloned() {
                    vec![pair]
                } else {
                    self.status.message = format!("dump-schema: table not found: {name}");
                    return;
                }
            }
        };

        if names.is_empty() {
            self.status.message = "dump-schema: nothing to dump".into();
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
                self.status.message = format!(
                    "dump-schema: wrote {} table(s) into the editor buffer",
                    tables.len()
                );
                self.focus = Pane::Editor;
            }
            Err(error) => {
                self.status.message = format!("dump-schema failed: {error}");
            }
        }
    }

    fn dispatch_explain(&mut self) {
        let Some(session) = self.session.as_ref() else {
            self.status.message = "no active connection".into();
            return;
        };
        if session.driver.name() != "postgres" {
            self.status.message = "explain is only supported on postgres for now".into();
            return;
        }
        let Some(sql) = self.tabs[self.active_tab]
            .editor
            .statement_at_cursor(session.dialect())
        else {
            self.status.message = "no statement under cursor".into();
            return;
        };
        let trimmed = sql.trim().trim_end_matches(';').trim().to_owned();
        if trimmed.is_empty() {
            self.status.message = "no statement under cursor".into();
            return;
        }
        self.dispatch_batch(vec![wrap_explain(&trimmed)], RunMode::Execute);
        self.status.message = "explaining…".into();
    }

    fn export_results(&mut self, format: &str, path: &str) {
        let Some(format) = ExportFormat::from_token(format) else {
            self.status.message = format!("unknown export format: {format} (csv|json|insert)");
            return;
        };
        let (columns, rows, source_table) = match self.tabs[self.active_tab].results.active_state()
        {
            ResultState::Rows {
                columns,
                rows,
                source_table,
                ..
            } => (columns.clone(), rows.clone(), source_table.clone()),
            ResultState::Running { columns, rows, .. } if !columns.is_empty() => {
                (columns.clone(), rows.clone(), None)
            }
            _ => {
                self.status.message = "no tabular result to export".into();
                return;
            }
        };

        // Respect active filter/sort: export only the visible rows.
        let visible_indices = self.tabs[self.active_tab]
            .results
            .active()
            .visible_rows(&columns, &rows);
        let visible_rows: Vec<Row> = visible_indices.iter().map(|&i| rows[i].clone()).collect();

        let path_buf = std::path::PathBuf::from(path);
        match export_rows(
            &columns,
            &visible_rows,
            format,
            &path_buf,
            source_table.as_ref(),
        ) {
            Ok(()) => {
                self.status.message = format!(
                    "exported {} rows to {} ({})",
                    visible_rows.len(),
                    path_buf.display(),
                    format.default_extension()
                );
            }
            Err(error) => {
                self.status.message = format!("export failed: {error}");
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
        self.status.message = "cancellation requested".into();
    }

    // ----- run-loop integration -----

    /// Receive the next [`RunUpdate`] from the worker channel.
    pub async fn recv_run_update(&mut self) -> Option<RunUpdate> {
        self.run_rx.recv().await
    }

    pub fn handle_run_update(&mut self, update: RunUpdate) {
        // All mutations target the tab that *started* the run, not the
        // tab the user may have switched to in the meantime (K1-A fix).
        let rt = self.run_tab_index();
        match update {
            RunUpdate::StatementStarted { index, total, sql } => {
                let streaming = matches!(
                    self.tabs[rt].results.active_state(),
                    ResultState::Running {
                        streaming: true,
                        ..
                    }
                );
                // If there is a previously-finalized entry sitting in the
                // active slot (index > 1 means we've finished at least one
                // statement), snapshot it before overwriting.
                //
                // Note: `index` is 1-based, so index > 1 means we are
                // past the first StatementStarted.
                if index > 1 {
                    let state = self.tabs[rt].results.states.swap_remove(0);
                    let view = self.tabs[rt].results.views.swap_remove(0);
                    self.pending_result_entries_states.push(state);
                    self.pending_result_entries_views.push(view);
                }
                // Create a fresh entry for the new statement.
                self.tabs[rt].results = ResultBundle::single(
                    ResultState::Running {
                        sql: sql.clone(),
                        index,
                        total,
                        columns: Vec::new(),
                        rows: Vec::new(),
                        streaming,
                        started_at: Instant::now(),
                        last_render: Instant::now(),
                    },
                    ResultView::new(),
                );
                self.status.message = format!("running {index}/{total}: {}", truncate(&sql, 60));
            }
            RunUpdate::HeaderReady { columns: cols } => {
                if let ResultState::Running { columns, .. } =
                    self.tabs[rt].results.active_state_mut()
                {
                    *columns = cols;
                }
            }
            RunUpdate::RowsAppended { rows: new_rows } => {
                if let ResultState::Running {
                    rows,
                    last_render,
                    streaming: true,
                    ..
                } = self.tabs[rt].results.active_state_mut()
                {
                    rows.extend(new_rows);
                    let now = Instant::now();
                    if now.duration_since(*last_render) >= Duration::from_millis(100) {
                        *last_render = now;
                    }
                } else if let ResultState::Running { rows, .. } =
                    self.tabs[rt].results.active_state_mut()
                {
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
                // If a streaming query was cancelled, produce a Cancelled
                // state so the title bar shows the partial row count.
                if let ResultState::Running {
                    rows,
                    streaming: true,
                    started_at,
                    ..
                } = self.tabs[rt].results.active_state()
                {
                    if error.contains("cancelled") {
                        let rows_so_far = rows.len();
                        let elapsed_ms = started_at
                            .elapsed()
                            .as_millis()
                            .try_into()
                            .unwrap_or(u64::MAX);
                        *self.tabs[rt].results.active_state_mut() = ResultState::Cancelled {
                            rows_so_far,
                            elapsed_ms,
                        };
                        self.tabs[rt].results.active_mut().reset();
                        return;
                    }
                }
                *self.tabs[rt].results.active_state_mut() = ResultState::Error {
                    message: error,
                    elapsed_ms,
                };
                self.tabs[rt].results.active_mut().reset();
            }
            RunUpdate::AllDone {
                successes,
                failures,
                ddl,
            } => {
                self.running = false;
                self.run_tab = None;

                // Build the final ResultBundle from collected entries.
                // Push the current (last) active entry into the pending
                // list first so everything is in order.
                let current_state = self.tabs[rt].results.states.swap_remove(0);
                let current_view = self.tabs[rt].results.views.swap_remove(0);
                self.pending_result_entries_states.push(current_state);
                self.pending_result_entries_views.push(current_view);

                let states = std::mem::take(&mut self.pending_result_entries_states);
                let views = std::mem::take(&mut self.pending_result_entries_views);
                self.tabs[rt].results = if states.len() == 1 {
                    // Single result — no strip, behaviour-preserving.
                    ResultBundle::single(
                        states.into_iter().next().unwrap_or(ResultState::Empty),
                        views.into_iter().next().unwrap_or_default(),
                    )
                } else {
                    let len = states.len();
                    let mut bundle = ResultBundle::multi(states, views);
                    // Default to showing the last result — this is the
                    // SELECT the user most likely cares about, and matches
                    // the pre-bundle behaviour where the final statement
                    // overwrote the result pane.
                    bundle.active = len - 1;
                    bundle
                };

                let base = if failures == 0 {
                    format!("done · {successes} statement(s)")
                } else {
                    format!("done · {successes} ok · {failures} failed")
                };
                self.status.message = match self.plugin_warning.take() {
                    Some(warning) => format!("{base} · {warning}"),
                    None => base,
                };

                // If any successful statement was DDL, schedule a
                // debounced schema refresh so the sidebar stays in sync.
                if ddl {
                    self.schedule_schema_refresh();
                }
            }
            RunUpdate::SchemaRefresh => {
                self.refresh_schema();
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

    /// Like [`Self::drain_run_updates`] but also waits for any pending
    /// debounced schema refresh to fire. Useful in tests that need to
    /// observe the auto-refresh side-effect of a DDL statement.
    pub async fn drain_run_updates_and_refresh(&mut self) {
        self.drain_run_updates().await;
        // Wait for the debounce timer to fire (200ms + small slack).
        if self.refresh_task.is_some() {
            tokio::time::sleep(Duration::from_millis(350)).await;
            // The debounce task sends SchemaRefresh through run_rx;
            // consume it.
            while let Ok(update) = self.run_rx.try_recv() {
                self.handle_run_update(update);
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
        let rt = self.run_tab_index();
        let (columns, rows, index, total, sql) =
            match std::mem::take(self.tabs[rt].results.active_state_mut()) {
                ResultState::Running {
                    columns,
                    rows,
                    index,
                    total,
                    sql,
                    ..
                } => (columns, rows, index, total, sql),
                other => {
                    *self.tabs[rt].results.active_state_mut() = other;
                    return;
                }
            };
        if columns.is_empty() {
            *self.tabs[rt].results.active_state_mut() = ResultState::Affected {
                rows: rows_affected.unwrap_or(0),
                elapsed_ms,
                index,
                total,
            };
        } else if is_explain_result(&columns) {
            match extract_explain_plan(&rows) {
                Ok(plan) => {
                    *self.tabs[rt].results.active_state_mut() = ResultState::Explain {
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
                    self.status.message = format!("explain ok · {elapsed_ms} ms");
                    return;
                }
                Err(error) => {
                    *self.tabs[rt].results.active_state_mut() = ResultState::Error {
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
            let source = self.tabs[rt].pending_source.take();
            let source_table = crate::export::extract_source_table(&sql);
            let (columns, rows) = self.apply_plugin_transforms(columns, rows, elapsed_ms);
            *self.tabs[rt].results.active_state_mut() = ResultState::Rows {
                columns,
                rows,
                elapsed_ms,
                streamed,
                index,
                total,
                source,
                source_table,
            };
        }
        self.status.message = match rows_affected {
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
            streaming,
            started_at,
            ..
        } => ResultDisplay::Running {
            sql,
            index: *index,
            total: *total,
            columns,
            rows,
            streaming: *streaming,
            started_at: *started_at,
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
            source_table: _,
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
        ResultState::Cancelled {
            rows_so_far,
            elapsed_ms,
        } => ResultDisplay::Cancelled {
            rows_so_far: *rows_so_far,
            elapsed_ms: *elapsed_ms,
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

/// Compute the longest common prefix across a non-empty slice of strings,
/// character by character.
fn longest_common_prefix(strings: &[&str]) -> String {
    if strings.is_empty() {
        return String::new();
    }
    let first = strings[0];
    let mut end = 0;
    for (i, ch) in first.char_indices() {
        if strings[1..].iter().all(|s| s.chars().nth(i) == Some(ch)) {
            end = i + ch.len_utf8();
        } else {
            break;
        }
    }
    first[..end].to_owned()
}

/// Find all occurrences of `needle` in `buffer`, returning
/// `(line_idx, byte_col)` pairs. Literal substring, no regex.
fn find_all(buffer: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (line_idx, line) in buffer.lines().enumerate() {
        let mut start = 0;
        while let Some(pos) = line[start..].find(needle) {
            out.push((line_idx, start + pos));
            start += pos + needle.len().max(1);
        }
    }
    out
}

/// Convert a (row, col) position in the editor buffer to a byte offset.
fn row_col_to_offset(buffer: &EditorBuffer, row: usize, col: usize) -> usize {
    let mut offset = 0usize;
    for (i, line) in buffer.lines().iter().enumerate() {
        if i == row {
            return offset + col.min(line.len());
        }
        offset += line.len() + 1; // +1 for the synthetic newline
    }
    offset
}

/// Replace the first occurrence of `pattern` with `replacement` in `text`.
/// Returns the new string and the number of replacements (0 or 1).
fn replace_first(text: &str, pattern: &str, replacement: &str) -> (String, usize) {
    if let Some(pos) = text.find(pattern) {
        let mut result = String::with_capacity(text.len() + replacement.len());
        result.push_str(&text[..pos]);
        result.push_str(replacement);
        result.push_str(&text[pos + pattern.len()..]);
        (result, 1)
    } else {
        (text.to_owned(), 0)
    }
}

/// Replace every occurrence of `pattern` with `replacement` in `text`.
/// Returns the new string and the count of replacements.
fn replace_all(text: &str, pattern: &str, replacement: &str) -> (String, usize) {
    if pattern.is_empty() {
        return (text.to_owned(), 0);
    }
    let mut result = String::with_capacity(text.len());
    let mut count = 0usize;
    let mut start = 0;
    while let Some(pos) = text[start..].find(pattern) {
        result.push_str(&text[start..start + pos]);
        result.push_str(replacement);
        start += pos + pattern.len();
        count += 1;
    }
    result.push_str(&text[start..]);
    (result, count)
}
