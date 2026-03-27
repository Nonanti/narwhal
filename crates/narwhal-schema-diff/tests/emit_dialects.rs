//! Per-dialect DDL emission smoke tests.
//!
//! Each test focuses on a single dialect-specific characteristic that
//! distinguishes the emitter from the ANSI fallback. The matrix is
//! deliberately narrow — broader coverage lives in
//! `tests/emit_generic.rs` and (future) round-trip tests against live
//! engines.

use narwhal_core::schema::{
    Column, ForeignKey, Index, ReferentialAction, Table, TableKind, TableSchema, UniqueConstraint,
};
use narwhal_schema_diff::diff;
use narwhal_schema_diff::emit::{
    DdlEmitter, MssqlEmitter, MysqlEmitter, PostgresEmitter, SqliteEmitter,
};

fn col(name: &str, ty: &str) -> Column {
    Column {
        name: name.into(),
        data_type: ty.into(),
        nullable: true,
        primary_key: false,
        default: None,
    }
}

fn col_not_null(name: &str, ty: &str) -> Column {
    let mut c = col(name, ty);
    c.nullable = false;
    c
}

fn col_default(name: &str, ty: &str, default: &str) -> Column {
    let mut c = col(name, ty);
    c.default = Some(default.into());
    c
}

fn table(name: &str, cols: Vec<Column>) -> TableSchema {
    TableSchema {
        table: Table {
            schema: "public".into(),
            name: name.into(),
            kind: TableKind::Table,
        },
        columns: cols,
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
        unique_constraints: Vec::new(),
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

// ---------------------------------------------------------------- postgres

#[test]
fn postgres_type_change_uses_using_cast() {
    let source = vec![table("u", vec![col("id", "int8")])];
    let target = vec![table("u", vec![col("id", "int4")])];
    let sql = PostgresEmitter::new()
        .emit(&diff(&source, &target))
        .unwrap();
    assert!(
        sql.contains("ALTER COLUMN id TYPE int8 USING id::int8"),
        "missing USING cast; got:\n{sql}"
    );
}

#[test]
fn postgres_changed_fk_dropped_before_recreate() {
    // The FK shape itself changes (referential action shift). The
    // emitter must drop the target form first, then add the source
    // form, so an in-flight transaction never sees two constraints
    // with the same name.
    let mut source = table("orders", vec![col("user_id", "int4")]);
    let mut fk_s = fk("fk_user", &["user_id"], "users", &["id"]);
    fk_s.on_delete = Some(ReferentialAction::Cascade);
    source.foreign_keys.push(fk_s);
    let mut target = table("orders", vec![col("user_id", "int4")]);
    let mut fk_t = fk("fk_user", &["user_id"], "users", &["id"]);
    fk_t.on_delete = Some(ReferentialAction::Restrict);
    target.foreign_keys.push(fk_t);

    let sql = PostgresEmitter::new()
        .emit(&diff(&[source], &[target]))
        .unwrap();
    let drop_pos = sql
        .find("DROP CONSTRAINT fk_user")
        .expect("expected DROP CONSTRAINT");
    let recreate_pos = sql.rfind("ADD CONSTRAINT fk_user").expect("recreate");
    assert!(
        drop_pos < recreate_pos,
        "drop must come before recreate:\n{sql}"
    );
    // The recreated form must carry the *source* cascade choice.
    assert!(
        sql[recreate_pos..].contains("ON DELETE CASCADE"),
        "recreated FK must reflect source side; got:\n{sql}"
    );
}

#[test]
fn postgres_default_set_and_drop() {
    // SET path.
    let with_def = vec![table("u", vec![col_default("c", "int4", "0")])];
    let without = vec![table("u", vec![col("c", "int4")])];
    let set = PostgresEmitter::new()
        .emit(&diff(&with_def, &without))
        .unwrap();
    assert!(set.contains("ALTER COLUMN c SET DEFAULT 0"), "got:\n{set}");

    // DROP path.
    let drop = PostgresEmitter::new()
        .emit(&diff(&without, &with_def))
        .unwrap();
    assert!(drop.contains("ALTER COLUMN c DROP DEFAULT"), "got:\n{drop}");
}

// ---------------------------------------------------------------- mysql

#[test]
fn mysql_coalesces_type_and_nullable_into_modify_column() {
    let source = vec![table("u", vec![col_not_null("c", "int8")])];
    let target = vec![table("u", vec![col("c", "int4")])];
    let sql = MysqlEmitter::new().emit(&diff(&source, &target)).unwrap();
    // One MODIFY COLUMN statement carries both the new type and the
    // NOT NULL — not two.
    let modify_count = sql.matches("MODIFY COLUMN c").count();
    assert_eq!(modify_count, 1, "expected one MODIFY COLUMN; got:\n{sql}");
    assert!(sql.contains("MODIFY COLUMN c int8 NOT NULL"), "got:\n{sql}");
}

#[test]
fn mysql_drops_foreign_key_with_dedicated_syntax() {
    let mut target = table("orders", vec![col("user_id", "int4")]);
    target
        .foreign_keys
        .push(fk("fk_user", &["user_id"], "users", &["id"]));
    let source = table("orders", vec![col("user_id", "int4")]);
    let sql = MysqlEmitter::new()
        .emit(&diff(&[source], &[target]))
        .unwrap();
    assert!(
        sql.contains("DROP FOREIGN KEY fk_user"),
        "MySQL uses DROP FOREIGN KEY, not DROP CONSTRAINT; got:\n{sql}"
    );
}

#[test]
fn mysql_drop_index_includes_on_table() {
    let mut target = table("u", vec![col("a", "int4")]);
    target.indexes.push(Index {
        name: "u_a_idx".into(),
        columns: vec!["a".into()],
        unique: false,
        primary: false,
    });
    let source = table("u", vec![col("a", "int4")]);
    let sql = MysqlEmitter::new()
        .emit(&diff(&[source], &[target]))
        .unwrap();
    assert!(
        sql.contains("DROP INDEX u_a_idx ON public.u"),
        "DROP INDEX must include ON <table>; got:\n{sql}"
    );
}

#[test]
fn mysql_modify_without_type_emits_placeholder_comment() {
    // M4.2: Only a nullable change with no type delta — MySQL MODIFY
    // COLUMN requires the full type, so we cannot emit a valid
    // statement. Instead, emit a TODO comment so the user knows
    // what to fill in.
    let source = vec![table("u", vec![col_not_null("c", "int4")])];
    let target = vec![table("u", vec![col("c", "int4")])];
    let sql = MysqlEmitter::new().emit(&diff(&source, &target)).unwrap();
    assert!(!sql.contains("/*"), "inline comment is invalid SQL: {sql}");
    assert!(
        !sql.contains("MODIFY COLUMN"),
        "no MODIFY COLUMN without type: {sql}"
    );
    assert!(
        sql.contains("-- TODO: type required to MODIFY column 'c'"),
        "expected TODO comment, got:\n{sql}"
    );
}

// ---------------------------------------------------------------- sqlite

#[test]
fn sqlite_type_change_is_a_comment_not_a_statement() {
    let source = vec![table("u", vec![col("id", "int8")])];
    let target = vec![table("u", vec![col("id", "int4")])];
    let sql = SqliteEmitter::new().emit(&diff(&source, &target)).unwrap();
    assert!(
        sql.contains("table rebuild needed: change type of public.u.id"),
        "expected rebuild comment; got:\n{sql}"
    );
    // No ALTER COLUMN *statement* — SQLite would reject it. The
    // header comment mentions the limitation but is, well, a
    // comment. Strip comment lines before searching to avoid the
    // false positive.
    let non_comment: String = sql
        .lines()
        .filter(|l| !l.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !non_comment.contains("ALTER COLUMN"),
        "SQLite cannot ALTER COLUMN; got non-comment lines:\n{non_comment}\n\nfull:\n{sql}"
    );
}

#[test]
fn sqlite_nullable_change_is_a_comment() {
    let source = vec![table("u", vec![col_not_null("c", "int4")])];
    let target = vec![table("u", vec![col("c", "int4")])];
    let sql = SqliteEmitter::new().emit(&diff(&source, &target)).unwrap();
    assert!(
        sql.contains("table rebuild needed: set public.u.c NOT NULL"),
        "got:\n{sql}"
    );
}

#[test]
fn sqlite_drop_column_warns_about_version() {
    let source = vec![table("u", vec![col("a", "int4")])];
    let target = vec![table("u", vec![col("a", "int4"), col("legacy", "text")])];
    let sql = SqliteEmitter::new().emit(&diff(&source, &target)).unwrap();
    assert!(
        sql.contains("DROP COLUMN legacy;  -- requires SQLite >= 3.35"),
        "got:\n{sql}"
    );
}

#[test]
fn sqlite_add_column_works_natively() {
    let source = vec![table("u", vec![col("a", "int4"), col("b", "text")])];
    let target = vec![table("u", vec![col("a", "int4")])];
    let sql = SqliteEmitter::new().emit(&diff(&source, &target)).unwrap();
    assert!(
        sql.contains("ALTER TABLE public.u ADD COLUMN b text"),
        "got:\n{sql}"
    );
}

#[test]
fn sqlite_index_create_drop_native() {
    let mut source = table("u", vec![col("a", "int4")]);
    source.indexes.push(Index {
        name: "u_a_idx".into(),
        columns: vec!["a".into()],
        unique: false,
        primary: false,
    });
    let target = table("u", vec![col("a", "int4")]);
    let sql = SqliteEmitter::new()
        .emit(&diff(&[source], &[target]))
        .unwrap();
    assert!(
        sql.contains("CREATE INDEX u_a_idx ON public.u (a)"),
        "got:\n{sql}"
    );
}

#[test]
fn sqlite_create_table_includes_fk_inline() {
    let mut t = table("orders", vec![col("user_id", "int4")]);
    t.foreign_keys
        .push(fk("fk_user", &["user_id"], "users", &["id"]));
    let sql = SqliteEmitter::new().emit(&diff(&[t], &[])).unwrap();
    // SQLite-style: FK declared inside the table body, not as a
    // separate ALTER statement.
    assert!(
        sql.contains("FOREIGN KEY (user_id) REFERENCES users (id)"),
        "got:\n{sql}"
    );
    assert!(
        !sql.contains("ADD CONSTRAINT fk_user"),
        "FK should not be a separate ALTER; got:\n{sql}"
    );
}

// ---------------------------------------------------------------- mssql

#[test]
fn mssql_default_is_a_named_constraint() {
    // Set a default on a column that previously had none.
    let source = vec![table("u", vec![col_default("c", "int4", "0")])];
    let target = vec![table("u", vec![col("c", "int4")])];
    let sql = MssqlEmitter::new().emit(&diff(&source, &target)).unwrap();
    assert!(
        sql.contains("ADD CONSTRAINT df_u_c DEFAULT 0 FOR c"),
        "expected named DEFAULT constraint; got:\n{sql}"
    );
    // The engine cannot SET DEFAULT in ALTER COLUMN — that form
    // must never appear in MSSQL output.
    assert!(
        !sql.contains("SET DEFAULT"),
        "MSSQL emitter must not use ALTER COLUMN SET DEFAULT; got:\n{sql}"
    );
}

#[test]
fn mssql_default_change_drops_then_recreates() {
    let source = vec![table("u", vec![col_default("c", "int4", "1")])];
    let target = vec![table("u", vec![col_default("c", "int4", "0")])];
    let sql = MssqlEmitter::new().emit(&diff(&source, &target)).unwrap();
    let drop_pos = sql.find("DROP CONSTRAINT df_u_c").expect("drop");
    let add_pos = sql
        .find("ADD CONSTRAINT df_u_c DEFAULT 1 FOR c")
        .expect("add");
    assert!(drop_pos < add_pos, "drop must precede add; got:\n{sql}");
}

#[test]
fn mssql_coalesces_type_and_nullable() {
    let source = vec![table("u", vec![col_not_null("c", "bigint")])];
    let target = vec![table("u", vec![col("c", "int")])];
    let sql = MssqlEmitter::new().emit(&diff(&source, &target)).unwrap();
    assert!(
        sql.contains("ALTER COLUMN c bigint NOT NULL"),
        "expected one coalesced ALTER COLUMN; got:\n{sql}"
    );
}

#[test]
fn mssql_drop_index_requires_on_table() {
    let mut target = table("u", vec![col("a", "int")]);
    target.indexes.push(Index {
        name: "ix_u_a".into(),
        columns: vec!["a".into()],
        unique: false,
        primary: false,
    });
    let source = table("u", vec![col("a", "int")]);
    let sql = MssqlEmitter::new()
        .emit(&diff(&[source], &[target]))
        .unwrap();
    assert!(
        sql.contains("DROP INDEX ix_u_a ON public.u"),
        "MSSQL DROP INDEX needs ON <table>; got:\n{sql}"
    );
}

#[test]
fn mssql_create_table_inline_default_uses_named_constraint() {
    let source = vec![table("u", vec![col_default("c", "int", "0")])];
    let sql = MssqlEmitter::new().emit(&diff(&source, &[])).unwrap();
    assert!(
        sql.contains("c int CONSTRAINT df_u_c DEFAULT 0"),
        "inline default must carry the named constraint; got:\n{sql}"
    );
}

#[test]
fn mssql_fk_drop_uses_drop_constraint() {
    let mut target = table("orders", vec![col("user_id", "int")]);
    target
        .foreign_keys
        .push(fk("fk_user", &["user_id"], "users", &["id"]));
    let source = table("orders", vec![col("user_id", "int")]);
    let sql = MssqlEmitter::new()
        .emit(&diff(&[source], &[target]))
        .unwrap();
    assert!(
        sql.contains("DROP CONSTRAINT fk_user"),
        "MSSQL uses DROP CONSTRAINT; got:\n{sql}"
    );
    assert!(
        !sql.contains("DROP FOREIGN KEY"),
        "DROP FOREIGN KEY is MySQL-only; got:\n{sql}"
    );
}

// ---------------------------------------------------------------- ordering

#[test]
fn postgres_unique_constraint_change_recreates_after_drop() {
    let mut source = table("u", vec![col("a", "int4"), col("b", "int4")]);
    source.unique_constraints.push(UniqueConstraint {
        name: "u_ab".into(),
        columns: vec!["a".into(), "b".into()],
    });
    let mut target = table("u", vec![col("a", "int4"), col("b", "int4")]);
    target.unique_constraints.push(UniqueConstraint {
        name: "u_ab".into(),
        columns: vec!["b".into(), "a".into()],
    });
    let sql = PostgresEmitter::new()
        .emit(&diff(&[source], &[target]))
        .unwrap();
    let drop_pos = sql.find("DROP CONSTRAINT u_ab").expect("drop");
    let add_pos = sql.rfind("ADD CONSTRAINT u_ab").expect("add");
    assert!(drop_pos < add_pos, "got:\n{sql}");
}

#[test]
fn postgres_referential_action_preserved() {
    let mut source = table("orders", vec![col("user_id", "int4")]);
    let mut fk_s = fk("fk_user", &["user_id"], "users", &["id"]);
    fk_s.on_delete = Some(ReferentialAction::Cascade);
    source.foreign_keys.push(fk_s);
    let sql = PostgresEmitter::new().emit(&diff(&[source], &[])).unwrap();
    assert!(sql.contains("ON DELETE CASCADE"), "got:\n{sql}");
}
