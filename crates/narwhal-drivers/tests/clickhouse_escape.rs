//! Regression tests for `escape_sql_string` backslash handling (bug M4).
//!
//! `ClickHouse` honours backslash escapes inside string literals.
//! Without escaping `\` to `\\`, a string like `C:\Users` would be
//! rendered as `'C:\Users'` — the `\U` would be interpreted as an
//! escape sequence, potentially changing the value or even "eating" a
//! closing quote if the last character is a backslash (e.g.
//! `'path\'` would see `\'` as an escaped quote, leaving the string
//! unclosed — injection vector).

use narwhal_core::Value;
use narwhal_drivers::clickhouse::__test_only::{replace_question_marks, substitute_params};

/// `escape_sql_string` must double backslashes so that `ClickHouse`
/// interprets them as literal backslash characters, not escape leaders.
#[test]
fn escape_handles_backslash() {
    let sql = "SELECT ?";
    let out = replace_question_marks(sql, &[Value::String(r"C:\Users\admin".to_owned())]);
    // The rendered literal must contain `\\` for each `\` in the input.
    assert!(
        out.contains(r"C:\\Users\\admin"),
        "backslash not escaped: {out}"
    );
}

/// A string that ends with a backslash must not swallow the closing
/// quote. Before the fix, `path\` would render as `'path\'` — the
/// `\'` is an escaped quote, not a closing quote + stray backslash.
#[test]
fn escape_handles_trailing_backslash() {
    let sql = "SELECT ?";
    let out = replace_question_marks(sql, &[Value::String(r"path\".to_owned())]);
    assert!(
        out.contains(r"'path\\'"),
        "trailing backslash not escaped: {out}"
    );
}

/// Quote + backslash interaction: `'it\'s'` (input with both) should
/// produce `'it\\''s'` — backslash escaped, quote doubled.
#[test]
fn escape_handles_quote_then_backslash() {
    let sql = "SELECT ?";
    let out = replace_question_marks(sql, &[Value::String("it\\'s".to_owned())]);
    assert!(
        out.contains(r"'it\\''s'"),
        "quote+backslash not correctly escaped: {out}"
    );
}

/// Backslash inside a string literal in the SQL itself must NOT be
/// escaped — only parameter values are escaped.
#[test]
fn backslash_in_sql_literal_not_escaped() {
    // The `\n` in the literal is already a ClickHouse escape and
    // should be left alone. Only the `?` parameter is interpolated.
    let sql = r"SELECT 'line1\nline2', ?";
    let out = replace_question_marks(sql, &[Value::Int(1)]);
    assert!(
        out.contains(r"'line1\nline2'"),
        "SQL literal backslash should not be touched: {out}"
    );
    assert!(out.ends_with(", 1"), "param not substituted: {out}");
}

/// `substitute_params` must also escape backslashes in string values.
#[test]
fn substitute_params_escapes_backslash() {
    let sql = "SELECT $1";
    let out = substitute_params(sql, &[Value::String(r"C:\tmp".to_owned())]);
    assert!(
        out.contains(r"C:\\tmp"),
        "backslash not escaped via substitute_params: {out}"
    );
}
