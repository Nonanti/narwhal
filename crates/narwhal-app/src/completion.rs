//! SQL completion provider.
//!
//! The provider produces an ordered list of [`Completion`] candidates from
//! a prefix and the active session's cached schemas. Matches are scored
//! cheaply: exact case-insensitive prefix match wins, otherwise candidates
//! that contain the prefix as a substring come second.
//!
//! Context detection walks the editor buffer backward from the cursor
//! and classifies the position into [`CompletionContext`] variants so the
//! candidate set can be narrowed — e.g. after `FROM` only table names
//! are shown, after `table.` only that table's columns.

use std::collections::{BTreeSet, HashMap};

use narwhal_core::ColumnHeader;
use narwhal_tui::SchemaListing;

/// Keywords that signal the next token should be a table name.
const TABLE_EXPECTED_KEYWORDS: &[&str] = &[
    "FROM", "JOIN", "INNER", "LEFT", "RIGHT", "OUTER", "FULL", "CROSS", "INTO", "UPDATE", "TABLE",
    "DESCRIBE", "DESC",
];

/// Context inferred from the tokens preceding the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompletionContext {
    /// No special context — mix keywords, phrases, and tables as before.
    Generic,
    /// Previous keyword is FROM / JOIN / INTO / UPDATE / TABLE / DESCRIBE
    /// etc. — prefer table names.
    TableExpected,
    /// Cursor sits right after `ident.` — suggest columns of that table.
    /// The `table` field is already alias-resolved: `FROM users u`
    /// followed by `u.` arrives here as `table = "users"`.
    ColumnExpected { table: String },
    /// Cursor sits right after `schema.` — suggest tables that live in
    /// the named schema (e.g. `public.`).
    SchemaTableExpected { schema: String },
}

/// Walk backward from `cursor_byte_offset` inside `buffer`, stopping at
/// the previous `;` or the start, and decide which context is in play.
///
/// The tokeniser is deliberately lightweight — it only needs to
/// distinguish identifiers, dots, keywords, and boundaries. It skips
/// over string literals and comments so they can't fake a keyword.
///
/// Schema-aware behaviour:
///
/// - `FROM users u` followed by `u.` resolves to
///   `ColumnExpected { table: "users" }` because we extract a
///   forward-walking alias map before the reverse classification pass.
/// - `public.` (where `public` is *not* a known alias) resolves to
///   `SchemaTableExpected { schema: "public" }` so the gather step
///   can narrow the table list.
pub fn detect_context(buffer: &str, cursor_byte_offset: usize) -> CompletionContext {
    detect_context_with_schemas(buffer, cursor_byte_offset, &[])
}

/// Like [`detect_context`] but consults the active session's schema
/// names so `public.` is recognised as a schema rather than misclassified
/// as a table alias. Pass an empty slice when no session is open.
pub fn detect_context_with_schemas(
    buffer: &str,
    cursor_byte_offset: usize,
    known_schemas: &[String],
) -> CompletionContext {
    let slice = trim_to_current_statement(buffer, cursor_byte_offset);
    let tokens = tokenize(&slice);

    // Build a forward-walking alias map: `FROM users u` and
    // `JOIN orders AS o` both contribute `(u, users)` / `(o, orders)`.
    // We need it before the reverse pass so `u.` can resolve to `users`.
    let alias_map = extract_aliases(&tokens);

    // Walk tokens in reverse so the *closest* keyword to the cursor
    // wins (handles nested clauses like `SELECT ... FROM (SELECT ...`).
    let mut saw_dot = false;
    let mut ident_before_dot: Option<String> = None;
    let mut last_keyword: Option<&str> = None;

    for tok in tokens.iter().rev() {
        match tok {
            Token::Dot => {
                if ident_before_dot.is_none() {
                    saw_dot = true;
                }
            }
            Token::Ident(name) => {
                if saw_dot && ident_before_dot.is_none() {
                    ident_before_dot = Some(name.to_ascii_lowercase());
                    saw_dot = false;
                }
            }
            Token::Keyword(kw) => {
                last_keyword = Some(kw);
                break;
            }
            Token::StringLiteral | Token::Other => {}
        }
    }

    if let Some(ident) = ident_before_dot {
        // Alias wins over schema (they share the same syntax `ident.`).
        if let Some(resolved) = alias_map.get(&ident) {
            return CompletionContext::ColumnExpected {
                table: resolved.clone(),
            };
        }
        // Then a known schema.
        if known_schemas.iter().any(|s| s.eq_ignore_ascii_case(&ident)) {
            return CompletionContext::SchemaTableExpected { schema: ident };
        }
        // Fallback: treat as a literal table name (legacy behaviour).
        return CompletionContext::ColumnExpected { table: ident };
    }

    if let Some(kw) = last_keyword {
        if TABLE_EXPECTED_KEYWORDS
            .iter()
            .any(|k| k.eq_ignore_ascii_case(kw))
        {
            return CompletionContext::TableExpected;
        }
    }

    CompletionContext::Generic
}

