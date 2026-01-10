//! Application runtime that wires drivers, configuration, modal input and
//! the terminal user interface together.

#![forbid(unsafe_code)]

pub mod app;
pub mod clipboard;
pub mod commands;
pub mod completion;
pub mod core;
pub mod ddl;
pub mod draw_scheduler;
pub mod edit;
pub mod editor;
pub mod explain;
pub mod export;
pub mod meta;
pub mod registry;
pub mod run;
pub mod session;
pub mod snippets;
pub mod terminal;
pub mod wizard;

pub use app::App;
pub use core::{
    AppCore, HistoryState, ResultBundle, ResultState, RowDetailState, SnippetsModal, StatusBar,
};
pub use export::{ExportError, ExportFormat, QualifiedName};
pub use registry::DriverRegistry;
pub use session::Session;
pub use snippets::{SnippetError, SnippetStore};
pub use terminal::TerminalGuard;
