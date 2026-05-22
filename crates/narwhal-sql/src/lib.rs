//! Statement-level splitting of SQL source text.
//!
//! The splitter is dialect-aware so that dialect-specific constructs such
//! as PostgreSQL dollar-quoted strings are not mistakenly cut in half.
//! It does not parse SQL; it only locates statement boundaries, which is
//! sufficient for routing each statement to the database driver
//! individually.

#![forbid(unsafe_code)]

pub mod formatter;
pub mod splitter;

pub use formatter::{format, format_for_driver};
pub use splitter::{split, split_with, Dialect, Statement};