/// Forward-walking pass that picks up `FROM table alias` and
/// `JOIN table AS alias` constructs and returns a map of
/// `lowercase(alias) -> lowercase(table_name)`.
///
/// Supports both implicit (`FROM users u`) and explicit
/// (`JOIN orders AS o`) alias forms. Comma-joined tables
/// (`FROM users u, orders o`) are not yet handled — only the
/// first table picks up an alias from that style today.
fn extract_aliases(tokens: &[Token]) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let mut i = 0;
    while i < tokens.len() {
        let claims_table = matches!(&tokens[i], Token::Keyword(kw)
            if matches!(kw.as_str(), "FROM" | "JOIN"));
        if !claims_table {
            i += 1;
            continue;
        }
        // Skip the FROM/JOIN keyword itself.
        i += 1;
        // Skip secondary join modifiers (LEFT JOIN, INNER JOIN, etc.).
        while let Some(Token::Keyword(kw)) = tokens.get(i) {
            if matches!(
                kw.as_str(),
                "INNER" | "OUTER" | "LEFT" | "RIGHT" | "FULL" | "CROSS" | "JOIN"
            ) {
                i += 1;
            } else {
                break;
            }
        }
        let Some(Token::Ident(table)) = tokens.get(i) else {
            continue;
        };
        let table_name = table.to_ascii_lowercase();
        i += 1;
        // Optional `schema.table` form: pick the second identifier.
        if matches!(tokens.get(i), Some(Token::Dot)) {
            if let Some(Token::Ident(real)) = tokens.get(i + 1) {
                let _ = table_name;
                let table_name = real.to_ascii_lowercase();
                i += 2;
                consume_alias(tokens, &mut i, &mut map, &table_name);
                continue;
            }
        }
        consume_alias(tokens, &mut i, &mut map, &table_name);
    }
    map
}

fn consume_alias(
    tokens: &[Token],
    i: &mut usize,
    map: &mut std::collections::HashMap<String, String>,
    table_name: &str,
) {
    // Optional explicit `AS`.
    if matches!(tokens.get(*i), Some(Token::Keyword(kw)) if kw == "AS") {
        *i += 1;
    }
    if let Some(Token::Ident(alias)) = tokens.get(*i) {
        // Reject anything that looks like a keyword being misclassified
        // as an identifier (defensive).
        let alias_lc = alias.to_ascii_lowercase();
        map.insert(alias_lc, table_name.to_owned());
        *i += 1;
    }
}

/// Trim `buffer` to only the portion of the current statement —
/// everything after the last `;` that appears before `cursor_byte_offset`.
fn trim_to_current_statement(buffer: &str, cursor_byte_offset: usize) -> String {
    let end = cursor_byte_offset.min(buffer.len());
    let prefix = &buffer[..end];
    // Find the last `;` before the cursor.
    if let Some(pos) = prefix.rfind(';') {
        prefix[pos + ';'.len_utf8()..].to_owned()
    } else {
        prefix.to_owned()
    }
}

/// Lightweight token produced by the backward-walking tokeniser.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// SQL identifier (table name, column name, etc.).
    Ident(String),
    /// SQL keyword (may also be an identifier in some contexts, but we
    /// classify it as a keyword when it matches a known SQL word).
    Keyword(String),
    /// Standalone dot between two identifiers.
    Dot,
    /// String literal — skipped for context purposes.
    StringLiteral,
    /// Anything else (operators, parentheses, etc.).
    Other,
}

