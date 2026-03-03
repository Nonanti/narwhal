//! DDL (CREATE TABLE / CREATE INDEX) generation from a [`TableSchema`].
//!
//! The output is engine-aware only in its identifier quoting style: types
//! and constraints are emitted verbatim using the engine's native names
//! captured at introspection time.

use narwhal_core::{ForeignKey, Index, TableSchema};
use narwhal_sql::Dialect;
use std::fmt::Write as _;

/// Render the schema of `table` as CREATE TABLE + accompanying
/// CREATE INDEX statements.
pub fn build_table_ddl(table: &TableSchema, dialect: Dialect) -> String {
    let mut out = String::with_capacity(256);
    let quoted_table = quote_qualified(&table.table.schema, &table.table.name, dialect);
    writeln!(&mut out, "CREATE TABLE {quoted_table} (").unwrap();

    // How many PK columns? Single-column PKs are declared inline on the column,
    // composite PKs are emitted as a table-level constraint at the bottom.
    let pk_columns: Vec<&str> = table
        .columns
        .iter()
        .filter(|c| c.primary_key)
        .map(|c| c.name.as_str())
        .collect();
    let composite_pk = pk_columns.len() > 1;

    let mut column_lines = Vec::with_capacity(table.columns.len());
    for col in &table.columns {
        let mut line = format!("  {} {}", quote_ident(&col.name, dialect), col.data_type);
        if !col.nullable {
            line.push_str(" NOT NULL");
        }
        if !composite_pk && col.primary_key {
            line.push_str(" PRIMARY KEY");
        }
        if let Some(default) = &col.default {
            // Defaults from the engine catalogue are already valid SQL
            // expressions, so they are inserted verbatim.
            write!(&mut line, " DEFAULT {default}").unwrap();
        }
        column_lines.push(line);
    }

    if composite_pk {
        let quoted: Vec<String> = pk_columns.iter().map(|c| quote_ident(c, dialect)).collect();
        column_lines.push(format!("  PRIMARY KEY ({})", quoted.join(", ")));
    }

    for uq in &table.unique_constraints {
        let quoted: Vec<String> = uq.columns.iter().map(|c| quote_ident(c, dialect)).collect();
        column_lines.push(format!(
            "  CONSTRAINT {} UNIQUE ({})",
            quote_ident(&uq.name, dialect),
            quoted.join(", ")
        ));
    }

    for fk in &table.foreign_keys {
        column_lines.push(format!("  {}", format_foreign_key(fk, dialect)));
    }

    out.push_str(&column_lines.join(",\n"));
    out.push('\n');
    out.push_str(");\n");

    // Non-implicit indexes become CREATE INDEX statements. PKs are already
    // represented inline (or by the composite constraint above).
    for idx in &table.indexes {
        if idx.primary {
            continue;
        }
        // Single-column unique indexes that match an autoindex placeholder
        // for a UNIQUE column declared inline are ignored; the column itself
        // already carries the UNIQUE keyword via the engine's catalogue.
        // We err on the side of including them because some engines (SQLite)
        // expose autoindexes verbatim and skipping is heuristic.
        out.push('\n');
        out.push_str(&format_index(
            &table.table.schema,
            &table.table.name,
            idx,
            dialect,
        ));
    }
    out
}

/// Build a `SELECT * FROM <table> LIMIT <n>` query targetting the engine's
/// quoting style. Used by the sidebar quick-preview action.
pub fn preview_query(schema: &str, table: &str, limit: usize, dialect: Dialect) -> String {
    preview_query_paged(schema, table, limit, 0, dialect)
}

/// Same as [`preview_query`] but with an explicit offset for pagination.
pub fn preview_query_paged(
    schema: &str,
    table: &str,
    limit: usize,
    offset: usize,
    dialect: Dialect,
) -> String {
    let qualified = quote_qualified(schema, table, dialect);
    if offset == 0 {
        format!("SELECT * FROM {qualified} LIMIT {limit}")
    } else {
        format!("SELECT * FROM {qualified} LIMIT {limit} OFFSET {offset}")
    }
}

/// Render multiple tables sequentially, separated by blank lines.
pub fn build_dump(tables: &[TableSchema], dialect: Dialect) -> String {
    let mut out = String::new();
    for (i, t) in tables.iter().enumerate() {
        if i > 0 {
            out.push_str("\n\n");
        }
        out.push_str(&build_table_ddl(t, dialect));
    }
    out
}

fn format_foreign_key(fk: &ForeignKey, dialect: Dialect) -> String {
    let cols: Vec<String> = fk.columns.iter().map(|c| quote_ident(c, dialect)).collect();
    let ref_cols: Vec<String> = fk
        .referenced_columns
        .iter()
        .map(|c| quote_ident(c, dialect))
        .collect();
    let referenced = quote_qualified(
        fk.referenced_schema.as_deref().unwrap_or(""),
        &fk.referenced_table,
        dialect,
    );
    let mut s = format!(
        "CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})",
        quote_ident(&fk.name, dialect),
        cols.join(", "),
        referenced,
        ref_cols.join(", ")
    );
    if let Some(action) = fk.on_update {
        write!(&mut s, " ON UPDATE {}", action.as_sql()).unwrap();
    }
    if let Some(action) = fk.on_delete {
        write!(&mut s, " ON DELETE {}", action.as_sql()).unwrap();
    }
    s
}

