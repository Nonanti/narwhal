//! Application runtime that wires drivers, configuration, modal input and
//! the terminal user interface together.

#![forbid(unsafe_code)]

pub mod app;
pub mod clipboard;
pub mod commands;
pub mod completion;
pub mod core;
pub mod ddl;
pub mod edit;
pub mod explain;
pub mod export;
pub mod registry;
pub mod run;
pub mod session;
pub mod terminal;
pub mod wizard;

pub use app::App;
pub use core::{AppCore, ResultState, StatusBar};
pub use registry::DriverRegistry;
pub use session::Session;
pub use terminal::TerminalGuard;
