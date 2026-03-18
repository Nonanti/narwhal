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
        &self.ui.status.message
    }

    /// Read-only accessor for the full [`StatusBar`] struct.
    pub const fn status_bar(&self) -> &StatusBar {
        &self.ui.status
    }

    pub fn result(&self) -> &ResultState {
        self.tab().results.active_state()
    }

    /// Test helper: is the completion popup currently open on the
    /// active tab? Used by integration tests that drive the editor and
    /// want to assert auto-trigger fired without depending on a
    /// specific status-bar message.
    #[doc(hidden)]
    pub async fn editor_completion_is_open(&self) -> bool {
        self.ui.tabs[self.ui.active_tab].completion.is_some()
    }

    /// Test helper: borrow the JSON viewer modal state, if open. Lives
    /// here (alongside the other modal accessors) so integration tests
    /// don't have to plumb through the full `Tab` graph.
    #[doc(hidden)]
    pub fn json_viewer_for_test(&self) -> Option<&JsonViewerState> {
        self.ui.tabs[self.ui.active_tab].json_viewer.as_ref()
    }

    /// Test helper: borrow the diagram modal state on the active tab.
    #[doc(hidden)]
    pub fn diagram_for_test(&self) -> Option<&super::state::DiagramModalState> {
        self.ui.tabs[self.ui.active_tab].diagram.as_ref()
    }

    pub fn editor(&self) -> &EditorBuffer {
        &self.tab().editor
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.ui.tabs
    }

    /// L36 #4 v1: read-only access to the live keymap. Used by tests
    /// and any future help/config tooling that wants to introspect
    /// the currently-active bindings.
    pub const fn keymap(&self) -> &narwhal_commands::keymap::Keymap {
        &self.deps.keymap
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
        &mut self.ui.tabs
    }

    pub const fn active_tab(&self) -> usize {
        self.ui.active_tab
    }

    /// Borrow the active tab.
    ///
    /// Indexing is sound because two invariants are upheld at every
    /// `&mut self` entry point:
    /// - `self.ui.tabs` always contains at least one element. `close_tab`
    ///   early-returns when `tabs.len() == 1`.
    /// - `self.ui.active_tab < self.ui.tabs.len()`. `close_tab` clamps after
    ///   removal; `cycle_tab` uses `rem_euclid(len)`.
    ///
    /// See `active_tab_invariant_holds_after_close` in the test suite.
    fn tab(&self) -> &Tab {
        &self.ui.tabs[self.ui.active_tab]
    }

    /// Mutable counterpart to [`Self::tab`]. Same invariants apply.
    #[allow(dead_code)]
    fn tab_mut(&mut self) -> &mut Tab {
        &mut self.ui.tabs[self.ui.active_tab]
    }

    pub const fn session(&self) -> Option<&Session> {
        self.session.active.as_ref()
    }

    pub const fn focus(&self) -> Pane {
        self.ui.focus
    }

    /// Tab index that owns the in-flight run.  Falls back to
    /// `active_tab` when no run is in progress (defensive default).
    pub(super) async fn run_tab_index(&self) -> usize {
        self.process.run_tab.unwrap_or(self.ui.active_tab)
    }

    /// Read-only accessor for the most recent layout regions computed
    /// during render. Used by tests to determine where to click.
    #[doc(hidden)]
    pub const fn last_layout(&self) -> &narwhal_tui::LayoutRegions {
        &self.ui.last_layout
    }

    pub const fn mode(&self) -> Mode {
        self.ui.vim.mode()
    }

    /// Read-only accessor for the connection wizard. Tests use this to
    /// assert that `:add` / `:url` / `:edit` open the wizard and that
    /// it carries the expected pre-filled state.
    #[doc(hidden)]
    pub const fn wizard(&self) -> Option<&crate::wizard::ConnectionWizard> {
        self.modals.wizard.as_ref()
    }

    /// Read-only accessor for the saved-connections list (the
    /// in-memory mirror of `connections.toml`).
    #[doc(hidden)]
    pub fn connections(&self) -> &[narwhal_core::ConnectionConfig] {
        &self.session.connections.connections
    }

    /// Read-only accessor for the materialised sidebar items in their
    /// current display order. Tests use this to assert recency-first
    /// ordering without a real terminal render.
    #[doc(hidden)]
    pub fn sidebar_items_for_test(&self) -> &[SidebarItem] {
        &self.ui.sidebar_items
    }

    /// Test helper: borrow the UI state. Lets tests pick up the live
    /// sidebar items / focus / pending leaders without depending on
    /// the dispatcher's internal access pattern.
    #[doc(hidden)]
    pub const fn ui_for_test(&self) -> &super::state::UiState {
        &self.ui
    }

    /// Test helper: position the sidebar selection cursor.
    #[doc(hidden)]
    pub fn set_sidebar_index_for_test(&mut self, idx: usize) {
        self.ui.sidebar_index = idx;
    }

    /// Test helper: move keyboard focus to the sidebar pane (skips the
    /// usual `Ctrl-W` cycle that drives this in interactive use).
    #[doc(hidden)]
    pub fn set_focus_sidebar_for_test(&mut self) {
        self.ui.focus = narwhal_tui::Pane::Sidebar;
    }

    /// Read-only accessor for the vim command buffer. Used by tests to
    /// assert prompt Tab-completion results.
    #[doc(hidden)]
    pub fn command_buffer(&self) -> &str {
        self.ui.vim.command_buffer()
    }

    pub const fn is_running(&self) -> bool {
        self.process.running
    }

    pub const fn should_quit(&self) -> bool {
        self.process.should_quit
    }

    /// Whether a debounced schema-refresh timer is currently pending.
    /// Useful in tests to verify that non-DDL statements don't schedule
    /// a refresh.
    #[doc(hidden)]
    pub const fn refresh_task(&self) -> Option<&tokio::task::AbortHandle> {
        self.process.refresh_task.as_ref()
    }

    pub const fn help_open(&self) -> bool {
        self.modals.help_open
    }

    /// Whether the history modal is currently open.
    #[doc(hidden)]
    pub const fn history_is_open(&self) -> bool {
        self.modals.history.is_some()
    }

    /// Read-only accessor for the history modal state (for tests).
    #[doc(hidden)]
    pub const fn history_state(&self) -> Option<&HistoryState> {
        self.modals.history.as_ref()
    }

    /// Whether the row detail modal is currently open on the active tab.
    #[doc(hidden)]
    pub async fn row_detail_is_open(&self) -> bool {
        self.ui.tabs[self.ui.active_tab].row_detail.is_some()
    }

    /// Whether the snippets modal is currently open.
    #[doc(hidden)]
    pub const fn snippets_modal_is_open(&self) -> bool {
        self.modals.snippets.is_some()
    }

    /// Read-only accessor for the snippets modal state (for tests).
    #[doc(hidden)]
    pub const fn snippets_modal(&self) -> Option<&SnippetsModal> {
        self.modals.snippets.as_ref()
    }

    /// Read-only accessor for the snippet store (for tests).
    #[doc(hidden)]
    pub const fn snippet_store(&self) -> &SnippetStore {
        &self.session.snippet_store
    }

    /// Read-only accessor for the row detail modal state (for tests).
    #[doc(hidden)]
    pub fn row_detail_state(&self) -> Option<&RowDetailState> {
        self.ui.tabs[self.ui.active_tab].row_detail.as_ref()
    }

    // open_help and other help/history/snippets modal handlers moved to
    // `core::modals` (L21).

    // editor_title_with_tabs moved to `core::tabs` (L21).
}
