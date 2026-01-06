//! Headless application state.
//!
//! [`AppCore`] owns every piece of state the UI depends on but contains no
//! terminal-IO logic. The render path takes a [`ratatui::Frame`] from the
//! caller, and key events come in as parsed crossterm [`KeyEvent`]s, so the
//! core is fully usable with `ratatui::backend::TestBackend` in tests.
//!
//! Submodules under `core/` host pure helpers extracted from this file as
//! part of the L21 split. They never touch [`AppCore`] state directly.

mod dump_export;
mod editor_handlers;
mod plugin_executor;
mod plugins;
mod render_helpers;
mod results_actions;
mod run_loop;
mod sessions;
mod tabs;
mod text_utils;
mod transactions;
use plugin_executor::PluginConnectionState;
use render_helpers::{display_from_state, sidebar_depth, sidebar_kind, sidebar_label};
use text_utils::split_head_arg;


use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use crossterm::event::{KeyCode as CtKey, KeyEvent, KeyModifiers};
use narwhal_config::{ConnectionsFile, CredentialStore, InMemoryStore};
use narwhal_core::{
    Column, ColumnHeader, Row, TableKind, TableSchema,
};
use narwhal_history::{HistoryEntry, Journal};
use narwhal_tui::{
    render_help_modal, render_history_modal, render_root, render_row_detail, render_snippets_modal,
    render_wizard, CompletionItemView, CompletionPopupView, EditorBuffer,
    EditorSearchHighlight, ExplainPlanLine, HistoryModalState, HistoryRow, LayoutRegions, Pane,
    ResultView, RootLayout, RowDetailView, SearchHighlight, SidebarRow, SidebarView,
    SnippetsModalState, StatusBarView, Theme, WizardFieldView, WizardView,
};
use narwhal_vim::{Mode, SearchDirection, Vim};
use ratatui::layout::Rect;
use ratatui::Frame;
use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;

use crate::clipboard::{Clipboard, InMemoryClipboard};
use crate::commands::{parse, Command};
use crate::completion::{Completion, CompletionKind};

use crate::meta::{MetaRequest, MetaUpdate};
use crate::registry::DriverRegistry;
use crate::run::{ActiveCancel, RunMode, RunUpdate};
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

    // accept_completion_at, handle_global_key, handle_editor_key, column_cache,
    // maybe_auto_complete, open_editor_search, handle_editor_search_key,
    // refresh_editor_search_matches, jump_to_editor_search_match,
    // sync_editor_search_current, repeat_editor_search, execute_substitute,
    // trigger_completion, handle_completion_key, apply_action, complete_prompt
    // moved to `core::editor_handlers` (L21).

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

    // Session lifecycle (open_named, open_connection*, close_session),
    // schema (refresh_schema, count_sidebar_tables, schedule_schema_refresh),
    // dispatch (dispatch_current_statement, dispatch_all_statements, dispatch_batch),
    // wizard entry (start_wizard) and removal (remove_connection, forget_password)
    // moved to `core::sessions` (L21).

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

    // dump_schema, dump_schema_single, dispatch_explain, export_results
    // moved to `core::dump_export` (L21).

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
