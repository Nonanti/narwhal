//! Reusable widgets.

use ratatui::layout::Rect;

/// Centre `(width × height)` inside `area`. Used by every modal that
/// renders as a centred popup. Lived in four widget files before L25.
pub(crate) fn centred_rect(area: Rect, width: u16, height: u16) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

pub mod editor;
pub mod help;
pub mod history;
pub mod results;
pub mod row_detail;
pub mod sidebar;
pub mod snippets;
pub mod wizard;

pub use editor::{
    editor_cursor_anchor, render_completion_popup, render_editor, CompletionHitRegions,
};
pub use help::{render_help_modal, CheatsheetEntry, CheatsheetSection, CHEATSHEET};
pub use history::{render_history_modal, HistoryModalState, HistoryRow};
pub use narwhal_domain::editor::{
    CompletionItemView, CompletionPopupView, EditorBuffer, EditorSearchHighlight,
};
pub use results::{
    compare_values, render_results, sanitize_for_display, CellEditView, CellPopup, ExplainPlanLine,
    ResultDisplay, ResultHitRegions, ResultView, SearchHighlight, SortDir,
};
pub use row_detail::{render_row_detail, RowDetailView};
pub use sidebar::{render_sidebar, SchemaListing, SidebarRow, SidebarRowKind, SidebarView};
pub use snippets::{render_snippets_modal, SnippetsModalState};
pub use wizard::{render_wizard, WizardFieldView, WizardView};
