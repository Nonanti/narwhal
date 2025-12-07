//! Regression tests for `unique_constraints_from_indexes` (bug M10).
//!
//! `describe_table` filtered UNIQUE constraints with `columns.len() > 1`,
//! which silently dropped every single-column UNIQUE. The Postgres
//! driver lists all UNIQUE constraints irrespective of arity, so the
//! application-level "UNIQUE constraints" view differed across drivers
//! depending on which one you connected with.
//!
//! The fix removes the arity filter so single-column UNIQUE constraints
//! survive.

use narwhal_core::Index;
use narwhal_driver_mysql::__test_only::unique_constraints_from_indexes;

fn idx(name: &str, columns: &[&str], unique: bool, primary: bool) -> Index {
    Index {
        name: name.to_owned(),
        columns: columns.iter().map(|c| (*c).to_owned()).collect(),
        unique,
        primary,
    }
}

#[test]
fn includes_single_column_unique_index() {
    let indexes = vec![idx("uq_email", &["email"], true, false)];
    let out = unique_constraints_from_indexes(&indexes);
    assert_eq!(out.len(), 1, "expected one UNIQUE, got {out:?}");
    assert_eq!(out[0].name, "uq_email");
    assert_eq!(out[0].columns, vec!["email".to_owned()]);
}

#[test]
fn includes_multi_column_unique_index() {
    let indexes = vec![idx("uq_pair", &["a", "b"], true, false)];
    let out = unique_constraints_from_indexes(&indexes);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].columns, vec!["a".to_owned(), "b".to_owned()]);
}

#[test]
fn excludes_primary_key_even_if_unique() {
    let indexes = vec![idx("PRIMARY", &["id"], true, true)];
    let out = unique_constraints_from_indexes(&indexes);
    assert!(out.is_empty(), "primary key must not appear: {out:?}");
}

#[test]
fn excludes_non_unique_index() {
    let indexes = vec![idx("ix_lookup", &["name"], false, false)];
    let out = unique_constraints_from_indexes(&indexes);
    assert!(out.is_empty(), "non-unique index must not appear: {out:?}");
}

#[test]
fn preserves_order_of_indexes() {
    let indexes = vec![
        idx("uq_a", &["a"], true, false),
        idx("ix_b", &["b"], false, false),
        idx("uq_cd", &["c", "d"], true, false),
        idx("PRIMARY", &["id"], true, true),
    ];
    let out = unique_constraints_from_indexes(&indexes);
    let names: Vec<_> = out.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["uq_a", "uq_cd"]);
}
