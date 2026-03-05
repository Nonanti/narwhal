//! Per-table schema diff and `ALTER TABLE` generation (v1.2 #8).
//!
//! Inputs are two [`TableSchema`]s describing the same logical table
//! in two contexts (a stored snapshot vs. live; staging vs. prod;
//! `before` vs. `after` of a migration). The output is a list of
//! `ALTER TABLE` statements that morphs the first into the second.
//!
//! Scope for v1.2:
//!
//! - Columns: detect adds, drops, type changes, nullability flips,
//!   default changes.
//! - Indexes: not in scope. PG / `MySQL` syntax diverges too much for
//!   a one-pass renderer; deferred to v1.3.
//! - Foreign keys: not in scope (same reason).
//!
//! The renderer quotes identifiers with the dialect's native style
//! and emits `ALTER TABLE ... ADD/DROP/ALTER COLUMN` per change so
//! the result can be executed statement-by-statement.

use narwhal_core::{Column, TableSchema};
use narwhal_sql::Dialect;

use crate::ddl::quote_ident_public as quote_ident;
use crate::ddl::quote_qualified_public as quote_qualified;

/// One change between two [`TableSchema`]s.
#[derive(Debug, Clone)]
pub enum ColumnChange {
    /// Column present in `after` but not in `before`.
    Added { column: Column },
    /// Column present in `before` but not in `after`.
    Dropped { name: String },
    /// `data_type`, `nullable`, or `default` differs.
    Modified {
        name: String,
        from: Box<Column>,
        to: Box<Column>,
    },
}

/// Compute the column-level changes between two schemas.
///
/// Matching is by column name (case-sensitive). Position changes are
/// not considered; SQL columns are unordered for the purposes of most
/// engines.
#[must_use]
pub fn diff_columns(before: &TableSchema, after: &TableSchema) -> Vec<ColumnChange> {
    let mut out = Vec::new();
    for new in &after.columns {
        match before.columns.iter().find(|c| c.name == new.name) {
            None => out.push(ColumnChange::Added {
                column: new.clone(),
            }),
            Some(old) if columns_equivalent(old, new) => {}
            Some(old) => out.push(ColumnChange::Modified {
                name: new.name.clone(),
                from: Box::new(old.clone()),
                to: Box::new(new.clone()),
            }),
        }
    }
    for old in &before.columns {
        if !after.columns.iter().any(|c| c.name == old.name) {
            out.push(ColumnChange::Dropped {
                name: old.name.clone(),
            });
        }
    }
    out
}

/// Render the diff between two schemas as a list of `ALTER TABLE`
/// statements. Returns one string per statement so the caller can
/// preview them individually and feed them through the same dispatch
/// path as user-typed SQL.
#[must_use]
pub fn render_alter_statements(
    before: &TableSchema,
    after: &TableSchema,
    dialect: Dialect,
) -> Vec<String> {
    let table = quote_qualified(&after.table.schema, &after.table.name, dialect);
    let mut out = Vec::new();
    for change in diff_columns(before, after) {
        match change {
            ColumnChange::Added { column } => {
                let mut line = format!(
                    "ALTER TABLE {table} ADD COLUMN {} {}",
                    quote_ident(&column.name, dialect),
                    column.data_type
                );
                if !column.nullable {
                    line.push_str(" NOT NULL");
                }
                if let Some(d) = &column.default {
                    line.push_str(" DEFAULT ");
                    line.push_str(d);
                }
                line.push(';');
                out.push(line);
            }
            ColumnChange::Dropped { name } => {
                out.push(format!(
                    "ALTER TABLE {table} DROP COLUMN {};",
                    quote_ident(&name, dialect)
                ));
            }
            ColumnChange::Modified { name, from, to } => {
                let q = quote_ident(&name, dialect);
                if from.data_type != to.data_type {
                    // PG and CH require `USING` for non-trivial casts;
                    // we emit the canonical syntax and let the user
                    // adjust if needed.
                    out.push(match dialect {
                        Dialect::MySql => {
                            format!("ALTER TABLE {table} MODIFY COLUMN {q} {};", to.data_type)
                        }
                        _ => format!(
                            "ALTER TABLE {table} ALTER COLUMN {q} TYPE {};",
                            to.data_type
                        ),
                    });
                }
                if from.nullable != to.nullable {
                    let verb = if to.nullable {
                        "DROP NOT NULL"
                    } else {
                        "SET NOT NULL"
                    };
                    out.push(match dialect {
                        Dialect::MySql => format!(
                            "ALTER TABLE {table} MODIFY COLUMN {q} {} {};",
                            to.data_type,
                            if to.nullable { "NULL" } else { "NOT NULL" }
                        ),
                        _ => format!("ALTER TABLE {table} ALTER COLUMN {q} {verb};"),
                    });
                }
                if from.default != to.default {
                    match (&to.default, dialect) {
                        (Some(d), Dialect::MySql) => out.push(format!(
                            "ALTER TABLE {table} ALTER COLUMN {q} SET DEFAULT {d};"
                        )),
                        (Some(d), _) => out.push(format!(
                            "ALTER TABLE {table} ALTER COLUMN {q} SET DEFAULT {d};"
                        )),
                        (None, _) => out.push(format!(
                            "ALTER TABLE {table} ALTER COLUMN {q} DROP DEFAULT;"
                        )),
                    }
                }
            }
        }
    }
    out
}

