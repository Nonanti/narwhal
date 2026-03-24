//! Inline unit tests for the treesitter module.
//!
//! The cross-module integration tests live in
//! `tests/treesitter.rs` and verify the public surface (parser ->
//! highlight + scope, incremental reparse correctness, property
//! tests against random edits, perf budget on a 10k-line buffer).

use super::*;

/// One-shot parser → owned tree handle. Tree-sitter `Tree` is
/// reference-counted so cloning the inner handle is cheap and lets
/// the test access spans after dropping the parser.
fn parse(src: &str) -> SqlTree {
    let mut parser = Parser::new().expect("grammar load");
    parser.parse(src).expect("parse");
    let raw = parser.tree().expect("tree set").raw().clone();
    SqlTree { inner: raw }
}

#[test]
fn smoke_select() {
    let src = "SELECT id FROM users;";
    let t = parse(src);
    let spans = t.highlights(src);
    assert!(
        spans
            .iter()
            .any(|s| s.kind == HighlightKind::Keyword && &src[s.byte_range.clone()] == "SELECT")
    );
    assert!(
        spans
            .iter()
            .any(|s| s.kind == HighlightKind::ColumnRef && &src[s.byte_range.clone()] == "id")
    );
    assert!(
        spans
            .iter()
            .any(|s| s.kind == HighlightKind::TableRef && &src[s.byte_range.clone()] == "users")
    );
    assert!(
        spans
            .iter()
            .any(|s| s.kind == HighlightKind::Punctuation && &src[s.byte_range.clone()] == ";")
    );
}

#[test]
fn highlight_spans_are_sorted_and_non_overlapping() {
    let src = "SELECT a, b + 1, 'hi' FROM t WHERE id = 42;";
    let t = parse(src);
    let spans = t.highlights(src);
    let mut last = 0;
    for s in &spans {
        assert!(s.byte_range.start >= last, "spans not sorted: {spans:#?}");
        assert!(s.byte_range.end > s.byte_range.start, "empty span: {s:?}");
        last = s.byte_range.end;
    }
}

#[test]
fn classifies_string_and_number_literals() {
    let src = "SELECT 'hello', 42, 1.5, NULL, TRUE FROM t;";
    let t = parse(src);
    let spans = t.highlights(src);
    let kinds: Vec<_> = spans
        .iter()
        .map(|s| (s.kind, &src[s.byte_range.clone()]))
        .collect();
    assert!(kinds.contains(&(HighlightKind::String, "'hello'")));
    assert!(kinds.contains(&(HighlightKind::Number, "42")));
    assert!(kinds.contains(&(HighlightKind::Number, "1.5")));
    assert!(kinds.contains(&(HighlightKind::Constant, "NULL")));
    assert!(kinds.contains(&(HighlightKind::Constant, "TRUE")));
}

#[test]
fn classifies_function_calls_and_aliases() {
    let src = "SELECT COUNT(*) AS cnt, u.name FROM users u;";
    let t = parse(src);
    let spans = t.highlights(src);
    let kinds: Vec<_> = spans
        .iter()
        .map(|s| (s.kind, &src[s.byte_range.clone()]))
        .collect();
    assert!(kinds.contains(&(HighlightKind::FunctionCall, "COUNT")));
    assert!(kinds.contains(&(HighlightKind::Alias, "cnt")));
    assert!(kinds.contains(&(HighlightKind::TableRef, "users")));
    assert!(kinds.contains(&(HighlightKind::Alias, "u")));
}

#[test]
fn comments_are_distinct_kinds() {
    let src = "-- line\n/* block */ SELECT 1;";
    let t = parse(src);
    let spans = t.highlights(src);
    assert!(spans.iter().any(
        |s| s.kind == HighlightKind::LineComment && src[s.byte_range.clone()].starts_with("--")
    ));
    assert!(
        spans.iter().any(|s| s.kind == HighlightKind::BlockComment
            && src[s.byte_range.clone()].starts_with("/*"))
    );
}

#[test]
fn highlights_in_range_clips() {
    let src = "SELECT 1; SELECT 2;";
    let t = parse(src);
    let all = t.highlights(src);
    let clipped = t.highlights_in_range(src, 10..src.len());
    // The clipped slice should drop the first SELECT.
    assert!(all.len() > clipped.len());
    assert!(clipped.iter().all(|s| s.byte_range.start >= 10));
}

#[test]
fn scope_select_projection() {
    let src = "SELECT id, name FROM users;";
    let t = parse(src);
    // Cursor inside the projection list ("name" starts at byte 11).
    let scope = t.scope_at(src, 12);
    assert_eq!(scope.kind, ScopeKind::SelectProjection);
}

