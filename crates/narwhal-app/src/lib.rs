//! Application runtime that wires drivers, configuration, modal input and
//! the terminal user interface together.

#![forbid(unsafe_code)]

pub mod app;
pub mod commands;
pub mod registry;
pub mod run;
pub mod session;
pub mod terminal;

pub use app::App;
pub use registry::DriverRegistry;
pub use session::Session;
pub use terminal::TerminalGuard;