fn columns_equivalent(a: &Column, b: &Column) -> bool {
    a.data_type == b.data_type && a.nullable == b.nullable && a.default == b.default
}

#[cfg(test)]
mod tests {
    use super::*;
    use narwhal_core::{Column, Table, TableKind, TableSchema};

    fn col(name: &str, ty: &str, nullable: bool) -> Column {
        Column {
            name: name.to_owned(),
            data_type: ty.to_owned(),
            nullable,
            default: None,
            primary_key: false,
        }
    }

    fn schema(cols: Vec<Column>) -> TableSchema {
        TableSchema {
            table: Table {
                schema: "public".into(),
                name: "t".into(),
                kind: TableKind::Table,
            },
            columns: cols,
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
            unique_constraints: Vec::new(),
        }
    }

    #[test]
    fn detects_added_dropped_modified() {
        let before = schema(vec![col("id", "INT", false), col("name", "TEXT", true)]);
        let after = schema(vec![
            col("id", "BIGINT", false),
            col("created_at", "TIMESTAMP", true),
        ]);
        let changes = diff_columns(&before, &after);
        assert_eq!(
            changes.len(),
            3,
            "id modified, created_at added, name dropped"
        );
        assert!(changes
            .iter()
            .any(|c| matches!(c, ColumnChange::Modified { name, .. } if name == "id")));
        assert!(changes
            .iter()
            .any(|c| matches!(c, ColumnChange::Added { column } if column.name == "created_at")));
        assert!(changes
            .iter()
            .any(|c| matches!(c, ColumnChange::Dropped { name } if name == "name")));
    }

    #[test]
    fn pg_alter_renders_type_change() {
        let before = schema(vec![col("id", "INT", false)]);
        let after = schema(vec![col("id", "BIGINT", false)]);
        let stmts = render_alter_statements(&before, &after, Dialect::Postgres);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("ALTER COLUMN"));
        assert!(stmts[0].contains("TYPE BIGINT"));
    }

    #[test]
    fn mysql_alter_uses_modify_column() {
        let before = schema(vec![col("id", "INT", false)]);
        let after = schema(vec![col("id", "BIGINT", false)]);
        let stmts = render_alter_statements(&before, &after, Dialect::MySql);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("MODIFY COLUMN"));
        assert!(stmts[0].contains("BIGINT"));
    }

    #[test]
    fn add_column_includes_not_null_and_default() {
        let before = schema(vec![]);
        let mut new_col = col("created_at", "TIMESTAMP", false);
        new_col.default = Some("now()".into());
        let after = schema(vec![new_col]);
        let stmts = render_alter_statements(&before, &after, Dialect::Postgres);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("ADD COLUMN"));
        assert!(stmts[0].contains("NOT NULL"));
        assert!(stmts[0].contains("DEFAULT now()"));
    }
}
