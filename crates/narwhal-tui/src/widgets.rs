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

pub mod chart;
pub mod confirm;
pub mod context_menu;
pub mod diagram;
pub mod editor;
pub mod goto;
pub mod settings_modal;
pub mod help;
pub mod history;
pub mod json_viewer;
pub mod pending_preview;
pub mod pivot;
pub mod results;
pub mod row_detail;
pub mod sidebar;
pub mod snippets;
pub mod wizard;

pub use chart::{
    ChartPlaceholder, ChartView, ChartViewKind, render_chart, render_chart_placeholder,
};
pub use confirm::{ConfirmModalView, render_confirm_modal};
pub use context_menu::{ContextMenuItemView, ContextMenuView, render_context_menu};
pub use settings_modal::{SettingsModalView, render_settings_modal};
pub use diagram::{DiagramView, DiagramViewMode, render_diagram};
pub use editor::{
    CompletionHitRegions, editor_cursor_anchor, gutter_width, render_completion_popup,
    render_editor,
};
pub use goto::{GotoModalView, GotoRowView, render_goto_modal};
pub use help::{
    CHEATSHEET, CHEATSHEET_BASIC_EDITOR, CHEATSHEET_EMACS_EDITOR, CheatsheetEntry,
    CheatsheetSection, HelpEditorMode, render_help_modal,
};
pub use history::{HistoryModalState, HistoryRow, HistoryRowOutcome, render_history_modal};
pub use json_viewer::{JsonViewerView, render_json_viewer};
pub use narwhal_domain::editor::{
    CompletionItemView, CompletionPopupView, EditorBuffer, EditorSearchHighlight,
};
pub use pending_preview::{PendingPreviewView, render_pending_preview};
pub use pivot::{PivotPlaceholder, PivotTableView, render_pivot, render_pivot_placeholder};
pub use results::{
    CellEditView, CellPopup, ExplainPlanLine, MetaTab, ResultDisplay, ResultHitRegions, ResultView,
    SearchHighlight, SortDir, compare_values, render_results, sanitize_for_display,
};
pub use row_detail::{RowDetailView, render_row_detail};
pub use sidebar::{SchemaListing, SidebarRow, SidebarRowKind, SidebarView, render_sidebar};
pub use snippets::{SnippetsModalState, render_snippets_modal};
pub use wizard::{WizardFieldView, WizardView, render_wizard};
