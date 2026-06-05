//! Diff-algorithm test matrix.
//!
//! Each test names the scenario in the function name and asserts on
//! the *shape* of the [`SchemaDiff`] — we do not lean on `Debug`
//! string equality so a future refactor that adds new variants can
//! land without rewriting the matrix.

use narwhal_core::schema::{
    Column, ForeignKey, Index, ReferentialAction, Table, TableKind, TableSchema, UniqueConstraint,
};
use narwhal_schema_diff::{
    ColumnChange, ForeignKeyChange, IndexChange, TableChange, UniqueConstraintChange, diff,
};

// ---------------------------------------------------------------- helpers

fn t(schema: &str, name: &str, columns: Vec<Column>) -> TableSchema {
    TableSchema {
        table: Table {
            schema: schema.into(),
            name: name.into(),
            kind: TableKind::Table,
        },
        columns,
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
        unique_constraints: Vec::new(),
    }
}

fn col(name: &str, data_type: &str) -> Column {
    Column {
        name: name.into(),
        data_type: data_type.into(),
        nullable: true,
        primary_key: false,
        default: None,
    }
}

fn col_not_null(name: &str, data_type: &str) -> Column {
    let mut c = col(name, data_type);
    c.nullable = false;
    c
}

fn col_default(name: &str, data_type: &str, default: &str) -> Column {
    let mut c = col(name, data_type);
    c.default = Some(default.into());
    c
}

fn idx(name: &str, columns: &[&str], unique: bool) -> Index {
    Index {
        name: name.into(),
        columns: columns.iter().map(|s| (*s).to_owned()).collect(),
        unique,
        primary: false,
    }
}

fn fk(name: &str, cols: &[&str], ref_table: &str, ref_cols: &[&str]) -> ForeignKey {
    ForeignKey {
        name: name.into(),
        columns: cols.iter().map(|s| (*s).to_owned()).collect(),
        referenced_schema: None,
        referenced_table: ref_table.into(),
        referenced_columns: ref_cols.iter().map(|s| (*s).to_owned()).collect(),
        on_update: None,
        on_delete: None,
    }
}

// ---------------------------------------------------------------- tests

#[test]
fn empty_equal_inputs_yields_empty_diff() {
    let d = diff(&[], &[]);
    assert!(d.is_empty());
    assert_eq!(d.change_count(), 0);
}

#[test]
fn single_table_added() {
    let source = vec![t("public", "users", vec![col("id", "int4")])];
    let d = diff(&source, &[]);
    assert_eq!(d.tables.len(), 1);
    assert!(matches!(d.tables[0], TableChange::Added(_)));
}

#[test]
fn single_table_removed() {
    let target = vec![t("public", "users", vec![col("id", "int4")])];
    let d = diff(&[], &target);
    assert_eq!(d.tables.len(), 1);
    assert!(matches!(d.tables[0], TableChange::Removed(_)));
}

#[test]
fn identical_tables_yield_no_diff() {
    let s = vec![t("public", "users", vec![col("id", "int4")])];
    let d = diff(&s, &s);
    assert!(d.is_empty());
}

#[test]
fn column_added_to_existing_table() {
    let source = vec![t(
        "public",
        "users",
        vec![col("id", "int4"), col("email", "text")],
    )];
    let target = vec![t("public", "users", vec![col("id", "int4")])];
    let d = diff(&source, &target);
    let TableChange::Changed { columns, .. } = &d.tables[0] else {
        panic!("expected Changed, got {:?}", d.tables[0]);
    };
    assert_eq!(columns.len(), 1);
    assert!(matches!(columns[0], ColumnChange::Added(_)));
}

#[test]
fn column_removed_from_existing_table() {
    let source = vec![t("public", "users", vec![col("id", "int4")])];
    let target = vec![t(
        "public",
        "users",
        vec![col("id", "int4"), col("legacy_field", "text")],
    )];
    let d = diff(&source, &target);
    let TableChange::Changed { columns, .. } = &d.tables[0] else {
        panic!();
    };
    assert!(matches!(columns[0], ColumnChange::Removed(_)));
}

#[test]
fn column_type_changed() {
    let source = vec![t("public", "u", vec![col("id", "int8")])];
    let target = vec![t("public", "u", vec![col("id", "int4")])];
    let d = diff(&source, &target);
    let TableChange::Changed { columns, .. } = &d.tables[0] else {
        panic!();
    };
    assert!(matches!(columns[0], ColumnChange::TypeChanged { .. }));
}

