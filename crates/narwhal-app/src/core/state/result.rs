//! Result-pane state shim.
//!
//! The structs that used to live here moved to
//! `narwhal_domain::result::state` (Faz 1 Madde 3, Adım 4d). This file
//! now re-exports them so every existing
//! `crate::core::state::result::*` / `crate::ResultBundle` import path
//! keeps working unchanged. `DiagramModalState` and `DiagramMode` stay
//! in `super::diagram_modal` because they pull `narwhal-diagram` types
//! that domain cannot name without inverting the dependency edge.

pub use narwhal_domain::result::state::{
    CellEdit, CompletionState, EditorSearchState, JsonViewerState, ResultBundle, ResultSearch,
    ResultState, RowDetailState, RowSource,
};
