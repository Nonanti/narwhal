//! Tree-sitter SQL parser integration (T1-T3-A).
//!
//! Replaces the historical regex-only approach to syntax colouring with
//! a real CST per buffer. The CST powers three downstream features:
//!
//! 1. **Highlighting**: source-ordered [`HighlightSpan`]s that the TUI
//!    editor turns into ratatui `Style`s via the theme palette.
//! 2. **Scope detection**: given a byte offset, [`Scope`] tells the
//!    completion engine and (in v2.1) the LSP client whether the cursor
//!    lives inside a `WHERE` clause, the projection list, an
//!    `ORDER BY` clause, etc. This is the contract the Tier 2
//!    **T2-T3-C** (LSP) and **T2-T3-D** (multi-cursor) tasks build on
//!    — they consume [`Scope`] / [`ScopeKind`] only, never the raw
//!    [`tree_sitter::Tree`].
//! 3. **Incremental reparse**: edits are described as [`Edit`] values
//!    and fed back to the parser via [`Parser::reparse`], so a single
//!    keystroke in a 10k-line buffer never re-tokenises the whole
//!    file.
//!
//! ## Public surface in one screen
//!
//! ```text
//!  narwhal_sql::treesitter
//!  ├─ Parser              — one per editor buffer; not Send/Sync
//!  │  ├─ new() / new_with_grammar(Grammar)
//!  │  ├─ parse(src: &str) -> &SqlTree
//!  │  ├─ reparse(src: &str) -> &SqlTree
//!  │  ├─ edit(&Edit)
//!  │  └─ tree() -> Option<&SqlTree>
//!  ├─ SqlTree             — immutable view, cheap clones not provided
//!  │  ├─ highlights(src: &str) -> Vec<HighlightSpan>
//!  │  ├─ highlights_in_range(src, byte_range) -> Vec<HighlightSpan>
//!  │  └─ scope_at(src, byte_offset)         -> Scope
//!  ├─ HighlightSpan { byte_range, kind: HighlightKind }
//!  ├─ HighlightKind (Keyword, Identifier, String, Number, …)
//!  ├─ Scope { kind: ScopeKind, statement_byte_range, clause_byte_range }
//!  ├─ ScopeKind (Where, SelectProjection, From, OrderBy, …)
//!  └─ Edit { start_byte, old_end_byte, new_end_byte, … }
//! ```
//!
//! Every public struct is `#[non_exhaustive]` and every enum is
//! `#[non_exhaustive]` per the Tier-0 convention captured in
//! `docs/dev/api-surface.md`.
//!
//! ## Threading
//!
//! [`Parser`] is `!Send` because the underlying [`tree_sitter::Parser`]
//! and [`tree_sitter::Tree`] hold raw C pointers that cannot cross
//! threads (the C lib's allocator is per-instance). Each editor tab
//! should keep its own [`Parser`] on the UI thread. If a future
//! background-parse worker is needed the tree can be shared via the
//! `tree-sitter` crate's own `Tree::walk` cursor on the owning
//! thread; we don't expose a cross-thread snapshot API yet.

mod edit;
mod highlight;
mod parser;
mod scope;

#[cfg(test)]
mod tests;

pub use edit::Edit;
pub use highlight::{HighlightKind, HighlightSpan};
pub use parser::{Grammar, ParseError, Parser, SqlTree};
pub use scope::{Scope, ScopeKind};
