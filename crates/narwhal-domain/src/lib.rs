//! Pure domain models for narwhal. No IO, no rendering, no async.
//!
//! Each module owns one concept and only exposes data + synchronous
//! transitions. Hosts (TUI, CLI, MCP, commands crate) consume these
//! models by reference for rendering and route mutations through their
//! published constructor / mutator API.

#![forbid(unsafe_code)]

pub mod editor;
pub mod motion;
pub mod relation;
pub mod result;
pub mod schema;

pub use editor::{
    BufferSnapshot, EditHistory, EditOp, EditorBuffer, Position, Selection, SelectionKind,
};
pub use motion::Motion;
pub use relation::{Cardinality, LogicalRelation, QualifiedName};
pub use result::{
    CellEdit, CellEditView, CellPopup, CompletionState, EditorSearchState, ExplainPlanLine,
    JsonViewerState, MetaTab, ResultBundle, ResultSearch, ResultState, ResultView, RowDetailState,
    RowSource, SortDir, compare_values,
};

pub mod completion;
pub use completion::{Completion, CompletionKind};

pub mod export;

pub mod history;
pub use history::HistoryState;

pub mod sidebar;
pub use sidebar::SidebarItem;

pub mod status;
pub use status::{Notification, StatusBar};
pub use schema::SchemaListing;
