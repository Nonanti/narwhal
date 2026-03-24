//! Cursor-position scope detection.
//!
//! Given a byte offset inside a parsed SQL buffer, return a [`Scope`]
//! describing the nearest meaningful clause. This is the contract the
//! Tier-2 LSP (T2-T3-C) and multi-cursor (T2-T3-D) tasks build on top
//! of — they only need the classification, not the raw CST.
//!
//! ## Algorithm
//!
//! 1. Find the smallest named node whose byte range contains the
//!    offset (`Node::descendant_for_byte_range`).
//! 2. Walk upwards, mapping each ancestor's kind to a [`ScopeKind`].
//!    The first match wins, which is naturally the *innermost*
//!    clause.
//! 3. Record the enclosing statement's byte range so downstream
//!    code can scope its work (e.g. completion limits itself to
//!    the current statement's CTEs).
//!
//! ## Ambiguity
//!
//! `GROUP BY ... HAVING ...` shares a single `group_by` parent in
//! tree-sitter-sequel; we disambiguate by checking whether the
//! offset sits at or after the inner `keyword_having` child.
//!
//! `JOIN` follows the same pattern: the `join` node holds both the
//! table and the `keyword_on` predicate. We distinguish
//! `ScopeKind::JoinTable` (before `keyword_on`) from
//! `ScopeKind::JoinCondition` (at/after `keyword_on`).

use std::ops::Range;

use tree_sitter::{Node, Tree};

/// What kind of SQL clause / position the cursor is in.
///
/// Adding a variant is backwards-compatible because the enum is
/// `#[non_exhaustive]`; downstream `match` arms must include `_`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ScopeKind {
    /// Not inside any recognised statement (whitespace between
    /// statements, top-level comments, empty buffer).
    None,
    /// Inside a `SELECT` projection list (`SELECT <here> FROM ...`).
    SelectProjection,
    /// Inside the `FROM` clause's table list, including its joins'
    /// table positions.
    From,
    /// Inside a `JOIN`'s relation list (before `ON`).
    JoinTable,
    /// Inside a `JOIN ... ON <here>` predicate.
    JoinCondition,
    /// Inside a `WHERE` predicate.
    Where,
    /// Inside a `GROUP BY` field list (before `HAVING`).
    GroupBy,
    /// Inside a `HAVING` predicate.
    Having,
    /// Inside an `ORDER BY` field list.
    OrderBy,
    /// Inside a `LIMIT` / `OFFSET` clause.
    Limit,
    /// Inside an `UPDATE` SET assignment list.
    UpdateSet,
    /// Inside an `INSERT INTO` target-column list.
    InsertColumns,
    /// Inside an `INSERT INTO ... VALUES (...)` values tuple.
    InsertValues,
    /// Inside a `CREATE TABLE` column-definition list.
    ColumnDefinition,
    /// Inside a CTE definition (`WITH cte AS (...)`).
    Cte,
    /// Inside a statement but no more specific clause matched
    /// (e.g. between `SELECT` and the first projection).
    Statement,
}

/// Detailed scope answer for a single byte offset.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Scope {
    /// What clause the offset sits in.
    pub kind: ScopeKind,
    /// Byte range of the enclosing top-level statement, if any.
    /// `0..0` when [`ScopeKind::None`].
    pub statement_byte_range: Range<usize>,
    /// Byte range of the most-specific clause node we matched
    /// (`where`, `select_expression`, …). Equal to
    /// `statement_byte_range` when no clause is found inside a
    /// recognised statement.
    pub clause_byte_range: Range<usize>,
}

impl Scope {
    /// Empty scope, used when the buffer is empty or the offset
    /// lands between statements.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            kind: ScopeKind::None,
            statement_byte_range: 0..0,
            clause_byte_range: 0..0,
        }
    }
}

