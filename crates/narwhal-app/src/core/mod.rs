//! Headless application state.
//!
//! [`AppCore`] owns every piece of state the UI depends on but contains no
//! terminal-IO logic. The render path takes a [`ratatui::Frame`] from the
//! caller, and key events come in as parsed crossterm `KeyEvent`s, so the
//! core is fully usable with `ratatui::backend::TestBackend` in tests.
//!
//! Submodules under `core/` host pure helpers extracted from this file as
//! part of the L21 split. They never touch [`AppCore`] state directly.

mod dump_export;
mod editor_dispatch;
mod format;
mod modals;
mod pending_actions;
mod plugin_executor;
mod plugins;
mod render_helpers;
mod results_actions;
mod run_loop;
mod sessions;
mod tabs;
pub(super) mod text_utils;
mod transactions;
use plugin_executor::PluginConnectionState;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use uuid::Uuid;

use narwhal_config::{ConnectionsFile, CredentialStore};
use narwhal_history::Journal;
use narwhal_tui::{LayoutRegions, Pane, ResultView, Theme};
use narwhal_vim::Vim;
use tokio::sync::mpsc;

use crate::clipboard::Clipboard;
use crate::keymap::Keymap;

use crate::meta::MetaUpdate;
use crate::registry::DriverRegistry;
use crate::run::{ActiveCancel, RunUpdate};
use crate::session::Session;
use crate::snippets::SnippetStore;
use narwhal_plugin::PluginRegistry;

pub mod state;
pub use state::{
    CellEdit, CompletionState, EditorSearchState, HistoryState, JsonViewerState, ModalState,
    ResultBundle, ResultSearch, ResultState, RowDetailState, RowSource, SidebarItem, SnippetsModal,
    StatusBar, Tab,
};

/// Pure, IO-free application state and behaviour.
pub struct AppCore {
    pub(super) registry: DriverRegistry,
    pub(super) connections: ConnectionsFile,
    pub(super) connections_path: Option<std::path::PathBuf>,
    /// Recency cache feeding the sidebar ordering. Populated from
    /// `paths.last_used_file()` on start-up (via [`Self::set_last_used_path`])
    /// and bumped on every successful `:open`.
    pub(super) last_used: narwhal_config::LastUsedStore,
    pub(super) last_used_path: Option<std::path::PathBuf>,
    pub(super) credentials: Arc<dyn CredentialStore>,
    pub(super) clipboard: Arc<dyn Clipboard>,
    pub(super) plugins: Arc<PluginRegistry>,
    /// Shared handle the plugin SQL executor reads on every
    /// `narwhal.sql_run` call. Updated whenever a session opens or
    /// closes so scripts always target the currently-active
    /// connection.
    pub(super) plugin_state: Arc<std::sync::Mutex<PluginConnectionState>>,
    pub(super) history_journal: Option<Arc<Journal>>,
    /// Persistent snippet store. (Not modal state — this is the
    /// on-disk catalogue. The modal that picks from it lives in
    /// `modals.snippets`.)
    pub(super) snippet_store: SnippetStore,
    /// Every modal-overlay field (wizard, help, history search,
    /// snippets picker). Bundled so the modal precedence check in
    /// `handle_key` has a single source of truth and so modal
    /// state cannot accidentally bleed into non-modal handlers.
    pub(super) modals: ModalState,
    pub(super) session: Option<Session>,
    pub(super) tabs: Vec<Tab>,
    pub(super) active_tab: usize,
    pub(super) next_tab_id: usize,
    pub(super) vim: Vim,
    pub(super) theme: Theme,
    pub(super) focus: Pane,
    pub(super) sidebar_items: Vec<SidebarItem>,
    pub(super) sidebar_index: usize,
    /// Sidebar viewport scroll (L24). First visible row.
    pub(super) sidebar_scroll: usize,
    pub(super) status: StatusBar,
    /// One-shot warning carried over from a plugin (transform or command
    /// hook) so that the final 'done · N statement(s)' `AllDone` message
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
    /// Channel for background metadata operations (`dump_schema`, refresh,
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
    /// Active key map. Starts as the built-in defaults; mutated in place
    /// by [`Self::apply_settings`] whenever the user's `config.toml`
    /// supplies a `[keymap.<group>]` override. Cloned reads are not
    /// taken on the hot path — the dispatcher borrows immutably.
    pub(super) keymap: Keymap,
    /// One-shot warnings collected from the most recent keymap override
    /// pass. Surfaced to the status bar once on the first render so the
    /// user notices malformed bindings without us having to plumb a
    /// dedicated banner widget.
    pub(super) keymap_warnings: Vec<String>,
    /// L36 #11: global read-only switch. When `true`, every row-CRUD
    /// entry point refuses to stage mutations regardless of the
    /// driver's `row_level_dml` capability. Driven by the `--read-only`
    /// CLI flag.
    pub(super) read_only: bool,
    /// `ConnectionConfig.id`s for which an `OpenSession` meta request
    /// is in flight. Used to drop stale `MetaUpdate::SessionOpened`
    /// replies (user opened another connection, closed the active
    /// one) before they clobber the current state.  (Bug H7 fix.)
    pub(super) pending_session_opens: std::collections::HashSet<Uuid>,
}

mod accessors;
mod construct;
mod dispatch;