#[test]
fn type_change_ignores_synonyms() {
    // `character varying(255)` canonicalises to `varchar(255)`.
    let source = vec![t(
        "public",
        "u",
        vec![col("email", "character varying(255)")],
    )];
    let target = vec![t("public", "u", vec![col("email", "varchar(255)")])];
    let d = diff(&source, &target);
    assert!(d.is_empty(), "synonyms must not register as a diff: {d:?}");
}

#[test]
fn type_change_ignores_whitespace_and_case() {
    let source = vec![t("public", "u", vec![col("c", "INTEGER")])];
    let target = vec![t("public", "u", vec![col("c", "int4")])];
    let d = diff(&source, &target);
    assert!(d.is_empty());
}

#[test]
fn nullable_change_detected() {
    let source = vec![t("public", "u", vec![col_not_null("c", "int4")])];
    let target = vec![t("public", "u", vec![col("c", "int4")])];
    let d = diff(&source, &target);
    let TableChange::Changed { columns, .. } = &d.tables[0] else {
        panic!();
    };
    assert!(matches!(
        columns[0],
        ColumnChange::NullableChanged {
            source: false,
            target: true,
            ..
        }
    ));
}

#[test]
fn default_change_detected() {
    let source = vec![t("public", "u", vec![col_default("c", "int4", "1")])];
    let target = vec![t("public", "u", vec![col("c", "int4")])];
    let d = diff(&source, &target);
    let TableChange::Changed { columns, .. } = &d.tables[0] else {
        panic!();
    };
    assert!(matches!(columns[0], ColumnChange::DefaultChanged { .. }));
}

#[test]
fn default_change_ignores_parens_and_case() {
    let source = vec![t("public", "u", vec![col_default("c", "int4", "(now())")])];
    let target = vec![t("public", "u", vec![col_default("c", "int4", "NOW()")])];
    let d = diff(&source, &target);
    assert!(d.is_empty(), "expected no diff, got {d:?}");
}

#[test]
fn default_null_equals_no_default() {
    let source = vec![t("public", "u", vec![col_default("c", "int4", "NULL")])];
    let target = vec![t("public", "u", vec![col("c", "int4")])];
    let d = diff(&source, &target);
    assert!(
        d.is_empty(),
        "NULL default must equal no default; got {d:?}"
    );
}

#[test]
fn system_schemas_are_filtered() {
    let target = vec![t("pg_catalog", "pg_class", vec![col("oid", "oid")])];
    let d = diff(&[], &target);
    assert!(d.is_empty());
}

#[test]
fn user_schema_with_pg_prefix_survives_filter() {
    // `pg_catalog_clone` is not the same as `pg_catalog`.
    let target = vec![t("pg_catalog_clone", "pg_class", vec![col("oid", "oid")])];
    let d = diff(&[], &target);
    assert_eq!(d.tables.len(), 1);
}

#[test]
fn index_added() {
    let mut s = t("public", "u", vec![col("id", "int4")]);
    s.indexes.push(idx("u_idx", &["id"], false));
    let target = t("public", "u", vec![col("id", "int4")]);
    let d = diff(&[s], &[target]);
    let TableChange::Changed { indexes, .. } = &d.tables[0] else {
        panic!();
    };
    assert!(matches!(indexes[0], IndexChange::Added(_)));
}

#[test]
fn index_changed_shape() {
    let mut source = t("public", "u", vec![col("a", "int4"), col("b", "int4")]);
    source.indexes.push(idx("u_idx", &["a", "b"], false));
    let mut target = t("public", "u", vec![col("a", "int4"), col("b", "int4")]);
    target.indexes.push(idx("u_idx", &["b", "a"], false)); // different order
    let d = diff(&[source], &[target]);
    let TableChange::Changed { indexes, .. } = &d.tables[0] else {
        panic!();
    };
    assert!(matches!(indexes[0], IndexChange::Changed { .. }));
}

#[test]
fn implicit_primary_index_is_ignored() {
    let mut both = t("public", "u", vec![col("id", "int4")]);
    both.indexes.push(Index {
        name: "u_pkey".into(),
        columns: vec!["id".into()],
        unique: true,
        primary: true,
    });
    let d = diff(&[both.clone()], &[both]);
    assert!(d.is_empty());
}

