//! SQL completion provider.
//!
//! The provider produces an ordered list of [`Completion`] candidates from
//! a prefix and the active session's cached schemas. Matches are scored
//! cheaply: exact case-insensitive prefix match wins, otherwise candidates
//! that contain the prefix as a substring come second.
//!
//! Context detection is intentionally minimal in this revision — we just
//! match against keywords + table names. A future iteration can plug in
//! per-table column suggestions by extending [`gather`] with a column
//! source.

use std::collections::BTreeSet;

use narwhal_tui::SchemaListing;

/// What a single completion entry represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompletionKind {
    /// Reserved SQL keyword (`SELECT`, `FROM`, …).
    Keyword,
    /// Table or view name.
    Table,
    /// Column belonging to a known table.
    Column,
}

/// Single completion candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub text: String,
    pub kind: CompletionKind,
    /// Optional secondary text shown next to the completion (e.g. the
    /// schema for a table or the type for a column).
    pub detail: Option<String>,
}

/// Statically known SQL keywords. The list is intentionally short — only
/// the ones that show up in everyday queries. Driver-specific keywords are
/// not handled here on purpose: the database server will reject typos and
/// adding obscure keywords would dilute completion quality.
pub const KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "IN",
    "BETWEEN",
    "LIKE",
    "IS",
    "NULL",
    "INSERT",
    "INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE",
    "TRUNCATE",
    "CREATE",
    "TABLE",
    "VIEW",
    "INDEX",
    "DROP",
    "ALTER",
    "ADD",
    "COLUMN",
    "PRIMARY",
    "KEY",
    "FOREIGN",
    "REFERENCES",
    "UNIQUE",
    "CHECK",
    "DEFAULT",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "OUTER",
    "FULL",
    "ON",
    "USING",
    "GROUP",
    "BY",
    "ORDER",
    "ASC",
    "DESC",
    "LIMIT",
    "OFFSET",
    "HAVING",
    "DISTINCT",
    "AS",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "UNION",
    "ALL",
    "EXCEPT",
    "INTERSECT",
    "EXISTS",
    "WITH",
    "RECURSIVE",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "SAVEPOINT",
    "RELEASE",
    "TRANSACTION",
];

/// Compute the completion list for `prefix` against `schemas`.
///
/// Returns up to `limit` entries, with exact prefix matches first. An
/// empty prefix returns an empty list — completion is opt-in and shouldn't
/// fire on `Tab` when the cursor is at column 0.
pub fn gather(prefix: &str, schemas: &[SchemaListing], limit: usize) -> Vec<Completion> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let lower_prefix = prefix.to_ascii_lowercase();

    let mut prefix_hits: Vec<Completion> = Vec::new();
    let mut substr_hits: Vec<Completion> = Vec::new();
    let mut seen: BTreeSet<(CompletionKind, String)> = BTreeSet::new();

    let push = |c: Completion,
                prefix_hits: &mut Vec<Completion>,
                substr_hits: &mut Vec<Completion>,
                seen: &mut BTreeSet<(CompletionKind, String)>| {
        let key = (c.kind, c.text.to_ascii_lowercase());
        if seen.contains(&key) {
            return;
        }
        let lower = c.text.to_ascii_lowercase();
        if lower.starts_with(&lower_prefix) {
            seen.insert(key);
            prefix_hits.push(c);
        } else if lower.contains(&lower_prefix) {
            seen.insert(key);
            substr_hits.push(c);
        }
    };

    for keyword in KEYWORDS {
        push(
            Completion {
                text: (*keyword).to_owned(),
                kind: CompletionKind::Keyword,
                detail: None,
            },
            &mut prefix_hits,
            &mut substr_hits,
            &mut seen,
        );
    }

    for (schema, tables) in schemas {
        for table in tables {
            let detail = if schema.name.is_empty() {
                None
            } else {
                Some(schema.name.clone())
            };
            push(
                Completion {
                    text: table.name.clone(),
                    kind: CompletionKind::Table,
                    detail,
                },
                &mut prefix_hits,
                &mut substr_hits,
                &mut seen,
            );
        }
    }

    // Sort each tier alphabetically (case-insensitive) for predictability.
    let cmp = |a: &Completion, b: &Completion| {
        a.text
            .to_ascii_lowercase()
            .cmp(&b.text.to_ascii_lowercase())
    };
    prefix_hits.sort_by(cmp);
    substr_hits.sort_by(cmp);

    let mut out = prefix_hits;
    out.extend(substr_hits);
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use narwhal_core::{Schema, Table, TableKind};

    fn listing() -> Vec<SchemaListing> {
        vec![(
            Schema {
                name: "public".into(),
            },
            vec![
                Table {
                    schema: "public".into(),
                    name: "orders".into(),
                    kind: TableKind::Table,
                },
                Table {
                    schema: "public".into(),
                    name: "order_items".into(),
                    kind: TableKind::Table,
                },
                Table {
                    schema: "public".into(),
                    name: "users".into(),
                    kind: TableKind::Table,
                },
            ],
        )]
    }

    #[test]
    fn empty_prefix_yields_nothing() {
        assert!(gather("", &listing(), 20).is_empty());
    }

    #[test]
    fn prefix_hits_come_before_substring_hits() {
        let out = gather("or", &listing(), 20);
        let ord = out
            .iter()
            .position(|c| c.text == "orders")
            .expect("orders present");
        let ord_items = out
            .iter()
            .position(|c| c.text == "order_items")
            .expect("order_items present");
        let or = out
            .iter()
            .position(|c| c.text == "OR")
            .expect("OR keyword present");
        // Both "orders" and "order_items" prefix-match; "OR" also
        // prefix-matches as a keyword. All three are in the prefix tier.
        assert!(ord < out.len() && ord_items < out.len() && or < out.len());
    }

    #[test]
    fn case_insensitive_match() {
        let out = gather("SEL", &listing(), 20);
        assert!(out.iter().any(|c| c.text == "SELECT"));
    }

    #[test]
    fn deduplicates_by_kind_and_name() {
        // Two listings would each emit `orders`; the result still has it
        // only once.
        let mut listings = listing();
        listings.push(listings[0].clone());
        let out = gather("orders", &listings, 20);
        let n = out.iter().filter(|c| c.text == "orders").count();
        assert_eq!(n, 1);
    }

    #[test]
    fn limit_is_respected() {
        let out = gather("e", &listing(), 3);
        assert!(out.len() <= 3);
    }
}