/// Tokenise `input` into a sequence of [`Token`] values. Walks forward
/// through the input, skipping string literals and comments.
fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Skip whitespace.
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // `--` line comment — skip to end of line.
        if i + 1 < len && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            i += 2;
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        // `/* */` block comment.
        if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < len {
                i += 2; // skip */
            }
            continue;
        }

        // Single-quoted string literal.
        if bytes[i] == b'\'' {
            i += 1;
            while i < len {
                if bytes[i] == b'\'' {
                    i += 1;
                    // Escaped quote inside string.
                    if i < len && bytes[i] == b'\'' {
                        i += 1;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            tokens.push(Token::StringLiteral);
            continue;
        }

        // Double-quoted identifier / string.
        if bytes[i] == b'"' {
            i += 1;
            while i < len && bytes[i] != b'"' {
                i += 1;
            }
            if i < len {
                i += 1;
            }
            tokens.push(Token::StringLiteral);
            continue;
        }

        // Dot.
        if bytes[i] == b'.' {
            tokens.push(Token::Dot);
            i += 1;
            continue;
        }

        // Identifier or keyword.
        if is_ident_start(bytes[i]) {
            let start = i;
            i += 1;
            while i < len && is_ident_cont(bytes[i]) {
                i += 1;
            }
            let word = &input[start..i];
            if TABLE_EXPECTED_KEYWORDS
                .iter()
                .any(|k| k.eq_ignore_ascii_case(word))
                || KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(word))
            {
                tokens.push(Token::Keyword(word.to_ascii_uppercase()));
            } else {
                tokens.push(Token::Ident(word.to_owned()));
            }
            continue;
        }

        // Anything else.
        i += 1;
        tokens.push(Token::Other);
    }

    tokens
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// What a single completion entry represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum CompletionKind {
    /// Reserved SQL keyword (`SELECT`, `FROM`, …).
    Keyword,
    /// Table or view name.
    Table,
    /// Column belonging to a known table.
    Column,
    /// Built-in / aggregate function (`COUNT(`, `SUM(`, …).
    /// Inserted with the trailing `(` so the cursor lands inside the
    /// argument list ready for the user to type the column.
    Function,
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

/// Built-in functions offered as `NAME(` so the cursor lands inside
/// the parens after acceptance. Mirrors the everyday set used across
/// postgres / mysql / sqlite / clickhouse / duckdb; engine-specific
/// helpers are intentionally omitted to keep the popup focused.
pub const FUNCTIONS: &[&str] = &[
    // Aggregates.
    "COUNT(",
    "COUNT(DISTINCT ",
    "SUM(",
    "AVG(",
    "MIN(",
    "MAX(",
    "STRING_AGG(",
    "ARRAY_AGG(",
    "GROUP_CONCAT(",
    // Null / type helpers.
    "COALESCE(",
    "NULLIF(",
    "GREATEST(",
    "LEAST(",
    "CAST(",
    // String.
    "LOWER(",
    "UPPER(",
    "LENGTH(",
    "TRIM(",
    "SUBSTRING(",
    "SUBSTR(",
    "REPLACE(",
    "CONCAT(",
    "SPLIT_PART(",
    // Date / time.
    "NOW()",
    "CURRENT_DATE",
    "CURRENT_TIMESTAMP",
    "DATE_TRUNC(",
    "EXTRACT(",
    "AGE(",
    // Math.
    "ROUND(",
    "CEIL(",
    "FLOOR(",
    "ABS(",
    "POWER(",
    // Window.
    "ROW_NUMBER() OVER (",
    "RANK() OVER (",
    "DENSE_RANK() OVER (",
    "LAG(",
    "LEAD(",
];

/// Multi-word SQL phrases offered as a single completion. They cover the
/// most common 2- to 4-token sequences a daily database user types so
/// the popup can suggest `CREATE TABLE` instead of just `CREATE` and
/// then forcing a second round of completion.
///
/// Matching is the same lowercase prefix/substring strategy used for
/// single keywords — typing `crea` lights up `CREATE TABLE`, `CREATE
/// INDEX`, ... in alphabetical order.
pub const PHRASES: &[&str] = &[
    "CREATE TABLE",
    "CREATE TABLE IF NOT EXISTS",
    "CREATE INDEX",
    "CREATE UNIQUE INDEX",
    "CREATE VIEW",
    "CREATE OR REPLACE VIEW",
    "CREATE MATERIALIZED VIEW",
    "CREATE SCHEMA",
    "CREATE TEMPORARY TABLE",
    "DROP TABLE",
    "DROP TABLE IF EXISTS",
    "DROP INDEX",
    "DROP VIEW",
    "DROP SCHEMA",
    "ALTER TABLE",
    "ALTER INDEX",
    "ADD COLUMN",
    "DROP COLUMN",
    "RENAME COLUMN",
    "RENAME TO",
    "INSERT INTO",
    "DELETE FROM",
    "SELECT *",
    "SELECT * FROM",
    "SELECT DISTINCT",
    "SELECT COUNT(*)",
    "LEFT JOIN",
    "RIGHT JOIN",
    "INNER JOIN",
    "OUTER JOIN",
    "FULL OUTER JOIN",
    "CROSS JOIN",
    "GROUP BY",
    "ORDER BY",
    "ORDER BY ASC",
    "ORDER BY DESC",
    "LIMIT",
    "OFFSET",
    "UNION ALL",
    "IS NULL",
    "IS NOT NULL",
    "NOT NULL",
    "DEFAULT NULL",
    "PRIMARY KEY",
    "FOREIGN KEY",
    "REFERENCES",
    "ON DELETE CASCADE",
    "ON UPDATE CASCADE",
    "ON CONFLICT",
    "BEGIN TRANSACTION",
    "COMMIT TRANSACTION",
    "ROLLBACK TRANSACTION",
    "SAVEPOINT",
    "WITH RECURSIVE",
    "AS",
    "CASE WHEN",
    "ELSE END",
];

/// Compute the completion list for `prefix` against `schemas`.
///
/// Returns up to `limit` entries, with exact prefix matches first. An
/// empty prefix returns an empty list — completion is opt-in and shouldn't
/// fire on `Tab` when the cursor is at column 0.
///
/// The `columns` map keys are lowercased table names; the values are
/// `(schema_name, columns)` tuples so each column completion can carry
/// the schema as its detail string.
pub fn gather(
    prefix: &str,
    schemas: &[SchemaListing],
    context: &CompletionContext,
    columns: &HashMap<String, (String, Vec<ColumnHeader>)>,
    limit: usize,
) -> Vec<Completion> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let lower_prefix = prefix.to_ascii_lowercase();

    let mut prefix_hits: Vec<Completion> = Vec::new();
    let mut substr_hits: Vec<Completion> = Vec::new();
    let mut seen: BTreeSet<(CompletionKind, String)> = BTreeSet::new();

    let mut push = |c: Completion| {
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

    match context {
        CompletionContext::TableExpected => {
            // Only tables — keywords after FROM/JOIN/etc. are never valid
            // SQL and would dilute the results.
            for (schema, tables) in schemas {
                for table in tables {
                    let detail = if schema.name.is_empty() {
                        None
                    } else {
                        Some(schema.name.clone())
                    };
                    push(Completion {
                        text: table.name.clone(),
                        kind: CompletionKind::Table,
                        detail,
                    });
                }
            }
        }
        CompletionContext::ColumnExpected { table } => {
            let lower_table = table.to_ascii_lowercase();
            if let Some((schema_name, cols)) = columns.get(&lower_table) {
                for col in cols {
                    let detail = if schema_name.is_empty() {
                        None
                    } else {
                        Some(schema_name.clone())
                    };
                    push(Completion {
                        text: col.name.clone(),
                        kind: CompletionKind::Column,
                        detail,
                    });
                }
            }
        }
        CompletionContext::SchemaTableExpected { schema } => {
            // Only emit tables whose owning schema name matches. We
            // skip the schema-name detail because the prefix already
            // displays it visually.
            for (s, tables) in schemas {
                if !s.name.eq_ignore_ascii_case(schema) {
                    continue;
                }
                for table in tables {
                    push(Completion {
                        text: table.name.clone(),
                        kind: CompletionKind::Table,
                        detail: None,
                    });
                }
            }
        }
        CompletionContext::Generic => {
            for keyword in KEYWORDS {
                push(Completion {
                    text: (*keyword).to_owned(),
                    kind: CompletionKind::Keyword,
                    detail: None,
                });
            }
            for phrase in PHRASES {
                push(Completion {
                    text: (*phrase).to_owned(),
                    kind: CompletionKind::Keyword,
                    detail: None,
                });
            }
            for func in FUNCTIONS {
                push(Completion {
                    text: (*func).to_owned(),
                    kind: CompletionKind::Function,
                    detail: None,
                });
            }
            for (schema, tables) in schemas {
                for table in tables {
                    let detail = if schema.name.is_empty() {
                        None
                    } else {
                        Some(schema.name.clone())
                    };
                    push(Completion {
                        text: table.name.clone(),
                        kind: CompletionKind::Table,
                        detail,
                    });
                }
            }
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

    fn no_columns() -> HashMap<String, (String, Vec<ColumnHeader>)> {
        HashMap::new()
    }

    #[test]
    fn empty_prefix_yields_nothing() {
        assert!(gather(
            "",
            &listing(),
            &CompletionContext::Generic,
            &no_columns(),
            20
        )
        .is_empty());
    }

    #[test]
    fn prefix_hits_come_before_substring_hits() {
        let out = gather(
            "or",
            &listing(),
            &CompletionContext::Generic,
            &no_columns(),
            20,
        );
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
        let out = gather(
            "SEL",
            &listing(),
            &CompletionContext::Generic,
            &no_columns(),
            20,
        );
        assert!(out.iter().any(|c| c.text == "SELECT"));
    }

    #[test]
    fn deduplicates_by_kind_and_name() {
        // Two listings would each emit `orders`; the result still has it
        // only once.
        let mut listings = listing();
        listings.push(listings[0].clone());
        let out = gather(
            "orders",
            &listings,
            &CompletionContext::Generic,
            &no_columns(),
            20,
        );
        let n = out.iter().filter(|c| c.text == "orders").count();
        assert_eq!(n, 1);
    }

    #[test]
    fn limit_is_respected() {
        let out = gather(
            "e",
            &listing(),
            &CompletionContext::Generic,
            &no_columns(),
            3,
        );
        assert!(out.len() <= 3);
    }

    // ----- context detection tests -----

    #[test]
    fn from_keyword_narrows_to_tables() {
        let ctx = detect_context("SELECT * FROM u", 14);
        assert_eq!(ctx, CompletionContext::TableExpected);

        let out = gather("u", &listing(), &ctx, &no_columns(), 50);
        // Should contain `users` table but NOT `UNION` or `UPDATE` keywords.
        assert!(out
            .iter()
            .any(|c| c.text == "users" && c.kind == CompletionKind::Table));
        assert!(!out
            .iter()
            .any(|c| c.text == "UNION" && c.kind == CompletionKind::Keyword));
        assert!(!out
            .iter()
            .any(|c| c.text == "UPDATE" && c.kind == CompletionKind::Keyword));
    }

    #[test]
    fn dotted_identifier_suggests_columns() {
        let mut cols = HashMap::new();
        cols.insert(
            "users".to_owned(),
            (
                "public".to_owned(),
                vec![
                    ColumnHeader {
                        name: "id".into(),
                        data_type: "int4".into(),
                    },
                    ColumnHeader {
                        name: "name".into(),
                        data_type: "varchar".into(),
                    },
                    ColumnHeader {
                        name: "email".into(),
                        data_type: "varchar".into(),
                    },
                ],
            ),
        );
        let ctx = detect_context("SELECT users.", 13);
        assert_eq!(
            ctx,
            CompletionContext::ColumnExpected {
                table: "users".into()
            }
        );

        let out = gather("", &listing(), &ctx, &cols, 50);
        // Empty prefix yields nothing — completion is opt-in.
        assert!(out.is_empty());

        // With a prefix we get the matching columns.
        let out = gather("n", &listing(), &ctx, &cols, 50);
        assert!(out
            .iter()
            .any(|c| c.text == "name" && c.kind == CompletionKind::Column));
        assert!(!out.iter().any(|c| c.kind == CompletionKind::Keyword));
    }

    #[test]
    fn context_stops_at_previous_semicolon() {
        let ctx = detect_context("SELECT * FROM users; SELECT u", 27);
        // The FROM is past the `;`, so we should NOT be in TableExpected.
        assert_eq!(ctx, CompletionContext::Generic);
    }

    #[test]
    fn join_keyword_narrows_to_tables() {
        let ctx = detect_context("SELECT * FROM orders JOIN u", 27);
        assert_eq!(ctx, CompletionContext::TableExpected);

        let out = gather("u", &listing(), &ctx, &no_columns(), 50);
        assert!(out
            .iter()
            .any(|c| c.text == "users" && c.kind == CompletionKind::Table));
        assert!(!out
            .iter()
            .any(|c| c.text == "UNION" && c.kind == CompletionKind::Keyword));
    }

    #[test]
    fn update_keyword_narrows_to_tables() {
        let ctx = detect_context("UPDATE u", 8);
        assert_eq!(ctx, CompletionContext::TableExpected);

        let out = gather("u", &listing(), &ctx, &no_columns(), 50);
        assert!(out
            .iter()
            .any(|c| c.text == "users" && c.kind == CompletionKind::Table));
        assert!(!out
            .iter()
            .any(|c| c.text == "UNION" && c.kind == CompletionKind::Keyword));
    }

    // ----- new tests: alias resolution + schema prefix + functions -----

    fn user_cols() -> HashMap<String, (String, Vec<ColumnHeader>)> {
        let mut m = HashMap::new();
        m.insert(
            "users".to_owned(),
            (
                "public".to_owned(),
                vec![
                    ColumnHeader {
                        name: "id".into(),
                        data_type: "int4".into(),
                    },
                    ColumnHeader {
                        name: "email".into(),
                        data_type: "text".into(),
                    },
                ],
            ),
        );
        m
    }

    /// `FROM users u WHERE u.` should resolve `u` → `users` and
    /// suggest the table's columns instead of treating `u` as a real
    /// table name.
    #[test]
    fn alias_in_from_resolves_to_real_table_for_dot_completion() {
        let buf = "SELECT * FROM users u WHERE u.";
        let ctx = detect_context(buf, buf.len());
        assert_eq!(
            ctx,
            CompletionContext::ColumnExpected {
                table: "users".into()
            }
        );
        let out = gather("e", &listing(), &ctx, &user_cols(), 50);
        assert!(out
            .iter()
            .any(|c| c.text == "email" && c.kind == CompletionKind::Column));
    }

    /// `JOIN orders AS o ON o.` walks through the explicit `AS` form.
    #[test]
    fn alias_with_explicit_as_keyword_is_resolved() {
        let mut cols = user_cols();
        cols.insert(
            "orders".to_owned(),
            (
                "public".to_owned(),
                vec![ColumnHeader {
                    name: "total".into(),
                    data_type: "numeric".into(),
                }],
            ),
        );
        let buf = "SELECT * FROM users u JOIN orders AS o ON o.";
        let ctx = detect_context(buf, buf.len());
        assert_eq!(
            ctx,
            CompletionContext::ColumnExpected {
                table: "orders".into()
            }
        );
        let out = gather("t", &listing(), &ctx, &cols, 50);
        assert!(out.iter().any(|c| c.text == "total"));
    }

    /// `public.` when `public` is a known schema lands in
    /// `SchemaTableExpected` and the gather only emits tables from
    /// that schema.
    #[test]
    fn schema_prefix_narrows_table_list() {
        let buf = "SELECT * FROM public.";
        let known = vec!["public".to_owned()];
        let ctx = detect_context_with_schemas(buf, buf.len(), &known);
        assert_eq!(
            ctx,
            CompletionContext::SchemaTableExpected {
                schema: "public".into()
            }
        );
        let out = gather("u", &listing(), &ctx, &no_columns(), 50);
        assert!(out
            .iter()
            .any(|c| c.text == "users" && c.kind == CompletionKind::Table));
    }

    /// Without the schema list, `public.` falls back to the legacy
    /// behaviour (ColumnExpected on a non-existent table).
    #[test]
    fn unknown_dotted_prefix_falls_back_to_column_lookup() {
        let buf = "SELECT * FROM public.";
        let ctx = detect_context(buf, buf.len());
        assert_eq!(
            ctx,
            CompletionContext::ColumnExpected {
                table: "public".into()
            }
        );
    }

    /// Generic context surfaces the function list with the trailing
    /// `(` so the cursor lands inside the call after acceptance.
    #[test]
    fn generic_context_includes_functions() {
        let ctx = detect_context("SELECT ", 7);
        assert_eq!(ctx, CompletionContext::Generic);
        let out = gather("cou", &listing(), &ctx, &no_columns(), 50);
        assert!(out
            .iter()
            .any(|c| c.text == "COUNT(" && c.kind == CompletionKind::Function));
    }

    /// Function suggestions are kind-tagged distinctly so the UI can
    /// render them with a different glyph.
    #[test]
    fn function_kind_distinct_from_keyword() {
        let ctx = detect_context("SELECT ", 7);
        let out = gather("now", &listing(), &ctx, &no_columns(), 50);
        assert!(out
            .iter()
            .any(|c| c.text == "NOW()" && c.kind == CompletionKind::Function));
    }
}
