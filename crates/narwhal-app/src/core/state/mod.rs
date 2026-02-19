//! Pure state types previously inlined in `core/mod.rs`. Each
//! sub-module owns one concept; nothing here mutates `AppCore`.

pub mod history;
pub mod modals;
pub mod process;
pub mod result;
pub mod session;
pub mod sidebar;
pub mod snippets_modal;
pub mod status;
pub mod tab;

pub use history::HistoryState;
pub use modals::ModalState;
pub use process::ProcessState;
pub use result::{
    CellEdit, CompletionState, EditorSearchState, JsonViewerState, ResultBundle, ResultSearch,
    ResultState, RowDetailState, RowSource,
};
pub use session::SessionState;
pub use sidebar::SidebarItem;
pub use snippets_modal::SnippetsModal;
pub use status::StatusBar;
pub use tab::{PendingPreviewState, Tab};
