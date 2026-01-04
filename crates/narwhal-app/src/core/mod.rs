//! Headless application state.
//!
//! [`AppCore`] owns every piece of state the UI depends on but contains no
//! terminal-IO logic. The render path takes a [`ratatui::Frame`] from the
//! caller, and key events come in as parsed crossterm [`KeyEvent`]s, so the
//! core is fully usable with `ratatui::backend::TestBackend` in tests.
//!
//! Submodules under `core/` host pure helpers extracted from this file as
//! part of the L21 split. They never touch [`AppCore`] state directly.

mod plugin_executor;
mod plugins;
mod render_helpers;
mod results_actions;
mod run_loop;
mod tabs;
mod text_utils;
mod transactions;
use plugin_executor::PluginConnectionState;
use render_helpers::{display_from_state, sidebar_depth, sidebar_kind, sidebar_label};
use text_utils::{
    find_all, longest_common_prefix, replace_all, replace_first, row_col_to_offset,
    split_head_arg,
};


use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode as CtKey, KeyEvent, KeyModifiers};
use narwhal_config::{ConnectionsFile, CredentialStore, InMemoryStore};
use narwhal_core::{
    Column, ColumnHeader, ConnectionConfig, Row, TableKind, TableSchema,
};
use narwhal_history::{HistoryEntry, Journal};
use narwhal_tui::{
    render_help_modal, render_history_modal, render_root, render_row_detail, render_snippets_modal,
    render_wizard, translate_key_event, CompletionItemView, CompletionPopupView, EditorBuffer,
    EditorSearchHighlight, ExplainPlanLine, HistoryModalState, HistoryRow, LayoutRegions, Pane,
    ResultView, RootLayout, RowDetailView, SearchHighlight, SidebarRow, SidebarView,
    SnippetsModalState, StatusBarView, Theme, WizardFieldView, WizardView,
};
use narwhal_vim::{Action, Mode, Operator, SearchDirection, Vim};
use ratatui::layout::Rect;
use ratatui::Frame;
use secrecy::ExposeSecret;
use tokio::sync::{mpsc, Mutex};
use tracing::debug;
use uuid::Uuid;

