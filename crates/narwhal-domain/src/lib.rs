//! Pure domain models for narwhal. No IO, no rendering, no async.
//!
//! Each module owns one concept and only exposes data + synchronous
//! transitions. Hosts (TUI, CLI, MCP, commands crate) consume these
//! models by reference for rendering and route mutations through their
//! published constructor / mutator API.

#![forbid(unsafe_code)]

pub mod editor;
pub mod motion;
pub mod schema;

pub use editor::EditorBuffer;
pub use motion::Motion;
pub use schema::SchemaListing;
