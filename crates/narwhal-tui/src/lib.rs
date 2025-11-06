//! Terminal user interface built on top of `ratatui`.

#![forbid(unsafe_code)]

pub mod input;
pub mod layout;
pub mod theme;
pub mod widgets;

pub use input::translate_key_event;
pub use layout::{render_root, LayoutRegions, Pane, RootLayout, StatusBarView};
pub use theme::Theme;
pub use widgets::{
    editor_cursor_anchor, render_completion_popup, render_editor, render_help_modal,
    render_history_modal, render_results, render_row_detail, render_sidebar, render_wizard,
    CellEditView, CellPopup, CheatsheetEntry, CheatsheetSection, CompletionHitRegions,
    CompletionItemView, CompletionPopupView, EditorBuffer, EditorSearchHighlight, ExplainPlanLine,
    HistoryModalState, HistoryRow, ResultDisplay, ResultHitRegions, ResultView, RowDetailView,
    SchemaListing, SearchHighlight, SidebarRow, SidebarRowKind, SidebarView, SortDir,
    WizardFieldView, WizardView, CHEATSHEET,
};
