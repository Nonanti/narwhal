//! Highlight span emission.
//!
//! tree-sitter ships its own `.scm` query language, but for the
//! categories narwhal renders (keyword / identifier / string / number
//! / comment / operator / punctuation / table-ref / column-ref /
//! function-call) a direct node-kind classifier is simpler, faster
//! and avoids shipping a `.scm` file we'd have to maintain alongside
//! upstream grammar churn.
//!
//! The visitor walks the CST in source order and emits one
//! [`HighlightSpan`] per node it can classify. Overlapping spans
//! cannot happen because each node either delegates to its children
//! or terminates the recursion.
//!
//! Performance budget: 50 ms for a 10k-line whole-file pass (see
//! `t1-t3-a-treesitter.md`). The walk is O(nodes), the classifier is
//! a `match` on a short string — well inside the budget.

use std::ops::Range;

use tree_sitter::{Node, Tree, TreeCursor};

/// Classification of a single highlight span.
///
/// Keep this list tight — every new variant grows the theme palette
/// and the per-keystroke render hot path. Add a variant only when an
/// existing one would mis-style real code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HighlightKind {
    /// SQL keyword: `SELECT`, `FROM`, `WHERE`, `JOIN`, `AND`, …
    Keyword,
    /// String literal: `'hello'`, `E'a\\n'`, `$tag$body$tag$`.
    String,
    /// Numeric literal: `42`, `1.5`, `-3`, `1e6`.
    Number,
    /// Boolean / NULL literal: `TRUE`, `FALSE`, `NULL`. Visually
    /// distinct from arbitrary keywords because most themes paint
    /// these alongside numbers ("constants").
    Constant,
    /// Single-line `-- comment`.
    LineComment,
    /// Block `/* comment */`.
    BlockComment,
    /// Operator: `+`, `-`, `*`, `=`, `>=`, `||`, …
    Operator,
    /// Punctuation: `,`, `;`, `(`, `)`, `[`, `]`.
    Punctuation,
    /// Function invocation: the function name in `COUNT(*)`.
    FunctionCall,
    /// Table reference: identifier inside `FROM` / `JOIN` /
    /// `UPDATE` / `INSERT INTO` / `DELETE FROM`.
    TableRef,
    /// Column reference: identifier appearing in a `field` position
    /// (projection, predicate, `ORDER BY`, …).
    ColumnRef,
    /// Alias introduced by `AS` or implicit `FROM t alias`.
    Alias,
    /// Type name in a `CREATE TABLE` / `CAST` context.
    Type,
    /// Identifier that we could classify but no more specifically.
    Identifier,
    /// Syntactically invalid region. The grammar emits `(ERROR)`
    /// nodes around fragments it couldn't parse; surfacing them lets
    /// the editor draw a subtle underline.
    Error,
}

/// One span of source text with an associated kind.
///
/// Spans never overlap and are returned in ascending `byte_range`
/// order. The renderer can apply them in a single pass.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HighlightSpan {
    /// Byte range in the source the span covers. End-exclusive.
    pub byte_range: Range<usize>,
    /// What to paint.
    pub kind: HighlightKind,
}

impl HighlightSpan {
    #[must_use]
    pub const fn new(byte_range: Range<usize>, kind: HighlightKind) -> Self {
        Self { byte_range, kind }
    }

    /// Convenience used by the editor when laying out per-line
    /// styles: span length in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.byte_range.end.saturating_sub(self.byte_range.start)
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.byte_range.end <= self.byte_range.start
    }
}

pub(super) fn highlights_for_range(
    tree: &Tree,
    source: &str,
    byte_range: Range<usize>,
) -> Vec<HighlightSpan> {
    // Heuristic: one span per ~6 source bytes on dense SQL. Avoids
    // the geometric reallocations the Vec would otherwise do on a
    // whole-file walk over a 10k-line buffer.
    let cap = (byte_range.end - byte_range.start) / 6;
    let mut out = Vec::with_capacity(cap);
    let root = tree.root_node();
    let mut cursor = root.walk();
    walk(&mut cursor, source, &byte_range, &mut out);
    out
}

