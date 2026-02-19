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

use std::sync::Arc;

use narwhal_config::CredentialStore;
use narwhal_tui::{LayoutRegions, Pane, ResultView, Theme};
use narwhal_vim::Vim;
use tokio::sync::mpsc;

use crate::clipboard::Clipboard;
use crate::keymap::Keymap;

use crate::meta::MetaUpdate;
use crate::registry::DriverRegistry;
use crate::run::RunUpdate;

use narwhal_plugin::PluginRegistry;

pub mod state;
pub use state::{
    CellEdit, CompletionState, EditorSearchState, HistoryState, JsonViewerState, ModalState,
    ProcessState, ResultBundle, ResultSearch, ResultState, RowDetailState, RowSource, SessionState,
    SidebarItem, SnippetsModal, StatusBar, Tab,
};

/// Pure, IO-free application state and behaviour.
pub struct AppCore {
    pub(super) registry: DriverRegistry,
    pub(super) credentials: Arc<dyn CredentialStore>,
    pub(super) clipboard: Arc<dyn Clipboard>,
    pub(super) plugins: Arc<PluginRegistry>,
    /// Shared handle the plugin SQL executor reads on every
    /// `narwhal.sql_run` call. Updated whenever a session opens or
    /// closes so scripts always target the currently-active
    /// connection.
    pub(super) plugin_state: Arc<std::sync::Mutex<PluginConnectionState>>,
    /// Every modal-overlay field (wizard, help, history search,
    /// snippets picker). Bundled so the modal precedence check in
    /// `handle_key` has a single source of truth and so modal
    /// state cannot accidentally bleed into non-modal handlers.
    pub(super) modals: ModalState,
    /// Connection catalogue, active session, history journal,
    /// snippet store, recency cache, and the read-only gate.
    /// Bundled so a session swap touches one struct instead of
    /// nine fields.
    pub(super) session: SessionState,
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
    /// Lifecycle + async-bridge state (`should_quit`, `running`,
    /// `run_tab`, `cancel_slot`, `plugin_warning`, `refresh_task`,
    /// `refresh_pending`, `run_tx`, `meta_tx`). Bundled so the
    /// event-loop invariants live in one place; the matching channel
    /// receivers
    /// stay below because draining them needs mutable `AppCore`.
    pub(super) process: ProcessState,
    /// Pending leader key for result-tab cycling. `]` followed by
    /// `r` cycles forward; `[` followed by `r` cycles backward.
    pub(super) pending_result_leader: Option<char>,
    /// Collects per-statement results during a multi-statement batch.
    /// Populated by `finalize_statement`; consumed and turned into a
    /// `ResultBundle` by the `AllDone` handler.
    pub(super) pending_result_entries_states: Vec<ResultState>,
    pub(super) pending_result_entries_views: Vec<ResultView>,
    pub(super) last_layout: LayoutRegions,
    /// Receiver halves of the channels owned by `process`.
    /// Kept outside `ProcessState` because draining them needs
    /// mutable access to `AppCore` (handlers mutate UI / session /
    /// modal state, not just process state).
    pub(crate) run_rx: mpsc::Receiver<RunUpdate>,
    pub(crate) meta_rx: mpsc::Receiver<MetaUpdate>,
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
}

mod accessors;
mod construct;
mod dispatch;
