//! Application runtime that wires drivers, configuration, modal input and
//! the terminal user interface together.

#![forbid(unsafe_code)]

pub mod app;
pub mod registry;
pub mod terminal;

pub use app::App;
pub use registry::DriverRegistry;
pub use terminal::TerminalGuard;
