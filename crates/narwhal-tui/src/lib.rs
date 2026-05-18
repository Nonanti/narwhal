//! Terminal user interface built on top of `ratatui`.

#![forbid(unsafe_code)]

pub mod input;
pub mod layout;
pub mod theme;
pub mod widgets;

pub use input::translate_key_event;
pub use layout::{render_root, Pane, RootLayout};
pub use theme::Theme;
pub use widgets::{
    editor_cursor_anchor, render_completion_popup, render_editor, render_results, render_sidebar,
    render_wizard, CellEditView, CellPopup, CompletionItemView, CompletionPopupView, EditorBuffer,
    ExplainPlanLine, ResultDisplay, ResultView, SchemaListing, SearchHighlight, SidebarRow,
    SidebarRowKind, SidebarView, WizardFieldView, WizardView,
};
