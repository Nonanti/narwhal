//! Application runtime that wires drivers, configuration, modal input and
//! the terminal user interface together.

#![forbid(unsafe_code)]
#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro,
    clippy::print_stdout,
    clippy::print_stderr
)]

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
pub use narwhal_commands::export::{ExportError, ExportFormat, QualifiedName, write_format};
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
