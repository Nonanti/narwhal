//! Append-only query history journal.
//!
//! Each executed statement is serialised as a single JSON object on its own
//! line. The journal is opened in append mode so concurrent processes do not
//! corrupt each other's writes, and read access is provided as a streaming
//! iterator that never materialises the entire file at once.

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

pub mod journal;

pub use journal::{HistoryEntry, HistoryError, Journal, JournalReader, Outcome};