/// Recursive (loop-based, via `TreeCursor`) source-order walk. Each
/// node either:
///
/// - is classified — emit a span, do *not* recurse,
/// - is structural — recurse into children,
/// - is irrelevant — skip.
fn walk(
    cursor: &mut TreeCursor<'_>,
    source: &str,
    range: &Range<usize>,
    out: &mut Vec<HighlightSpan>,
) {
    loop {
        let node = cursor.node();
        if intersects(&node.byte_range(), range) {
            // `field_name` is only consulted for `identifier` nodes;
            // skipping the FFI call for the other 99 % of nodes is a
            // measurable win on whole-file passes.
            let field_name = if node.kind() == "identifier" {
                cursor.field_name()
            } else {
                None
            };
            if let Some(kind) = classify(node, field_name, source) {
                push_span(out, node.byte_range(), kind);
            } else if cursor.goto_first_child() {
                continue;
            }
        }
        // Move to next sibling, or pop up while we can.
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

const fn intersects(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

/// Push `(range, kind)` while preserving the no-overlap invariant.
/// In practice the walker only emits leaf spans so overlap is
/// impossible, but the asserting append is cheap insurance.
fn push_span(out: &mut Vec<HighlightSpan>, range: Range<usize>, kind: HighlightKind) {
    if range.start >= range.end {
        return;
    }
    if let Some(last) = out.last() {
        if last.byte_range.end > range.start {
            // Overlap: trust the later (deeper) classifier. Drop the
            // earlier span entirely if fully covered; otherwise trim.
            // This only fires under highly malformed input where the
            // grammar nested an `ERROR` node inside another classified
            // leaf — not worth panicking the editor over.
            if last.byte_range.start >= range.start {
                out.pop();
            } else {
                let mut trimmed = out.pop().unwrap_or_else(|| unreachable!());
                trimmed.byte_range.end = range.start;
                if !trimmed.is_empty() {
                    out.push(trimmed);
                }
            }
        }
    }
    out.push(HighlightSpan::new(range, kind));
}

/// Map a node to a [`HighlightKind`] or `None` to recurse.
///
/// The classification mirrors the node-kind taxonomy of
/// `tree-sitter-sequel` empirically (see the doctests in
/// `tests.rs`). Anonymous nodes (operators / punctuation) are
/// matched on their raw text.
fn classify(node: Node<'_>, field_name: Option<&str>, source: &str) -> Option<HighlightKind> {
    let kind = node.kind();

    // Anonymous nodes: classify by exact source text. They never have
    // children, so emitting a span here is always a leaf.
    if !node.is_named() {
        let text = node_text(node, source);
        return classify_anonymous(text);
    }

    // Named nodes.
    match kind {
        // Comments.
        "comment" => Some(HighlightKind::LineComment),
        "marginalia" => Some(HighlightKind::BlockComment),

        // Errors first — outranks any inner classification because
        // we want the underline to span the broken region.
        "ERROR" => Some(HighlightKind::Error),

        // Keywords cover every `keyword_*` node.
        k if k.starts_with("keyword_") => {
            // `keyword_null` / `keyword_true` / `keyword_false` paint
            // as Constants in most themes; the rest are real
            // keywords.
            if matches!(k, "keyword_null" | "keyword_true" | "keyword_false") {
                Some(HighlightKind::Constant)
            } else {
                Some(HighlightKind::Keyword)
            }
        }

        // Literals: distinguish string vs number by the first byte.
        // tree-sitter-sequel wraps booleans / NULL in a `literal`
        // with a `keyword_*` child — those will be classified by
        // the keyword arm when we recurse into the literal, so this
        // arm only fires for childless / leaf-style literals.
        "literal" => {
            if node.named_child_count() > 0 {
                // Has `keyword_null` etc. as a named child — let
                // the recursion paint the inner keyword.
                return None;
            }
            let text = node_text(node, source);
            Some(classify_literal_text(text))
        }

        // Structural — recurse so the inner identifier gets painted.
        "object_reference" => None,

        "identifier" => Some(identifier_kind(node, field_name)),

        // Structural nodes — recurse.
        _ => None,
    }
}

/// Classify an `identifier` based on the field-name edge from its
/// parent and the parent / grand-parent kinds.
fn identifier_kind(node: Node<'_>, field_name: Option<&str>) -> HighlightKind {
    if field_name == Some("alias") {
        return HighlightKind::Alias;
    }
    let parent = match node.parent() {
        Some(p) => p,
        None => return HighlightKind::Identifier,
    };
    match parent.kind() {
        "object_reference" => {
            let grand = parent.parent();
            match grand.map(|g| g.kind()) {
                Some("relation") => HighlightKind::TableRef,
                Some("invocation") => HighlightKind::FunctionCall,
                Some("field") => HighlightKind::TableRef, // qualifier in `t.col`
                Some("from") => HighlightKind::TableRef,
                Some("insert" | "update" | "delete") => HighlightKind::TableRef,
                Some("create_table" | "create_view" | "create_index") => HighlightKind::TableRef,
                _ => HighlightKind::Identifier,
            }
        }
        "field" => HighlightKind::ColumnRef,
        "column" => HighlightKind::ColumnRef,
        "column_definition" => HighlightKind::ColumnRef,
        "cte" => HighlightKind::TableRef,
        _ => HighlightKind::Identifier,
    }
}

fn node_text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    let range = node.byte_range();
    source.get(range).unwrap_or("")
}

fn classify_literal_text(text: &str) -> HighlightKind {
    let bytes = text.as_bytes();
    let first = bytes.first().copied().unwrap_or(0);
    match first {
        b'\'' | b'"' | b'$' => HighlightKind::String,
        b'0'..=b'9' => HighlightKind::Number,
        b'-' | b'+' if bytes.get(1).is_some_and(|c| c.is_ascii_digit()) => HighlightKind::Number,
        b'.' if bytes.get(1).is_some_and(|c| c.is_ascii_digit()) => HighlightKind::Number,
        _ => HighlightKind::Identifier,
    }
}

/// Operators / punctuation from anonymous nodes. Returning `None`
/// means "skip" — e.g. surrounding whitespace tokens.
fn classify_anonymous(text: &str) -> Option<HighlightKind> {
    if text.is_empty() {
        return None;
    }
    match text {
        "," | ";" | "(" | ")" | "[" | "]" => Some(HighlightKind::Punctuation),
        _ => {
            // Treat any anonymous node whose first byte is in the
            // operator set as an operator. This deliberately accepts
            // multi-char operators like `>=`, `<>`, `||`.
            let first = text.as_bytes()[0];
            if matches!(
                first,
                b'+' | b'-'
                    | b'*'
                    | b'/'
                    | b'%'
                    | b'='
                    | b'<'
                    | b'>'
                    | b'!'
                    | b'|'
                    | b'&'
                    | b'^'
                    | b'~'
            ) {
                Some(HighlightKind::Operator)
            } else {
                None
            }
        }
    }
}
