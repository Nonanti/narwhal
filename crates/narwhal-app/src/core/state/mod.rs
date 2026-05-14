//! Pure state types previously inlined in `core/mod.rs`. Each
//! sub-module owns one concept; nothing here mutates `AppCore`.

pub mod deps;
pub mod diagram_modal;
pub mod goto_modal;
pub mod history;
pub mod modals;
pub mod process;
pub mod result;
pub mod session;
pub mod sidebar;
pub mod snippets_modal;
pub mod status;
pub mod tab;
pub mod ui;

pub use deps::AppDeps;
pub use diagram_modal::{DiagramModalState, DiagramMode};
pub use goto_modal::{GotoEntry, GotoMatch, GotoModal};
pub use history::HistoryState;
pub use modals::{ConfirmModal, ModalState, PendingConfirm, SettingsModal};
pub use process::ProcessState;
pub use result::{
    CellEdit, CompletionState, EditorSearchState, JsonViewerState, ResultBundle, ResultSearch,
    ResultState, RowDetailState, RowSource,
};
pub use session::{GotoCorpusCache, SessionState};
pub use sidebar::SidebarItem;
pub use snippets_modal::SnippetsModal;
pub use status::StatusBar;
pub use tab::{PendingPreviewState, Tab};
pub use ui::UiState;
