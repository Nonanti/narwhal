//! narwhal-app — orchestrates terminal, drivers, vim state, and UI.

pub mod app;
pub mod registry;
pub mod terminal;

pub use app::App;
pub use registry::DriverRegistry;
