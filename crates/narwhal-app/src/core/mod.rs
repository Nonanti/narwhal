//! Headless application state.
//!
//! [`AppCore`] owns every piece of state the UI depends on but contains no
//! terminal-IO logic. The render path takes a [`ratatui::Frame`] from the
//! caller, and key events come in as parsed crossterm `KeyEvent`s, so the
//! core is fully usable with `ratatui::backend::TestBackend` in tests.
//!
//! Submodules under `core/` host pure helpers extracted from this file as
//! part of the L21 split. They never touch [`AppCore`] state directly.

mod confirm;
mod diagram;
mod diff_schema;
mod dump_export;
mod editor_dispatch;
mod fk_nav;
mod format;
mod goto;
mod lint_cmd;
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
use tokio::sync::mpsc;

use crate::meta::MetaUpdate;
use crate::run::RunUpdate;

pub mod state;
pub use state::{
    AppDeps, CellEdit, CompletionState, ConfirmModal, DiagramModalState, DiagramMode,
    EditorSearchState, GotoCorpusCache, GotoEntry, GotoMatch, GotoModal, HistoryState,
    JsonViewerState, ModalState, PendingConfirm, ProcessState, ResultBundle, ResultSearch,
    ResultState, RowDetailState, RowSource, SessionState, SidebarItem, SnippetsModal, StatusBar,
    Tab, UiState,
};

/// Pure, IO-free application state and behaviour.
pub struct AppCore {
    /// Wiring established at startup: driver registry, credential
    /// store, clipboard, plugin registry + state, keymap. Cheap to
    /// clone (everything is an `Arc` or owned-value handle); the
    /// test fixture only has to build an `AppDeps` to mock the
    /// entire I/O boundary.
    pub(super) deps: AppDeps,
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
    /// Visible-on-screen state (tabs, focus, sidebar, theme,
    /// status, last layout, pending leader / result entries).
    /// Bundled so the dispatcher and render helpers carry a
    /// single mutable borrow instead of a dozen.
    pub(super) ui: UiState,
    /// Lifecycle + async-bridge state (`should_quit`, `running`,
    /// `run_tab`, `cancel_slot`, `plugin_warning`, `refresh_task`,
    /// `refresh_pending`, `run_tx`, `meta_tx`). Bundled so the
    /// event-loop invariants live in one place; the matching channel
    /// receivers
    /// stay below because draining them needs mutable `AppCore`.
    pub(super) process: ProcessState,
    /// Receiver halves of the channels owned by `process`.
    /// Kept outside `ProcessState` because draining them needs
    /// mutable access to `AppCore` (handlers mutate UI / session /
    /// modal state, not just process state).
    pub(crate) run_rx: mpsc::Receiver<RunUpdate>,
    pub(crate) meta_rx: mpsc::Receiver<MetaUpdate>,
    /// One-shot warnings collected from the most recent keymap override
    /// pass. Surfaced to the status bar once on the first render so the
    /// user notices malformed bindings without us having to plumb a
    /// dedicated banner widget.
    pub(super) keymap_warnings: Vec<String>,
}

mod accessors;
mod construct;
mod dispatch;