#[test]
fn scope_from_and_where() {
    let src = "SELECT id FROM users WHERE age > 18;";
    let t = parse(src);
    // Inside FROM.
    let from = t.scope_at(src, 16);
    assert_eq!(from.kind, ScopeKind::From);
    // Inside WHERE predicate ("age").
    let wh = t.scope_at(src, 27);
    assert_eq!(wh.kind, ScopeKind::Where);
}

#[test]
fn scope_group_having_split() {
    let src = "SELECT a FROM t GROUP BY a HAVING COUNT(*) > 1;";
    let t = parse(src);
    // "a" after GROUP BY.
    let gb = t.scope_at(src, 25);
    assert_eq!(gb.kind, ScopeKind::GroupBy);
    // Inside the HAVING predicate.
    let having_byte = src.find("COUNT").expect("COUNT present");
    let hv = t.scope_at(src, having_byte + 1);
    assert_eq!(hv.kind, ScopeKind::Having);
}

#[test]
fn scope_join_table_vs_condition() {
    let src = "SELECT 1 FROM a JOIN b ON a.id = b.id;";
    let t = parse(src);
    let jt = t.scope_at(src, src.find('b').expect("b") + 1);
    assert_eq!(jt.kind, ScopeKind::JoinTable);
    let jc = t.scope_at(src, src.find("a.id").expect("a.id") + 1);
    assert_eq!(jc.kind, ScopeKind::JoinCondition);
}

#[test]
fn scope_insert_columns_vs_values() {
    let src = "INSERT INTO t (a, b) VALUES (1, 2);";
    let t = parse(src);
    let cols = t.scope_at(src, src.find("a, b").expect("cols") + 1);
    assert_eq!(cols.kind, ScopeKind::InsertColumns);
    let vals = t.scope_at(src, src.find("1, 2").expect("vals") + 1);
    assert_eq!(vals.kind, ScopeKind::InsertValues);
}

#[test]
fn scope_outside_statement() {
    let src = "  -- nothing here\n  ";
    let t = parse(src);
    let s = t.scope_at(src, 0);
    assert_eq!(s.kind, ScopeKind::None);
}

#[test]
fn incremental_reparse_after_insertion() {
    let mut parser = Parser::new().expect("grammar");
    let v1 = "SELECT a FROM t;";
    parser.parse(v1).expect("parse v1");

    // Insert ", b" after "a" -> "SELECT a, b FROM t;"
    let v2 = "SELECT a, b FROM t;";
    let edit = Edit::from_diff(v1, v2, 8, 8, 11);
    assert!(parser.edit(&edit));

    let tree2 = parser.reparse(v2).expect("reparse");
    let spans = tree2.highlights(v2);
    let column_refs: Vec<_> = spans
        .iter()
        .filter(|s| s.kind == HighlightKind::ColumnRef)
        .map(|s| &v2[s.byte_range.clone()])
        .collect();
    assert!(column_refs.contains(&"a"), "a in {column_refs:?}");
    assert!(column_refs.contains(&"b"), "b in {column_refs:?}");
}

proptest::proptest! {
    /// Random byte-level insertions must never produce an out-of-sync
    /// highlight pass: every emitted span must lie strictly within the
    /// post-edit buffer, with non-decreasing byte order. The walker
    /// itself enforces this; the property test guards against future
    /// regressions in the incremental path.
    #[test]
    fn incremental_reparse_stays_in_bounds(
        ops in proptest::collection::vec(
            (0usize..16usize, proptest::string::string_regex("[a-zA-Z0-9_, ]{0,8}").unwrap()),
            0..16,
        ),
    ) {
        let mut buf = String::from("SELECT a, b, c FROM t WHERE id = 1;");
        let mut parser = Parser::new().expect("grammar");
        parser.parse(&buf).expect("initial parse");

        for (pos, ins) in ops {
            let pos = pos.min(buf.len());
            // Snap to a char boundary so we never split a UTF-8 codepoint.
            let mut pos = pos;
            while !buf.is_char_boundary(pos) && pos < buf.len() {
                pos += 1;
            }
            let old = buf.clone();
            buf.insert_str(pos, &ins);
            let edit = Edit::from_diff(&old, &buf, pos, pos, pos + ins.len());
            parser.edit(&edit);
            let tree = parser.reparse(&buf).expect("reparse");
            for s in tree.highlights(&buf) {
                proptest::prop_assert!(s.byte_range.end <= buf.len());
                proptest::prop_assert!(s.byte_range.start < s.byte_range.end);
            }
        }
    }
}
