//! Extract the source table from a SELECT statement so the export
//! pipeline can decide whether INSERT output is safe.

use super::format::QualifiedName;
use super::quoting::unquote_ident;

pub fn extract_source_table(sql: &str) -> Option<QualifiedName> {
    // Normalise: trim, remove trailing semicolons, collapse whitespace.
    let normalised = sql.trim().trim_end_matches(';').trim();
    let lower = normalised.to_ascii_lowercase();

    // Must start with SELECT (case-insensitive).
    if !lower.starts_with("select") {
        return None;
    }

    // Find FROM keyword — simple scan. We look for ` FROM ` as a word
    // boundary to avoid matching "INFORMATION_SCHEMA" or subqueries.
    // This is a heuristic, not a full SQL parser.
    let from_pos = find_from_keyword(&lower)?;
    let after_from = &normalised[from_pos + " from ".len()..].trim_start();

    // Extract the first identifier (possibly qualified as schema.table).
    let (ident, rest) = extract_first_identifier(after_from);

    if ident.is_empty() {
        return None;
    }

    let rest_trimmed = rest.trim();

    // End of query or clause boundary → single table.
    if rest_trimmed.is_empty() || is_clause_boundary(rest_trimmed) {
        return Some(split_qualified(&ident));
    }

    // Multi-table indicator (JOIN, comma) → not a single table.
    if is_multi_table_indicator(rest_trimmed) {
        return None;
    }

    // Try to skip an alias ("AS alias" or bare non-keyword identifier),
    // then re-check for end-of-query / clause boundary.
    if let Some(after_alias) = skip_alias(rest_trimmed) {
        let after_trimmed = after_alias.trim();
        if after_trimmed.is_empty() || is_clause_boundary(after_trimmed) {
            return Some(split_qualified(&ident));
        }
    }

    None
}

/// Find the position of the first top-level ` FROM ` keyword in the
/// lowercased SQL string. Skips over parenthesised subqueries.
pub(super) fn find_from_keyword(lower: &str) -> Option<usize> {
    let bytes = lower.as_bytes();
    let len = bytes.len();
    let keyword = b" from ";
    let mut depth = 0usize;

    let mut i = 0;
    while i + keyword.len() <= len {
        if bytes[i] == b'(' {
            depth += 1;
            i += 1;
            continue;
        }
        if bytes[i] == b')' {
            depth = depth.saturating_sub(1);
            i += 1;
            continue;
        }
        if depth == 0 && lower[i..].starts_with(" from ") {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Extract the first SQL identifier from `input`, returning the
/// identifier and the remaining text. An identifier may be
/// `schema.table` (two identifiers joined by a dot). Supports both
/// bare (`users`) and double-quoted (`"users"`) identifiers.
fn extract_first_identifier(input: &str) -> (String, &str) {
    let bytes = input.as_bytes();
    let len = bytes.len();

    // First segment: bare or quoted.
    let end = extract_ident_segment(bytes, 0);
    if end == 0 {
        return (String::new(), input);
    }

    // Optional dot + second segment.
    let mut final_end = end;
    if final_end < len && bytes[final_end] == b'.' {
        let seg = extract_ident_segment(bytes, final_end + 1);
        if seg > final_end + 1 {
            final_end = seg;
        }
    }

    (input[..final_end].to_owned(), &input[final_end..])
}

/// Advance past one identifier segment starting at `start`.
/// Returns the new position (== `start` if no identifier found).
/// Handles both bare identifiers and `"quoted"` identifiers.
fn extract_ident_segment(bytes: &[u8], start: usize) -> usize {
    let len = bytes.len();
    if start >= len {
        return start;
    }
    if bytes[start] == b'"' {
        // Quoted identifier: scan to the closing unescaped double-quote.
        let mut i = start + 1;
        while i < len {
            if bytes[i] == b'"' {
                if i + 1 < len && bytes[i + 1] == b'"' {
                    i += 2; // escaped ""
                } else {
                    return i + 1; // closing quote
                }
            } else {
                i += 1;
            }
        }
        start // unclosed quote — treat as no identifier
    } else if bytes[start].is_ascii_alphabetic() || bytes[start] == b'_' {
        let mut i = start + 1;
        while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        i
    } else {
        start
    }
}

/// Check whether `rest` starts with a SQL clause keyword, indicating
/// that the previous identifier was the sole table reference.
fn is_clause_boundary(rest: &str) -> bool {
    let lower = rest.to_ascii_lowercase();
    let keywords = [
        "where ",
        "where\t",
        "where\n",
        "group ",
        "having ",
        "order ",
        "limit ",
        "offset ",
        "union ",
        "intersect ",
        "except ",
        "for ",
        "lock ",
        ";",
    ];
    keywords.iter().any(|kw| lower.starts_with(kw))
}

/// Split a possibly-quoted qualified identifier (`schema.table` or
/// `"schema"."table"`) into a [`QualifiedName`], unquoting each part.
fn split_qualified(ident: &str) -> QualifiedName {
    let bytes = ident.as_bytes();
    let len = bytes.len();

    // Skip the first segment to find the dot separator.
    let first_end = extract_ident_segment(bytes, 0);

    if first_end < len && bytes[first_end] == b'.' {
        let schema_raw = &ident[..first_end];
        let table_raw = &ident[first_end + 1..];
        let schema = unquote_ident(schema_raw);
        let table = unquote_ident(table_raw);
        if !schema.is_empty() && !table.is_empty() {
            return QualifiedName {
                schema: Some(schema),
                table,
            };
        }
    }

    QualifiedName {
        schema: None,
        table: unquote_ident(ident),
    }
}

/// Return `true` if `s` starts with a JOIN keyword or comma,
/// indicating a multi-table query.
fn is_multi_table_indicator(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    [
        "join ", "inner ", "left ", "right ", "full ", "cross ", "natural ", ",",
    ]
    .iter()
    .any(|kw| lower.starts_with(kw))
}

/// Try to skip an alias after a table name. Returns the remaining text
/// after the alias, or `None` if no alias pattern was recognised.
fn skip_alias(s: &str) -> Option<&str> {
    let lower = s.to_ascii_lowercase();

    // "AS <alias>" pattern.
    if lower.starts_with("as ") || lower.starts_with("as\t") || lower.starts_with("as\n") {
        let after_as = s["as".len()..].trim_start();
        let (_alias, rest) = extract_first_identifier(after_as);
        if _alias.is_empty() {
            return None;
        }
        return Some(rest);
    }

    // Bare alias: a non-keyword identifier.
    let (candidate, rest) = extract_first_identifier(s);
    if candidate.is_empty() {
        return None;
    }

    // Reject SQL keywords that can follow a table name but are not aliases.
    let kw = candidate.to_ascii_lowercase();
    let reserved = [
        "where",
        "group",
        "having",
        "order",
        "limit",
        "offset",
        "union",
        "intersect",
        "except",
        "for",
        "lock",
        "join",
        "inner",
        "left",
        "right",
        "full",
        "cross",
        "natural",
        "on",
        "using",
    ];
    if reserved.contains(&kw.as_str()) {
        return None;
    }

    Some(rest)
}

