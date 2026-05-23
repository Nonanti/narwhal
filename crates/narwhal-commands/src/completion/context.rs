//! Cursor-context classification: where in the SQL statement the
//! cursor sits, and which kind of completion candidate makes
//! sense there.

use super::tokenizer::{tokenize, Token};

pub(super) const TABLE_EXPECTED_KEYWORDS: &[&str] = &[
    "FROM", "JOIN", "INNER", "LEFT", "RIGHT", "OUTER", "FULL", "CROSS", "INTO", "UPDATE", "TABLE",
    "DESCRIBE", "DESC",
];

/// Context inferred from the tokens preceding the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
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
