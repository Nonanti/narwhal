//! Pure domain models for narwhal. No IO, no rendering, no async.
//!
//! Each module owns one concept and only exposes data + synchronous
//! transitions. Hosts (TUI, CLI, MCP, commands crate) consume these
//! models by reference for rendering and route mutations through their
//! published constructor / mutator API.

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

pub mod editor;

pub use editor::EditorBuffer;
