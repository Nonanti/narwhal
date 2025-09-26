//! Append-only query history journal.
//!
//! Each executed statement is serialised as a single JSON object on its own
//! line. The journal is opened in append mode so concurrent processes do not
//! corrupt each other's writes, and read access is provided as a streaming
//! iterator that never materialises the entire file at once.

#![forbid(unsafe_code)]

pub mod journal;

pub use journal::{HistoryEntry, HistoryError, Journal, JournalReader, Outcome};
