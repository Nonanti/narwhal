//! Application runtime that wires drivers, configuration, modal input and
//! the terminal user interface together.

#![forbid(unsafe_code)]

pub mod app;
pub mod clipboard;
pub mod core;
pub mod draw_scheduler;
pub mod registry;
pub mod run;
pub mod terminal;

pub use app::App;
pub use core::{
    AppCore, HistoryState, ResultBundle, ResultState, RowDetailState, SnippetsModal, StatusBar,
};
pub use narwhal_commands::export::{write_format, ExportError, ExportFormat, QualifiedName};
pub use narwhal_commands::session::Session;
pub use narwhal_commands::snippets::{SnippetError, SnippetStore};
pub use registry::DriverRegistry;
pub use terminal::TerminalGuard;

// Re-export submodules so existing call sites (`crate::commands`,
// `crate::completion`, etc.) keep compiling while the migration to
// `narwhal_commands::*` continues incrementally.
pub use narwhal_commands::{
    cell_edit, commands, completion, ddl, explain, export, meta, session, snippets, statements,
    wizard,
};