fn format_index(schema: &str, table: &str, idx: &Index, dialect: Dialect) -> String {
    let unique = if idx.unique { "UNIQUE " } else { "" };
    let cols: Vec<String> = idx
        .columns
        .iter()
        .map(|c| quote_ident(c, dialect))
        .collect();
    format!(
        "CREATE {unique}INDEX {} ON {} ({});\n",
        quote_ident(&idx.name, dialect),
        quote_qualified(schema, table, dialect),
        cols.join(", ")
    )
}

/// v1.2 #8: public re-export for `schema_diff`.
#[must_use]
pub fn quote_qualified_public(schema: &str, name: &str, dialect: Dialect) -> String {
    quote_qualified(schema, name, dialect)
}

/// v1.2 #8: public re-export for `schema_diff`.
#[must_use]
pub fn quote_ident_public(name: &str, dialect: Dialect) -> String {
    quote_ident(name, dialect)
}

fn quote_qualified(schema: &str, name: &str, dialect: Dialect) -> String {
    if schema.is_empty() {
        quote_ident(name, dialect)
    } else {
        format!(
            "{}.{}",
            quote_ident(schema, dialect),
            quote_ident(name, dialect)
        )
    }
}

fn quote_ident(name: &str, dialect: Dialect) -> String {
    match dialect {
        Dialect::MySql => format!("`{}`", name.replace('`', "``")),
        _ => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use narwhal_core::{
        Column, ForeignKey, Index, ReferentialAction, Table, TableKind, UniqueConstraint,
    };

    fn sample_table() -> TableSchema {
        TableSchema {
            table: Table {
                schema: "public".into(),
                name: "orders".into(),
                kind: TableKind::Table,
            },
            columns: vec![
                Column {
                    name: "id".into(),
                    data_type: "INTEGER".into(),
                    nullable: false,
                    primary_key: true,
                    default: None,
                },
                Column {
                    name: "customer_id".into(),
                    data_type: "INTEGER".into(),
                    nullable: false,
                    primary_key: false,
                    default: None,
                },
                Column {
                    name: "placed_at".into(),
                    data_type: "TEXT".into(),
                    nullable: false,
                    primary_key: false,
                    default: Some("CURRENT_TIMESTAMP".into()),
                },
            ],
            indexes: vec![
                Index {
                    name: "orders_pkey".into(),
                    columns: vec!["id".into()],
                    unique: true,
                    primary: true,
                },
                Index {
                    name: "idx_orders_placed_at".into(),
                    columns: vec!["placed_at".into()],
                    unique: false,
                    primary: false,
                },
            ],
            foreign_keys: vec![ForeignKey {
                name: "fk_orders_customer".into(),
                columns: vec!["customer_id".into()],
                referenced_schema: Some("public".into()),
                referenced_table: "customers".into(),
                referenced_columns: vec!["id".into()],
                on_update: None,
                on_delete: Some(ReferentialAction::Cascade),
            }],
            unique_constraints: vec![UniqueConstraint {
                name: "uniq_customer_placed".into(),
                columns: vec!["customer_id".into(), "placed_at".into()],
            }],
        }
    }

    #[test]
    fn renders_postgres_create_table_with_constraints_and_index() {
        let ddl = build_table_ddl(&sample_table(), Dialect::Postgres);
        let expected = "CREATE TABLE \"public\".\"orders\" (\n  \"id\" INTEGER NOT NULL PRIMARY KEY,\n  \"customer_id\" INTEGER NOT NULL,\n  \"placed_at\" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,\n  CONSTRAINT \"uniq_customer_placed\" UNIQUE (\"customer_id\", \"placed_at\"),\n  CONSTRAINT \"fk_orders_customer\" FOREIGN KEY (\"customer_id\") REFERENCES \"public\".\"customers\" (\"id\") ON DELETE CASCADE\n);\n\nCREATE INDEX \"idx_orders_placed_at\" ON \"public\".\"orders\" (\"placed_at\");\n";
        assert_eq!(ddl, expected);
    }

    #[test]
    fn mysql_uses_backticks() {
        let ddl = build_table_ddl(&sample_table(), Dialect::MySql);
        assert!(ddl.starts_with("CREATE TABLE `public`.`orders` ("));
        assert!(ddl.contains("`customer_id`"));
        assert!(ddl.contains("CREATE INDEX `idx_orders_placed_at`"));
    }

    #[test]
    fn preview_query_quotes_per_dialect() {
        assert_eq!(
            preview_query("public", "orders", 50, Dialect::Postgres),
            r#"SELECT * FROM "public"."orders" LIMIT 50"#
        );
        assert_eq!(
            preview_query("", "orders", 25, Dialect::MySql),
            "SELECT * FROM `orders` LIMIT 25"
        );
    }

    #[test]
    fn composite_primary_key_is_emitted_as_table_constraint() {
        let mut t = sample_table();
        // Make id+customer_id a composite PK.
        t.columns[1].primary_key = true;
        let ddl = build_table_ddl(&t, Dialect::Generic);
        assert!(ddl.contains("PRIMARY KEY (\"id\", \"customer_id\")"));
        assert!(!ddl.contains("INTEGER NOT NULL PRIMARY KEY"));
    }
}
