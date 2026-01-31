//! `AppCore` read-only accessors used by tests, the renderer and
//! external callers (e.g. the binary).

use narwhal_tui::{EditorBuffer, Pane};
use narwhal_vim::Mode;

use super::{
    AppCore, HistoryState, JsonViewerState, ResultState, RowDetailState, SidebarItem,
    SnippetsModal, StatusBar, Tab,
};
use crate::session::Session;
use crate::snippets::SnippetStore;

impl AppCore {
    pub fn status_message(&self) -> &str {
        &self.status.message
    }

    /// Read-only accessor for the full [`StatusBar`] struct.
    pub const fn status_bar(&self) -> &StatusBar {
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

    /// Test helper: borrow the JSON viewer modal state, if open. Lives
    /// here (alongside the other modal accessors) so integration tests
    /// don't have to plumb through the full `Tab` graph.
    #[doc(hidden)]
    pub fn json_viewer_for_test(&self) -> Option<&JsonViewerState> {
        self.tabs[self.active_tab].json_viewer.as_ref()
    }

    pub fn editor(&self) -> &EditorBuffer {
        &self.tab().editor
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// L36 #4 v1: read-only access to the live keymap. Used by tests
    /// and any future help/config tooling that wants to introspect
    /// the currently-active bindings.
    pub const fn keymap(&self) -> &narwhal_commands::keymap::Keymap {
        &self.keymap
    }

    /// L36 #4 v1: diagnostics collected the last time
    /// `Settings::keymap` was applied. Empty when the user-supplied
    /// overrides parsed cleanly.
    pub fn keymap_warnings(&self) -> &[String] {
        &self.keymap_warnings
    }

    /// Test helper: mutable handle to the tab list. Used by
    /// integration tests that pre-populate fields (notably the
    /// staged-mutation queue) before exercising a public path.
    #[doc(hidden)]
    pub fn tabs_mut(&mut self) -> &mut Vec<Tab> {
        &mut self.tabs
    }

    pub const fn active_tab(&self) -> usize {
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

    pub const fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    pub const fn focus(&self) -> Pane {
        self.focus
    }

    /// Tab index that owns the in-flight run.  Falls back to
    /// `active_tab` when no run is in progress (defensive default).
    pub(super) fn run_tab_index(&self) -> usize {
        self.run_tab.unwrap_or(self.active_tab)
    }

    /// Read-only accessor for the most recent layout regions computed
    /// during render. Used by tests to determine where to click.
    #[doc(hidden)]
    pub const fn last_layout(&self) -> &narwhal_tui::LayoutRegions {
        &self.last_layout
    }

    pub const fn mode(&self) -> Mode {
        self.vim.mode()
    }

    /// Read-only accessor for the connection wizard. Tests use this to
    /// assert that `:add` / `:url` / `:edit` open the wizard and that
    /// it carries the expected pre-filled state.
    #[doc(hidden)]
    pub const fn wizard(&self) -> Option<&crate::wizard::ConnectionWizard> {
        self.wizard.as_ref()
    }

    /// Read-only accessor for the saved-connections list (the
    /// in-memory mirror of `connections.toml`).
    #[doc(hidden)]
    pub fn connections(&self) -> &[narwhal_core::ConnectionConfig] {
        &self.connections.connections
    }

    /// Read-only accessor for the materialised sidebar items in their
    /// current display order. Tests use this to assert recency-first
    /// ordering without a real terminal render.
    #[doc(hidden)]
    pub fn sidebar_items_for_test(&self) -> &[SidebarItem] {
        &self.sidebar_items
    }

    /// Read-only accessor for the vim command buffer. Used by tests to
    /// assert prompt Tab-completion results.
    #[doc(hidden)]
    pub fn command_buffer(&self) -> &str {
        self.vim.command_buffer()
    }

    pub const fn is_running(&self) -> bool {
        self.running
    }

    pub const fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// Whether a debounced schema-refresh timer is currently pending.
    /// Useful in tests to verify that non-DDL statements don't schedule
    /// a refresh.
    #[doc(hidden)]
    pub const fn refresh_task(&self) -> Option<&tokio::task::AbortHandle> {
        self.refresh_task.as_ref()
    }

    pub const fn help_open(&self) -> bool {
        self.help_open
    }

    /// Whether the history modal is currently open.
    #[doc(hidden)]
    pub const fn history_is_open(&self) -> bool {
        self.history_state.is_some()
    }

    /// Read-only accessor for the history modal state (for tests).
    #[doc(hidden)]
    pub const fn history_state(&self) -> Option<&HistoryState> {
        self.history_state.as_ref()
    }

    /// Whether the row detail modal is currently open on the active tab.
    #[doc(hidden)]
    pub fn row_detail_is_open(&self) -> bool {
        self.tabs[self.active_tab].row_detail.is_some()
    }

    /// Whether the snippets modal is currently open.
    #[doc(hidden)]
    pub const fn snippets_modal_is_open(&self) -> bool {
        self.snippets_modal.is_some()
    }

    /// Read-only accessor for the snippets modal state (for tests).
    #[doc(hidden)]
    pub const fn snippets_modal(&self) -> Option<&SnippetsModal> {
        self.snippets_modal.as_ref()
    }

    /// Read-only accessor for the snippet store (for tests).
    #[doc(hidden)]
    pub const fn snippet_store(&self) -> &SnippetStore {
        &self.snippet_store
    }

    /// Read-only accessor for the row detail modal state (for tests).
    #[doc(hidden)]
    pub fn row_detail_state(&self) -> Option<&RowDetailState> {
        self.tabs[self.active_tab].row_detail.as_ref()
    }

    // open_help and other help/history/snippets modal handlers moved to
    // `core::modals` (L21).

    // editor_title_with_tabs moved to `core::tabs` (L21).
}