pub(super) fn scope_at_offset(tree: &Tree, _source: &str, byte_offset: usize) -> Scope {
    let root = tree.root_node();
    if root.named_child_count() == 0 {
        return Scope::none();
    }

    // Anchor at the smallest descendant that contains `byte_offset`.
    // When the offset is at end-of-buffer or in trailing whitespace,
    // `descendant_for_byte_range` will return the root; fall back to
    // the last child statement.
    let anchor = root
        .descendant_for_byte_range(byte_offset, byte_offset)
        .unwrap_or(root);

    let statement = enclosing_statement(anchor).unwrap_or(anchor);
    let statement_range = if statement.kind() == "program" {
        return Scope::none();
    } else {
        statement.byte_range()
    };

    // Walk upwards from `anchor`, picking the first kind we recognise.
    let mut node = Some(anchor);
    while let Some(n) = node {
        if let Some(kind) = classify(n, byte_offset) {
            return Scope {
                kind,
                statement_byte_range: statement_range,
                clause_byte_range: n.byte_range(),
            };
        }
        if n.id() == statement.id() {
            break;
        }
        node = n.parent();
    }

    Scope {
        kind: ScopeKind::Statement,
        statement_byte_range: statement_range.clone(),
        clause_byte_range: statement_range,
    }
}

/// Return the enclosing `statement` node, or `None` if the anchor
/// is the program root.
fn enclosing_statement(mut node: Node<'_>) -> Option<Node<'_>> {
    loop {
        if node.kind() == "statement" {
            return Some(node);
        }
        node = node.parent()?;
    }
}

fn classify(node: Node<'_>, byte_offset: usize) -> Option<ScopeKind> {
    match node.kind() {
        "select_expression" => Some(ScopeKind::SelectProjection),
        "from" => Some(ScopeKind::From),
        "where" => Some(ScopeKind::Where),
        "join" => {
            // tree-sitter-sequel attaches the `ON` predicate as a
            // field-named child (`predicate:`). If the offset is at
            // or after the `keyword_on` byte, classify as
            // JoinCondition; otherwise JoinTable.
            if offset_is_at_or_after_keyword(node, "keyword_on", byte_offset) {
                Some(ScopeKind::JoinCondition)
            } else {
                Some(ScopeKind::JoinTable)
            }
        }
        "group_by" => {
            // The grammar packs HAVING into the group_by node.
            if offset_is_at_or_after_keyword(node, "keyword_having", byte_offset) {
                Some(ScopeKind::Having)
            } else {
                Some(ScopeKind::GroupBy)
            }
        }
        "order_by" => Some(ScopeKind::OrderBy),
        "limit" => Some(ScopeKind::Limit),
        "update" => {
            // Anywhere inside an UPDATE that didn't match a more
            // specific child clause is treated as the SET list.
            // (The WHERE child would have matched first via the
            // walk-up.)
            Some(ScopeKind::UpdateSet)
        }
        "list" => {
            // INSERT INTO t (a, b) VALUES (1, 2);
            //               ^^^^^^  ^^^^^^
            //               columns  values
            // The grammar uses a generic `list` node for both. We
            // disambiguate by looking at the previous named sibling.
            classify_insert_list(node).or(Some(ScopeKind::Statement))
        }
        "column_definitions" => Some(ScopeKind::ColumnDefinition),
        "cte" => Some(ScopeKind::Cte),
        _ => None,
    }
}

fn offset_is_at_or_after_keyword(node: Node<'_>, keyword_kind: &str, byte_offset: usize) -> bool {
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return false;
    }
    loop {
        let child = cursor.node();
        if child.kind() == keyword_kind {
            return byte_offset >= child.start_byte();
        }
        if !cursor.goto_next_sibling() {
            return false;
        }
    }
}

/// Inside an `insert` node, classify a `list` child as either
/// `InsertColumns` or `InsertValues` based on whether
/// `keyword_values` has appeared earlier in the sibling stream.
fn classify_insert_list(list_node: Node<'_>) -> Option<ScopeKind> {
    let parent = list_node.parent()?;
    if parent.kind() != "insert" {
        return None;
    }
    let mut cursor = parent.walk();
    if !cursor.goto_first_child() {
        return None;
    }
    let mut seen_values = false;
    loop {
        let child = cursor.node();
        if child.id() == list_node.id() {
            return Some(if seen_values {
                ScopeKind::InsertValues
            } else {
                ScopeKind::InsertColumns
            });
        }
        if child.kind() == "keyword_values" {
            seen_values = true;
        }
        if !cursor.goto_next_sibling() {
            return None;
        }
    }
}
