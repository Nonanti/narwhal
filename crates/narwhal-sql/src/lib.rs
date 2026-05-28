//! Statement-level splitting of SQL source text.
//!
//! The splitter is dialect-aware so that dialect-specific constructs such
//! as `PostgreSQL` dollar-quoted strings are not mistakenly cut in half.
//! It does not parse SQL; it only locates statement boundaries, which is
//! sufficient for routing each statement to the database driver
//! individually.

#![forbid(unsafe_code)]

pub mod formatter;
pub mod guard;
pub mod lint;
pub mod splitter;
// tree-sitter SQL parser. Provides a CST per buffer plus
// incremental reparse, [highlight] spans, and [scope] detection used
// by the editor for syntax colouring and (in v2.1) the LSP completion
// engine.
pub mod treesitter;

pub use formatter::{format, format_for_driver};
pub use guard::{StatementKind, classify_statement, guard_read_only};
pub use lint::{LintFinding, LintSeverity, lint, lint_with_dialect};
pub use splitter::{Dialect, Statement, split, split_with};
