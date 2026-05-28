//! [`Parser`] — thin facade around [`tree_sitter::Parser`].
//!
//! One [`Parser`] per editor buffer. The previous parse is cached so
//! [`Parser::reparse`] can run incrementally after a sequence of
//! [`Parser::edit`] calls.

use std::fmt;

use tree_sitter::Tree;

use super::edit::Edit;
use super::highlight::{HighlightSpan, highlights_for_range};
use super::scope::{Scope, scope_at_offset};

/// Grammar variant to load. Only [`Grammar::Generic`] is wired today;
/// future Tier-2 dialect grammars (Postgres-specific, MSSQL-specific)
/// can be added without breaking existing code thanks to
/// `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Grammar {
    /// `tree-sitter-sequel` — the generic SQL grammar (`Postgres` /
    /// `MySQL` / `SQLite` / `MSSQL` all parse acceptably; some dialect
    /// extensions degrade to `(ERROR)` nodes without breaking the
    /// surrounding tree).
    #[default]
    Generic,
}

/// Why a parser couldn't be built or a parse failed.
///
/// `narwhal-sql` policy bans `unwrap` / `expect` in production paths,
/// so the few fallible tree-sitter operations are mapped to this enum
/// and surfaced to callers.
#[derive(Debug)]
#[non_exhaustive]
pub enum ParseError {
    /// `tree_sitter::Parser::set_language` rejected the grammar.
    /// This is a build-time mismatch (e.g. `tree-sitter` and
    /// `tree-sitter-sequel` ABI versions drifted apart) and indicates
    /// a packaging bug rather than user error.
    GrammarMismatch(tree_sitter::LanguageError),
    /// The parser produced no tree. tree-sitter only returns `None`
    /// when no language is set (already handled) or a parse timeout
    /// was hit (we don't set one); in practice this is unreachable but
    /// kept so callers don't have to `unwrap`.
    NoTree,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GrammarMismatch(err) => {
                write!(f, "tree-sitter rejected the SQL grammar: {err}")
            }
            Self::NoTree => f.write_str("tree-sitter returned no tree"),
        }
    }
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::GrammarMismatch(err) => Some(err),
            Self::NoTree => None,
        }
    }
}

/// One parser per editor buffer.
///
/// Not `Send` — the inner [`tree_sitter::Parser`] holds a raw C
/// pointer. Each tab should keep its own instance on the UI thread.
pub struct Parser {
    inner: tree_sitter::Parser,
    tree: Option<SqlTree>,
}

impl fmt::Debug for Parser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The inner `tree_sitter::Parser` doesn't implement `Debug`,
        // and the cached tree is large and opaque — dump only the
        // bit a user can act on (whether we hold a tree at all).
        f.debug_struct("Parser")
            .field("has_tree", &self.tree.is_some())
            .finish_non_exhaustive()
    }
}

impl Parser {
    /// Build a parser using the default grammar ([`Grammar::Generic`]).
    pub fn new() -> Result<Self, ParseError> {
        Self::new_with_grammar(Grammar::Generic)
    }

    /// Build a parser using `grammar`.
    pub fn new_with_grammar(grammar: Grammar) -> Result<Self, ParseError> {
        let mut inner = tree_sitter::Parser::new();
        let language: tree_sitter::Language = match grammar {
            Grammar::Generic => tree_sitter_sequel::LANGUAGE.into(),
        };
        inner
            .set_language(&language)
            .map_err(ParseError::GrammarMismatch)?;
        Ok(Self { inner, tree: None })
    }

    /// Parse `source` from scratch, discarding any cached tree.
    /// Use when the buffer is replaced wholesale (file open,
    /// `:reload`).
    pub fn parse(&mut self, source: &str) -> Result<&SqlTree, ParseError> {
        let tree = self.inner.parse(source, None).ok_or(ParseError::NoTree)?;
        self.tree = Some(SqlTree { inner: tree });
        // Safe: we just set it.
        self.tree.as_ref().ok_or(ParseError::NoTree)
    }

    /// Apply a buffer edit to the cached tree. Must be followed by
    /// [`Parser::reparse`] with the post-edit source before any
    /// queries are issued; otherwise the cached tree is out of sync.
    ///
    /// Returns `false` (silently) if there is no cached tree —
    /// the caller should fall back to [`Parser::parse`].
    pub fn edit(&mut self, edit: &Edit) -> bool {
        match self.tree.as_mut() {
            Some(t) => {
                t.inner.edit(&(*edit).into_input_edit());
                true
            }
            None => false,
        }
    }

    /// Incremental reparse. The cached (edited) tree is reused so the
    /// new parse only re-tokenises the changed region. If no tree is
    /// cached, falls back to a from-scratch parse.
    pub fn reparse(&mut self, new_source: &str) -> Result<&SqlTree, ParseError> {
        let old = self.tree.as_ref().map(|t| &t.inner);
        let tree = self
            .inner
            .parse(new_source, old)
            .ok_or(ParseError::NoTree)?;
        self.tree = Some(SqlTree { inner: tree });
        self.tree.as_ref().ok_or(ParseError::NoTree)
    }

    /// Borrow the cached tree, if any.
    #[must_use]
    pub const fn tree(&self) -> Option<&SqlTree> {
        self.tree.as_ref()
    }
}

/// Parsed SQL buffer. Cheap to keep around; expensive to clone (it
/// would require a tree-sitter `Tree::clone`, intentionally not
/// exposed here — callers that need a snapshot for a worker thread
/// should hold the underlying `&tree_sitter::Tree` via `raw`).
pub struct SqlTree {
    pub(super) inner: Tree,
}

impl fmt::Debug for SqlTree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqlTree")
            .field("root_kind", &self.inner.root_node().kind())
            .field("byte_range", &self.inner.root_node().byte_range())
            .finish()
    }
}

impl SqlTree {
    /// Whole-buffer highlight spans, in source order.
    #[must_use]
    pub fn highlights(&self, source: &str) -> Vec<HighlightSpan> {
        highlights_for_range(&self.inner, source, 0..source.len())
    }

    /// Highlight spans intersecting `byte_range`. Use this from the
    /// renderer to skip work on off-screen lines.
    #[must_use]
    pub fn highlights_in_range(
        &self,
        source: &str,
        byte_range: std::ops::Range<usize>,
    ) -> Vec<HighlightSpan> {
        highlights_for_range(&self.inner, source, byte_range)
    }

    /// Classify the SQL clause that contains `byte_offset`.
    #[must_use]
    pub fn scope_at(&self, source: &str, byte_offset: usize) -> Scope {
        scope_at_offset(&self.inner, source, byte_offset)
    }

    /// Borrow the underlying tree-sitter tree. Intended for the small
    /// set of advanced callers (e.g. the future multi-cursor
    /// task) that need to walk the CST directly. Most callers should
    /// stick to [`highlights`](Self::highlights) /
    /// [`scope_at`](Self::scope_at).
    #[must_use]
    pub const fn raw(&self) -> &Tree {
        &self.inner
    }

    /// Convenience: S-expression of the root node, useful in tests
    /// and `:dump` style introspection.
    #[must_use]
    pub fn sexp(&self) -> String {
        self.inner.root_node().to_sexp()
    }
}
