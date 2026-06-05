//! Reconstruct `CREATE TABLE` DDL from `sys.*`.
//!
//! SQL Server has no `SHOW CREATE TABLE` equivalent. We walk
//! `INFORMATION_SCHEMA.COLUMNS` for the column shape, `sys.indexes` for
//! the primary key, and `sys.default_constraints` for default
//! expressions, and assemble the statement.
//!
//! v1 emits a single-table `CREATE TABLE schema.name (... PRIMARY KEY
//! (...));`. Indexes, FKs and triggers are out of scope for the
//! first revision; the existing per-driver tests verify that
//! `describe_table` exposes them separately, which is enough for the
//! TUI's schema view.

use narwhal_core::{Error, Result, Value};
use std::fmt::Write as _;

/// `[name]` with `]]` escape for embedded close brackets.
fn quote_ident(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

struct ColumnInfo {
    name: String,
    data_type: String,
    not_null: bool,
    default: Option<String>,
    is_identity: bool,
    seed: Option<i64>,
    increment: Option<i64>,
}

pub(crate) async fn build_create_table(
    conn: &super::MssqlConnection,
    schema: &str,
    table: &str,
) -> Result<String> {
    let columns = fetch_columns(conn, schema, table).await?;
    if columns.is_empty() {
        return Err(Error::Schema(format!("table {schema}.{table} not found")));
    }
    let pk_columns = fetch_pk(conn, schema, table).await?;

    let qualified = format!("{}.{}", quote_ident(schema), quote_ident(table));
    let mut out = String::with_capacity(256);
    writeln!(&mut out, "CREATE TABLE {qualified} (").map_err(|e| Error::Other(e.to_string()))?;

    let composite_pk = pk_columns.len() > 1;
    let mut column_lines: Vec<String> = Vec::with_capacity(columns.len());

    for col in &columns {
        let is_pk = pk_columns.contains(&col.name);
        let mut line = format!("  {} {}", quote_ident(&col.name), col.data_type);

        // Order: type → IDENTITY → DEFAULT → NOT NULL → PRIMARY KEY.
        // IDENTITY columns can carry a custom (seed, increment) pair
        // that we replay verbatim. Missing pair defaults to (1,1) which
        // SQL Server also uses when the column was declared with bare
        // `IDENTITY`.
        if col.is_identity {
            let seed = col.seed.unwrap_or(1);
            let increment = col.increment.unwrap_or(1);
            write!(&mut line, " IDENTITY({seed},{increment})")
                .map_err(|e| Error::Other(e.to_string()))?;
        } else if let Some(default) = &col.default {
            // Defaults come back wrapped in parentheses already:
            // e.g. `((0))`, `(getdate())`, `(N'literal')`. Emit as-is
            // so the round-trip is byte-stable when the user re-runs
            // the DDL.
            write!(&mut line, " DEFAULT {default}").map_err(|e| Error::Other(e.to_string()))?;
        }

        // PK implies NOT NULL: skip the redundant annotation for the
        // single-column PK case to keep the rendered DDL idiomatic.
        if col.not_null && (composite_pk || !is_pk) {
            line.push_str(" NOT NULL");
        }
        if !composite_pk && is_pk {
            line.push_str(" PRIMARY KEY");
        }
        column_lines.push(line);
    }

    if composite_pk {
        let quoted: Vec<String> = pk_columns.iter().map(|c| quote_ident(c)).collect();
        column_lines.push(format!("  PRIMARY KEY ({})", quoted.join(", ")));
    }

    out.push_str(&column_lines.join(",\n"));
    out.push_str("\n);\n");
    Ok(out)
}

async fn fetch_columns(
    conn: &super::MssqlConnection,
    schema: &str,
    table: &str,
) -> Result<Vec<ColumnInfo>> {
    // Mix INFORMATION_SCHEMA.COLUMNS (portable) with sys.columns
    // (IDENTITY metadata). The join key is the column-name +
    // OBJECT_ID(schema.table) pair. We use `LEFT JOIN
    // sys.identity_columns` so non-identity columns produce NULLs
    // instead of dropping out of the result set.
    const SQL: &str = "
        SELECT
            c.COLUMN_NAME,
            CASE
                WHEN c.DATA_TYPE IN ('varchar','nvarchar','char','nchar','varbinary','binary')
                  AND c.CHARACTER_MAXIMUM_LENGTH IS NOT NULL THEN
                    c.DATA_TYPE + '(' +
                      CASE WHEN c.CHARACTER_MAXIMUM_LENGTH = -1
                           THEN 'max'
                           ELSE CAST(c.CHARACTER_MAXIMUM_LENGTH AS varchar(11))
                      END + ')'
                WHEN c.DATA_TYPE IN ('decimal','numeric')
                  AND c.NUMERIC_PRECISION IS NOT NULL THEN
                    c.DATA_TYPE + '(' +
                      CAST(c.NUMERIC_PRECISION AS varchar(11)) + ',' +
                      CAST(COALESCE(c.NUMERIC_SCALE, 0) AS varchar(11)) + ')'
                ELSE c.DATA_TYPE
            END AS data_type,
            CASE WHEN c.IS_NULLABLE = 'YES' THEN CAST(0 AS bit) ELSE CAST(1 AS bit) END AS not_null,
            dc.definition AS column_default,
            CASE WHEN ic.column_id IS NULL THEN CAST(0 AS bit) ELSE CAST(1 AS bit) END AS is_identity,
            -- `sys.identity_columns.seed_value` / `.increment_value`
            -- are declared as `sql_variant`. Tiberius cannot decode
            -- sql_variant (panics with `not yet implemented for
            -- SSVariant`), so we cast to `bigint` server-side; this
            -- also keeps the type stable across IDENTITY base types.
            CAST(ic.seed_value AS bigint)      AS seed_value,
            CAST(ic.increment_value AS bigint) AS increment_value
        FROM INFORMATION_SCHEMA.COLUMNS c
        JOIN sys.columns sc
          ON sc.object_id = OBJECT_ID(QUOTENAME(c.TABLE_SCHEMA) + '.' + QUOTENAME(c.TABLE_NAME))
         AND sc.name      = c.COLUMN_NAME
        LEFT JOIN sys.default_constraints dc
          ON dc.parent_object_id = sc.object_id
         AND dc.parent_column_id = sc.column_id
        LEFT JOIN sys.identity_columns ic
          ON ic.object_id = sc.object_id
         AND ic.column_id = sc.column_id
        WHERE c.TABLE_SCHEMA = @P1 AND c.TABLE_NAME = @P2
        ORDER BY c.ORDINAL_POSITION";

    let result = conn
        .run(
            SQL,
            &[
                Value::String(schema.to_owned()),
                Value::String(table.to_owned()),
            ],
        )
        .await?;

    let mut columns = Vec::with_capacity(result.rows.len());
    for row in result.rows {
        let mut iter = row.0.into_iter();
        let name = match iter.next() {
            Some(Value::String(s)) => s,
            _ => continue,
        };
        let data_type = match iter.next() {
            Some(Value::String(s) | Value::Unknown(s)) => s,
            _ => "unknown".into(),
        };
        let not_null = matches!(iter.next(), Some(Value::Bool(true)));
        let default = match iter.next() {
            Some(Value::String(s) | Value::Unknown(s)) => Some(s),
            _ => None,
        };
        let is_identity = matches!(iter.next(), Some(Value::Bool(true)));
        // seed/increment come back as Numeric strings — sys.identity_columns
        // stores them as `sql_variant`/`numeric`. Parse defensively;
        // missing values default to (1, 1) at render time.
        let seed = match iter.next() {
            Some(Value::Int(i)) => Some(i),
            Some(Value::String(s)) => s.parse::<i64>().ok(),
            _ => None,
        };
        let increment = match iter.next() {
            Some(Value::Int(i)) => Some(i),
            Some(Value::String(s)) => s.parse::<i64>().ok(),
            _ => None,
        };
        columns.push(ColumnInfo {
            name,
            data_type,
            not_null,
            default,
            is_identity,
            seed,
            increment,
        });
    }
    Ok(columns)
}

async fn fetch_pk(conn: &super::MssqlConnection, schema: &str, table: &str) -> Result<Vec<String>> {
    const SQL: &str = "
        SELECT c.name
          FROM sys.indexes i
          JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id
          JOIN sys.columns       c  ON c.object_id  = ic.object_id AND c.column_id = ic.column_id
          JOIN sys.tables        t  ON t.object_id  = i.object_id
          JOIN sys.schemas       s  ON s.schema_id  = t.schema_id
         WHERE s.name = @P1 AND t.name = @P2 AND i.is_primary_key = 1
         ORDER BY ic.key_ordinal";

    let result = conn
        .run(
            SQL,
            &[
                Value::String(schema.to_owned()),
                Value::String(table.to_owned()),
            ],
        )
        .await?;

    Ok(result
        .rows
        .into_iter()
        .filter_map(|row| match row.0.into_iter().next() {
            Some(Value::String(s)) => Some(s),
            _ => None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::quote_ident;

    #[test]
    fn quote_ident_doubles_close_bracket() {
        assert_eq!(quote_ident("plain"), "[plain]");
        assert_eq!(quote_ident("a]b"), "[a]]b]");
    }
}