#[test]
fn foreign_key_added() {
    let mut source = t("public", "orders", vec![col("user_id", "int4")]);
    source
        .foreign_keys
        .push(fk("fk_user", &["user_id"], "users", &["id"]));
    let target = t("public", "orders", vec![col("user_id", "int4")]);
    let d = diff(&[source], &[target]);
    let TableChange::Changed { foreign_keys, .. } = &d.tables[0] else {
        panic!();
    };
    assert!(matches!(foreign_keys[0], ForeignKeyChange::Added(_)));
}

#[test]
fn foreign_key_referential_action_change() {
    let mut source = t("public", "orders", vec![col("user_id", "int4")]);
    let mut fk_s = fk("fk_user", &["user_id"], "users", &["id"]);
    fk_s.on_delete = Some(ReferentialAction::Cascade);
    source.foreign_keys.push(fk_s);
    let mut target = t("public", "orders", vec![col("user_id", "int4")]);
    let mut fk_t = fk("fk_user", &["user_id"], "users", &["id"]);
    fk_t.on_delete = Some(ReferentialAction::Restrict);
    target.foreign_keys.push(fk_t);
    let d = diff(&[source], &[target]);
    let TableChange::Changed { foreign_keys, .. } = &d.tables[0] else {
        panic!();
    };
    assert!(matches!(foreign_keys[0], ForeignKeyChange::Changed { .. }));
}

#[test]
fn foreign_key_no_action_equals_none() {
    // `None` and `Some(NoAction)` mean the same thing in SQL.
    let mut source = t("public", "orders", vec![col("user_id", "int4")]);
    let mut fk_s = fk("fk_user", &["user_id"], "users", &["id"]);
    fk_s.on_delete = Some(ReferentialAction::NoAction);
    source.foreign_keys.push(fk_s);
    let mut target = t("public", "orders", vec![col("user_id", "int4")]);
    let mut fk_t = fk("fk_user", &["user_id"], "users", &["id"]);
    fk_t.on_delete = None;
    target.foreign_keys.push(fk_t);
    let d = diff(&[source], &[target]);
    assert!(d.is_empty());
}

#[test]
fn unique_constraint_added() {
    let mut source = t("public", "u", vec![col("a", "int4"), col("b", "int4")]);
    source.unique_constraints.push(UniqueConstraint {
        name: "u_ab".into(),
        columns: vec!["a".into(), "b".into()],
    });
    let target = t("public", "u", vec![col("a", "int4"), col("b", "int4")]);
    let d = diff(&[source], &[target]);
    let TableChange::Changed {
        unique_constraints, ..
    } = &d.tables[0]
    else {
        panic!();
    };
    assert!(matches!(
        unique_constraints[0],
        UniqueConstraintChange::Added(_)
    ));
}

#[test]
fn output_is_deterministic_across_input_order() {
    let a = t("public", "users", vec![col("id", "int4")]);
    let b = t("public", "orders", vec![col("id", "int4")]);
    let d1 = diff(&[a.clone(), b.clone()], &[]);
    let d2 = diff(&[b, a], &[]);
    assert_eq!(d1, d2);
    assert_eq!(d1.tables[0].qualified_name(), ("public", "orders"));
    assert_eq!(d1.tables[1].qualified_name(), ("public", "users"));
}

#[test]
fn cross_schema_tables_kept_separate() {
    let s1 = t("staging", "users", vec![col("id", "int4")]);
    let s2 = t("prod", "users", vec![col("id", "int4")]);
    let d = diff(&[s1, s2], &[]);
    assert_eq!(d.tables.len(), 2);
}

#[test]
fn multiple_column_changes_aggregate() {
    let source = vec![t(
        "public",
        "u",
        vec![
            col_not_null("id", "int4"),
            col("email", "text"),
            col_default("created_at", "timestamp", "now()"),
        ],
    )];
    let target = vec![t(
        "public",
        "u",
        vec![col("id", "int4"), col("legacy", "text")],
    )];
    let d = diff(&source, &target);
    let TableChange::Changed { columns, .. } = &d.tables[0] else {
        panic!();
    };
    // id: nullable change; email: added; legacy: removed; created_at: added.
    assert_eq!(columns.len(), 4, "got: {columns:?}");
}

#[test]
fn change_count_aggregates_across_tables() {
    let source = vec![
        t("public", "a", vec![col("x", "int4")]),
        t("public", "b", vec![col("y", "int4"), col("z", "int4")]),
    ];
    let target = vec![t("public", "b", vec![col("y", "int4")])];
    let d = diff(&source, &target);
    // a: added (1) + b: column added (1) = 2 changes
    assert_eq!(d.change_count(), 2);
}