use crate::clipboard::{Clipboard, InMemoryClipboard};
use crate::commands::{parse, Command, DumpTarget};
use crate::completion::{detect_context, gather as gather_completions, Completion, CompletionKind};
use crate::ddl::{build_dump, build_table_ddl};
use crate::editor::{all_statements, statement_at_cursor};
use crate::explain::wrap_explain;
use crate::export::{export_rows, ExportFormat};
use crate::meta::{MetaRequest, MetaUpdate};
use crate::registry::DriverRegistry;
use crate::run::{spawn_run, ActiveCancel, RunContext, RunMode, RunRequest, RunTarget, RunUpdate};
use crate::session::Session;
use crate::snippets::SnippetStore;
use crate::wizard::{ConnectionWizard, DRIVERS};
use narwhal_plugin::PluginRegistry;

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
#[non_exhaustive]
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
pub(super) enum SidebarItem {
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
    pub(super) registry: DriverRegistry,
    pub(super) connections: ConnectionsFile,
    pub(super) connections_path: Option<std::path::PathBuf>,
    pub(super) credentials: Arc<dyn CredentialStore>,
    pub(super) clipboard: Arc<dyn Clipboard>,
    pub(super) plugins: Arc<PluginRegistry>,
    /// Shared handle the plugin SQL executor reads on every
    /// `narwhal.sql_run` call. Updated whenever a session opens or
    /// closes so scripts always target the currently-active
    /// connection.
    pub(super) plugin_state: Arc<std::sync::Mutex<PluginConnectionState>>,
    pub(super) history_journal: Option<Arc<Journal>>,
    /// When `Some`, the Ctrl+R history modal is open.
    pub(super) history_state: Option<HistoryState>,
    /// Persistent snippet store.
    pub(super) snippet_store: SnippetStore,
    /// When `Some`, the `:snippets` modal is open.
    pub(super) snippets_modal: Option<SnippetsModal>,
    pub(super) session: Option<Session>,
    pub(super) tabs: Vec<Tab>,
    pub(super) active_tab: usize,
    pub(super) next_tab_id: usize,
    pub(super) vim: Vim,
    pub(super) theme: Theme,
    pub(super) focus: Pane,
    pub(super) sidebar_items: Vec<SidebarItem>,
    pub(super) sidebar_index: usize,
    pub(super) status: StatusBar,
    /// One-shot warning carried over from a plugin (transform or command
    /// hook) so that the final 'done · N statement(s)' AllDone message
    /// doesn't overwrite it silently. Cleared after it bubbles up.
    pub(super) plugin_warning: Option<String>,
    pub(super) running: bool,
    /// Tab index that owns the in-flight run. Set to `Some(active_tab)`
    /// when `dispatch_batch` fires; cleared to `None` on `AllDone`.
    /// All `handle_run_update` / `finalize_statement` mutations target
    /// this tab, not `active_tab`, so a mid-run tab switch cannot
    /// corrupt a different tab's results.  (Bug K1-A fix.)
    pub(super) run_tab: Option<usize>,
    pub(super) cancel_slot: ActiveCancel,
    pub(super) should_quit: bool,
    pub(super) wizard: Option<ConnectionWizard>,
    pub(super) wizard_error: Option<String>,
    pub(super) help_open: bool,
    /// Pending leader key for result-tab cycling. `]` followed by
    /// `r` cycles forward; `[` followed by `r` cycles backward.
    pub(super) pending_result_leader: Option<char>,
    /// Collects per-statement results during a multi-statement batch.
    /// Populated by `finalize_statement`; consumed and turned into a
    /// `ResultBundle` by the `AllDone` handler.
    pub(super) pending_result_entries_states: Vec<ResultState>,
    pub(super) pending_result_entries_views: Vec<ResultView>,
    pub(super) last_layout: LayoutRegions,
    pub(super) run_tx: mpsc::Sender<RunUpdate>,
    pub(crate) run_rx: mpsc::Receiver<RunUpdate>,
    /// Channel for background metadata operations (dump_schema, refresh,
    /// history). Separated from the run channel so meta ops don't
    /// interfere with query execution state.
    pub(super) meta_tx: mpsc::Sender<MetaUpdate>,
    pub(crate) meta_rx: mpsc::Receiver<MetaUpdate>,
    /// Handle to the in-flight debounced schema refresh task.
    /// Aborting it cancels the pending timer; a new task replaces it
    /// on every `schedule_schema_refresh` call.
    pub(super) refresh_task: Option<tokio::task::AbortHandle>,
    /// Shared flag set by `schedule_schema_refresh` and consumed by
    /// the debounce timer task to know whether a refresh is still
    /// pending.
    pub(super) refresh_pending: Arc<AtomicBool>,
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
        let (meta_tx, meta_rx) = mpsc::channel(RUN_CHANNEL_CAPACITY);
        let mut this = Self::new_inner(
            registry,
            connections,
            history,
            credentials,
            clipboard,
            run_tx,
            run_rx,
            meta_tx,
            meta_rx,
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
        meta_tx: mpsc::Sender<MetaUpdate>,
        meta_rx: mpsc::Receiver<MetaUpdate>,
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
            meta_tx,
            meta_rx,
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

    /// Open the Ctrl+R history modal. Dispatches a background
    /// load via the meta channel (H11) so the UI stays responsive.
    pub fn open_history(&mut self) {
        let Some(_journal) = &self.history_journal else {
            self.status.message = "history disabled".into();
            return;
        };
        self.dispatch_meta(MetaRequest::LoadHistory { limit: 200 });
        self.status.message = "loading history…".into();
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

    // editor_title_with_tabs moved to `core::tabs` (L21).

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
                        value: f.value.expose(),
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
            // Future panes fall through to the editor handler until wired.
            _ => self.handle_editor_key(key),
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
    /// and run a preview query. Uses `run_preview` (same as the
    /// keyboard-driven `o` path) so that `pending_source` is set and
    /// cell editing (`e`) works on mouse-previewed tables (M15).
    fn click_sidebar_table(&mut self, sidebar_idx: usize) {
        let Some(item) = self.sidebar_items.get(sidebar_idx).cloned() else {
            return;
        };
        let SidebarItem::Table { schema, name, .. } = item else {
            return;
        };
        self.sidebar_index = sidebar_idx;
        self.run_preview(&schema, &name, 0);
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
            // Future search directions: default to forward prompt.
            _ => '/',
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
                    // Future search directions: default to forward prompt.
                    _ => '/',
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
                    // Future search directions: default to forward prompt.
                    _ => '/',
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
            // Future search directions: treat as forward.
            _ => tab
                .editor_search
                .matches
                .iter()
                .position(|&(l, c)| {
                    let m_byte = row_col_to_offset(&tab.editor, l, c);
                    m_byte > cursor_byte
                })
                .or(if tab.editor_search.matches.is_empty() {
                    None
                } else {
                    Some(0)
                }),
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
            // Future directions default to forward.
            (_, false) => true,
            (_, true) => false,
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

    // handle_results_key, selected_original_row, yank_cell, yank_row,
    // start_cell_edit, handle_cell_edit_key, sync_edit_view, commit_cell_edit,
    // set_edit_error, start_search, toggle_sort, open_filter_prompt,
    // refresh_search_matches, advance_search, jump_to_current_match,
    // open_cell_popup, open_row_detail, handle_row_detail_key moved to
    // `core::results_actions` (L21).

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
                    Mode::OperatorPending(op) => format!(
                        "-- {} --",
                        match op {
                            Operator::Delete => "OPERATOR DELETE",
                            Operator::Yank => "OPERATOR YANK",
                            Operator::Change => "OPERATOR CHANGE",
                            // Future operators surface as a generic label.
                            _ => "OPERATOR",
                        }
                    ),
                    // Future modes default to a generic status line.
                    _ => "ready".into(),
                };
            }
            Action::SubmitCommand(cmd) => self.execute_command(&cmd),
            Action::Pending if self.vim.mode() == Mode::Command => {
                self.status.message = format!(":{}", self.vim.command_buffer());
            }
            Action::Pending => {}
            Action::PromptComplete => self.complete_prompt(),
            Action::OpenSearch(dir) => self.open_editor_search(dir),
            Action::RepeatSearch => self.repeat_editor_search(false),
            Action::RepeatSearchReverse => self.repeat_editor_search(true),
            Action::Operate { .. } => {}
            // Future Action variants are silently ignored until wired.
            _ => {}
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

    // Plugin lifecycle and dispatch methods moved to `core::plugins` (L21).

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
        let password = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.credentials.get(config.id))
        });
        let password = match password {
            Ok(secret) => secret,
            Err(error) => {
                debug!(target: "narwhal::app", error = %error, "keyring lookup failed; continuing without password");
                None
            }
        };
        // Convert SecretString → Option<String> for the Session::open API.
        // The driver layer still expects plain String; we expose the secret
        // only here and let it drop naturally after the call.
        let plain_password = password.map(|s| s.expose_secret().to_owned());
        self.open_connection_with_password(config, plain_password);
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
        let Some(_session) = self.session.as_ref() else {
            self.status.message = "no active connection".into();
            return;
        };
        // H11: Offload to the meta channel so the UI stays responsive
        // during schema refreshes on databases with many schemas/tables.
        self.dispatch_meta(MetaRequest::RefreshSchemas);
        self.status.message = "refreshing schema…".into();
    }

    /// Count the number of tables currently shown in the sidebar.
    fn count_sidebar_tables(&self) -> usize {
        self.sidebar_items
            .iter()
            .filter(|item| matches!(item, SidebarItem::Table { .. }))
            .count()
    }

    /// Schedule a debounced schema refresh against `session_id`. Each
    /// call resets the 200ms timer; the refresh fires once the timer
    /// expires without being rescheduled. A migration with 50 DDL
    /// statements fires exactly one refresh.
    ///
    /// The `session_id` is round-tripped through the
    /// [`RunUpdate::SchemaRefresh`] payload so the handler can drop
    /// the notification if the user has switched sessions in the
    /// meantime (bug C5).
    fn schedule_schema_refresh(&mut self, session_id: Uuid) {
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
                    let _ = tx.send(RunUpdate::SchemaRefresh { session_id }).await;
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
        let Some(sql) = statement_at_cursor(
            &self.tabs[self.active_tab].editor,
            session.dialect(),
        ) else {
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
        let statements = all_statements(
            &self.tabs[self.active_tab].editor,
            session.dialect(),
        );
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
    // Transaction methods (begin/commit/rollback/savepoint/release/
    // rollback_to_savepoint, with_txn_conn) moved to
    // `core::transactions` (L21).

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
        if let Err(error) = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.credentials.delete(removed.id))
        }) {
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
        match tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.credentials.delete(config.id))
        }) {
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
                    if let Err(error) = tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current()
                            .block_on(self.credentials.set(connection_id, secret))
                    }) {
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

    // new_tab/close_tab/cycle_tab/cycle_result_tab moved to `core::tabs` (L21).

    fn dump_schema(&mut self, target: DumpTarget) {
        let Some(_) = self.session.as_ref() else {
            self.status.message = "no active connection".into();
            return;
        };

        match target {
            DumpTarget::All => {
                // H11: Offload to the meta channel so the UI stays
                // responsive during long-running dump_schema all.
                self.dispatch_meta(MetaRequest::DumpSchemaAll {
                    tab: self.active_tab,
                });
                self.status.message = "dump-schema: fetching DDL for all tables…".into();
            }
            DumpTarget::Current | DumpTarget::Named(_) => {
                // Current/Named targets fetch a single table's DDL;
                // the blocking call is brief enough that the
                // block_in_place overhead is negligible.
                self.dump_schema_single(target);
            }
        }
    }

    /// Fetch DDL for a single named or current table (synchronous path).
    fn dump_schema_single(&mut self, target: DumpTarget) {
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
            DumpTarget::Named(ref name) => {
                if let Some(pair) = schemas.iter().find(|(_, t)| t == name).cloned() {
                    vec![pair]
                } else {
                    self.status.message = format!("dump-schema: table not found: {name}");
                    return;
                }
            }
            DumpTarget::All => unreachable!("handled by dump_schema"),
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
        let Some(sql) = statement_at_cursor(
            &self.tabs[self.active_tab].editor,
            session.dialect(),
        ) else {
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

    // Run-loop / meta-update / finalize_statement / spawn_cancel moved to
    // `core::run_loop` (L21).
}

// `is_explain_result`, `extract_explain_plan`, `display_from_state`,
// `sidebar_label`, `sidebar_depth`, and `sidebar_kind` moved to
// `core::render_helpers` (L21).

// `map_isolation` and `isolation_label` moved to `core::transactions` (L21).

// `PluginConnectionState` and `AppPluginExecutor` moved to
// `core::plugin_executor` (L21).

// Tiny text helpers moved to `text_utils.rs` (see top-of-file `mod text_utils;`).
// The original `split_head_arg` doc and a handful of pure helpers now live
// in `core::text_utils`.
