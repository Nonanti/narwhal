//! Reusable widgets.

pub mod editor;
pub mod help;
pub mod history;
pub mod results;
pub mod sidebar;
pub mod wizard;

pub use editor::{
    editor_cursor_anchor, render_completion_popup, render_editor, CompletionHitRegions,
    CompletionItemView, CompletionPopupView, EditorBuffer, EditorSearchHighlight,
};
pub use help::{render_help_modal, CheatsheetEntry, CheatsheetSection, CHEATSHEET};
pub use history::{render_history_modal, HistoryModalState, HistoryRow};
pub use results::{
    compare_values, render_results, CellEditView, CellPopup, ExplainPlanLine, ResultDisplay,
    ResultHitRegions, ResultView, SearchHighlight, SortDir,
};
pub use sidebar::{render_sidebar, SchemaListing, SidebarRow, SidebarRowKind, SidebarView};
pub use wizard::{render_wizard, WizardFieldView, WizardView};
