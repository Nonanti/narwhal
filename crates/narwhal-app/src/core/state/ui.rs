//! Terminal-UI state.
//!
//! Everything visible on screen at idle (tabs, focus pane, sidebar
//! listing, theme, status bar, last frame layout) plus the
//! short-lived input-routing state that the editor and results
//! handlers share (pending leader keys, in-flight result bundles
//! that have not yet been promoted into a tab).
//!
//! UI state owns the *appearance* of the app. It deliberately
//! contains no data the app would survive a redraw without:
//! session, connections, history, and modal overlays live in
//! their own sub-states.

use narwhal_config::DiagramIcons;
use narwhal_tui::{LayoutRegions, Pane, ResultView, Theme};

use super::{ResultState, SidebarItem, StatusBar, Tab};

/// Visible-on-screen state. Mutated by the dispatcher, render
/// helpers, and the run-loop's per-tick `RunUpdate` handler.
pub struct UiState {
    /// Editor / Results tabs. The user can have several open at
    /// once and cycle between them with `gt` / `gT` (vim) or
    /// `Ctrl-Tab` (default keymap).
    pub tabs: Vec<Tab>,
    /// Index into `tabs` for the currently-focused tab. Reads
    /// during a run go through `process.run_tab` instead so
    /// mid-run tab switches do not scribble into the wrong tab.
    pub active_tab: usize,
    /// Monotonic id source for new tabs. Stable handles (the
    /// `Tab::id()` value) survive index shuffling, which is the
    /// C5 invariant: meta updates carry the id, not the index.
    pub next_tab_id: usize,
    /// Cursor and motion state for vim-flavoured navigation. One
    /// `Vim` instance covers all tabs; the active editor is the
    /// `tabs[active_tab].editor`.
    pub vim: narwhal_vim::Vim,
    /// Colour palette. Driven by `config.toml [theme]` and the
    /// CLI `--theme` flag (high-contrast, default).
    pub theme: Theme,
    /// Which pane currently owns keystrokes when no modal is open.
    /// Editor / Sidebar / Results. The modal layer routes input
    /// before this is consulted.
    pub focus: Pane,
    /// Flattened sidebar listing (one row per connection, schema,
    /// table). Rebuilt on `:open`, schema refresh, and connection
    /// add/remove.
    pub sidebar_items: Vec<SidebarItem>,
    /// Selected row in the sidebar listing. Driven by Up/Down and
    /// by jump-to-symbol commands.
    pub sidebar_index: usize,
    /// Sidebar viewport scroll. First visible row index. (L24)
    pub sidebar_scroll: usize,
    /// One-line status display at the bottom of the screen.
    /// Spinners, last-error, command-prompt echo all funnel here.
    pub status: StatusBar,
    /// Pending leader key for result-tab cycling. `]` then `r`
    /// cycles forward; `[` then `r` cycles backward. Any other
    /// key clears the pending leader.
    pub pending_result_leader: Option<char>,
    /// Pending leader key on the sidebar pane. Currently used for
    /// the `gd` chord ("goto diagram") that opens the Focused diagram
    /// modal for the selected table. Cleared by any non-matching key
    /// so it never traps the user.
    pub pending_sidebar_leader: Option<char>,
    /// Collects per-statement results during a multi-statement
    /// batch. Populated by `finalize_statement`; consumed and
    /// turned into a `ResultBundle` by the `AllDone` handler.
    pub pending_result_entries_states: Vec<ResultState>,
    /// Parallel array to `pending_result_entries_states` carrying
    /// the matching `ResultView`s (sort, filter, column widths).
    pub pending_result_entries_views: Vec<ResultView>,
    /// Last frame's layout regions. Stored so non-render code
    /// (mouse hit-testing, viewport jump-to-cursor) can find pane
    /// rectangles without rerunning the layout algorithm.
    pub last_layout: LayoutRegions,
    /// Glyph set used when the diagram modal opens. Resolved from
    /// `[diagram].icons` in `apply_settings`; defaults to `Ascii`
    /// so terminals without a Nerd Font never see broken glyphs.
    pub diagram_icons: DiagramIcons,
}

impl UiState {
    /// Construct a fresh `UiState` with one untitled tab open,
    /// the editor in focus, and an empty sidebar / status.
    pub fn new() -> Self {
        Self {
            tabs: vec![Tab::new(1, "untitled-1")],
            active_tab: 0,
            next_tab_id: 2,
            vim: narwhal_vim::Vim::new(),
            theme: Theme::default(),
            focus: Pane::Editor,
            sidebar_items: Vec::new(),
            sidebar_index: 0,
            sidebar_scroll: 0,
            status: StatusBar {
                message: "ready".into(),
                ..Default::default()
            },
            pending_result_leader: None,
            pending_sidebar_leader: None,
            pending_result_entries_states: Vec::new(),
            pending_result_entries_views: Vec::new(),
            last_layout: LayoutRegions::default(),
            diagram_icons: DiagramIcons::default(),
        }
    }
}

impl Default for UiState {
    fn default() -> Self {
        Self::new()
    }
}
