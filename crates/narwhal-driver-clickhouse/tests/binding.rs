//! Regression tests for `replace_question_marks` (bug C2).
//!
//! The previous implementation iterated `sql.as_bytes()` and pushed each
//! byte as `c as char`. That works for ASCII but mangles any multi-byte
//! UTF-8 sequence into a sequence of Latin-1 code points
//! (`kullanıcılar` -> `kullanÄ±cÄ±lar`). Whenever the SQL string also
//! had parameters, `ClickHouse` received the corrupted identifier and
//! returned `Unknown table`.
//!
//! Separately, the `$N` placeholder path was triggered by *any* literal
//! `$` in the SQL (including inside a quoted string like `'$1.99'`),
//! which corrupted unrelated text.

use narwhal_core::Value;
use narwhal_driver_clickhouse::__test_only::{replace_question_marks, substitute_params};

#[test]
fn replace_question_marks_preserves_non_ascii_identifier() {
    let sql = "SELECT * FROM \"kullanıcılar\" WHERE ad = ?";
    let out = replace_question_marks(sql, &[Value::String("ali".to_owned())]);
    assert!(
        out.contains("kullanıcılar"),
        "non-ASCII identifier mangled: {out}"
    );
    assert!(out.ends_with("= 'ali'"), "param not substituted: {out}");
}

#[test]
fn replace_question_marks_preserves_non_ascii_in_string_literal() {
    let sql = "SELECT 'çöğşüı' AS x, ?";
    let out = replace_question_marks(sql, &[Value::Int(1)]);
    assert!(out.contains("çöğşüı"), "literal mangled: {out}");
    assert!(out.ends_with(", 1"), "param not substituted: {out}");
}

#[test]
fn replace_question_marks_preserves_emoji() {
    let sql = "SELECT '🦀 narwhal' AS x, ?";
    let out = replace_question_marks(sql, &[Value::Int(42)]);
    assert!(out.contains("🦀 narwhal"), "emoji mangled: {out}");
    assert!(out.ends_with(", 42"));
}

#[test]
fn substitute_params_does_not_misfire_on_dollar_in_literal() {
    // The `$1` here is *inside a string literal* so it must not trigger
    // the `$N` substitution path. The `?` outside the literal is the
    // only real placeholder.
    let sql = "SELECT '$1.99' AS price, ?";
    let out = substitute_params(sql, &[Value::String("usd".to_owned())]);
    assert!(
        out.contains("'$1.99'"),
        "dollar-amount literal corrupted: {out}"
    );
    assert!(out.contains("'usd'"), "param not substituted: {out}");
}

#[test]
fn replace_question_marks_does_not_replace_inside_string_literal() {
    let sql = "SELECT 'a?b' AS x, ?";
    let out = replace_question_marks(sql, &[Value::Int(1)]);
    assert!(out.contains("'a?b'"), "literal `?` was replaced: {out}");
    assert!(out.ends_with(", 1"));
}
