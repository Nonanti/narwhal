//! Inline cell edit support: parsing user input into a [`Value`] and
//! generating an UPDATE statement for the originating row.
//!
//! Used by [`crate::core::AppCore`] when the user commits a cell edit on a
//! result that has a [`crate::core::RowSource`] attached (i.e. previewed
//! tables). Freeform SQL results are not editable because we don't know
//! which table or primary key to target.

use narwhal_core::{Column, Row, Value};
use narwhal_sql::Dialect;

/// Parse a raw textual input into a typed [`Value`].
///
/// The rules are intentionally simple and predictable:
///
/// - The literal token `NULL` (any case) maps to [`Value::Null`].
/// - `true` / `false` map to [`Value::Bool`].
/// - A token that parses as `i64` becomes [`Value::Int`].
/// - A token that parses as `f64` (and contains a `.` or exponent) becomes
///   [`Value::Float`].
/// - Anything else is treated as a string. The engine performs whatever
///   coercion its native type system allows.
pub fn parse_input(text: &str) -> Value {
    let trimmed = text.trim();
    if trimmed.eq_ignore_ascii_case("null") {
        return Value::Null;
    }
    if trimmed == "true" {
        return Value::Bool(true);
    }
    if trimmed == "false" {
        return Value::Bool(false);
    }
    if let Ok(i) = trimmed.parse::<i64>() {
        return Value::Int(i);
    }
    if trimmed.contains('.') || trimmed.contains('e') || trimmed.contains('E') {
        if let Ok(f) = trimmed.parse::<f64>() {
            return Value::Float(f);
        }
    }
    Value::String(text.to_owned())
}

/// Quote an identifier for the given dialect.
pub fn quote_ident(name: &str, dialect: Dialect) -> String {
    match dialect {
        Dialect::MySql => format!("`{}`", name.replace('`', "``")),
        _ => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

/// Quote a `schema.table` pair, omitting an empty schema (SQLite).
pub fn quote_qualified(schema: &str, table: &str, dialect: Dialect) -> String {
    if schema.is_empty() {
        quote_ident(table, dialect)
    } else {
        format!(
            "{}.{}",
            quote_ident(schema, dialect),
            quote_ident(table, dialect)
        )
    }
}

/// One placeholder in the generated SQL. `1`-indexed for PG, `?` for the
/// rest.
pub fn placeholder(index: usize, dialect: Dialect) -> String {
    match dialect {
        Dialect::Postgres => format!("${index}"),
        _ => "?".into(),
    }
}

/// Compiled UPDATE statement: SQL with placeholders plus the bound values
/// in declaration order.
#[derive(Debug, Clone)]
pub struct CompiledUpdate {
    pub sql: String,
    pub params: Vec<Value>,
}

/// Build an `UPDATE schema.table SET col=$1 WHERE pk1=$2 [AND pk2=$3 …]`
/// statement against `columns` and the supplied PK identity row.
///
/// Returns an error string when the table has no usable primary key or
/// when any PK column value is null in the originating row (which would
/// match multiple rows on the server and isn't a safe update).
#[allow(clippy::too_many_arguments)]
pub fn build_update(
    schema: &str,
    table: &str,
    columns: &[Column],
    target_column: &str,
    new_value: &Value,
    row: &Row,
    column_order: &[String],
    dialect: Dialect,
) -> Result<CompiledUpdate, String> {
    let pk_columns: Vec<&Column> = columns.iter().filter(|c| c.primary_key).collect();
    if pk_columns.is_empty() {
        return Err(format!(
            "table {table}: no primary key, cell edits are disabled"
        ));
    }

    // Build the WHERE clause from the current row. Map each PK column name
    // back to its position in `column_order` to read the cell value.
    let mut where_parts = Vec::with_capacity(pk_columns.len());
    let mut params: Vec<Value> = Vec::with_capacity(1 + pk_columns.len());
    params.push(new_value.clone());

    for pk in &pk_columns {
        let Some(idx) = column_order.iter().position(|c| c == &pk.name) else {
            return Err(format!(
                "primary key column '{}' is not present in the result set",
                pk.name
            ));
        };
        let value = row.0.get(idx).cloned().unwrap_or(Value::Null);
        if value.is_null() {
            return Err(format!(
                "primary key column '{}' is NULL in this row; refusing to UPDATE",
                pk.name
            ));
        }
        let ph = placeholder(params.len() + 1, dialect);
        where_parts.push(format!("{} = {ph}", quote_ident(&pk.name, dialect)));
        params.push(value);
    }

    let sql = format!(
        "UPDATE {} SET {} = {} WHERE {}",
        quote_qualified(schema, table, dialect),
        quote_ident(target_column, dialect),
        placeholder(1, dialect),
        where_parts.join(" AND "),
    );

    Ok(CompiledUpdate { sql, params })
}

#[cfg(test)]
mod tests {
    use super::*;
    use narwhal_core::Column;

    fn pk_col(name: &str) -> Column {
        Column {
            name: name.into(),
            data_type: "integer".into(),
            nullable: false,
            primary_key: true,
            default: None,
        }
    }
    fn nopk_col(name: &str) -> Column {
        Column {
            name: name.into(),
            data_type: "text".into(),
            nullable: true,
            primary_key: false,
            default: None,
        }
    }

    #[test]
    fn parse_input_dispatches_by_shape() {
        assert!(matches!(parse_input("NULL"), Value::Null));
        assert!(matches!(parse_input(" null "), Value::Null));
        assert!(matches!(parse_input("true"), Value::Bool(true)));
        assert!(matches!(parse_input("false"), Value::Bool(false)));
        assert!(matches!(parse_input("42"), Value::Int(42)));
        assert!(matches!(parse_input("-7"), Value::Int(-7)));
        assert!(matches!(parse_input("3.14"), Value::Float(_)));
        assert!(matches!(parse_input("1e6"), Value::Float(_)));
        match parse_input("hello world") {
            Value::String(s) => assert_eq!(s, "hello world"),
            other => panic!("expected string, got {other:?}"),
        }
        // Trailing whitespace is preserved for string values so the user
        // can tell quoted-looking inputs apart from trimmed ones.
        match parse_input("  x  ") {
            Value::String(s) => assert_eq!(s, "  x  "),
            other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn quote_ident_per_dialect() {
        assert_eq!(quote_ident("orders", Dialect::Postgres), "\"orders\"");
        assert_eq!(quote_ident("orders", Dialect::Sqlite), "\"orders\"");
        assert_eq!(quote_ident("orders", Dialect::MySql), "`orders`");
        assert_eq!(quote_ident("a\"b", Dialect::Postgres), "\"a\"\"b\"");
        assert_eq!(quote_ident("a`b", Dialect::MySql), "`a``b`");
    }

    #[test]
    fn build_update_single_pk_postgres() {
        let columns = vec![pk_col("id"), nopk_col("label")];
        let row = Row(vec![Value::Int(42), Value::String("old".into())]);
        let order = vec!["id".to_owned(), "label".to_owned()];
        let upd = build_update(
            "public",
            "items",
            &columns,
            "label",
            &Value::String("new".into()),
            &row,
            &order,
            Dialect::Postgres,
        )
        .unwrap();
        assert_eq!(
            upd.sql,
            "UPDATE \"public\".\"items\" SET \"label\" = $1 WHERE \"id\" = $2"
        );
        assert_eq!(upd.params.len(), 2);
        match &upd.params[0] {
            Value::String(s) => assert_eq!(s, "new"),
            other => panic!("{other:?}"),
        }
        match &upd.params[1] {
            Value::Int(i) => assert_eq!(*i, 42),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn build_update_composite_pk_mysql() {
        let columns = vec![pk_col("a"), pk_col("b"), nopk_col("c")];
        let row = Row(vec![
            Value::Int(1),
            Value::Int(2),
            Value::String("x".into()),
        ]);
        let order = vec!["a".into(), "b".into(), "c".into()];
        let upd = build_update(
            "",
            "t",
            &columns,
            "c",
            &Value::String("y".into()),
            &row,
            &order,
            Dialect::MySql,
        )
        .unwrap();
        assert_eq!(upd.sql, "UPDATE `t` SET `c` = ? WHERE `a` = ? AND `b` = ?");
        assert_eq!(upd.params.len(), 3);
    }

    #[test]
    fn build_update_rejects_no_pk() {
        let columns = vec![nopk_col("a")];
        let row = Row(vec![Value::String("x".into())]);
        let order = vec!["a".to_owned()];
        let err = build_update(
            "",
            "t",
            &columns,
            "a",
            &Value::String("y".into()),
            &row,
            &order,
            Dialect::Sqlite,
        )
        .unwrap_err();
        assert!(err.contains("no primary key"));
    }

    #[test]
    fn build_update_rejects_null_pk() {
        let columns = vec![pk_col("id"), nopk_col("v")];
        let row = Row(vec![Value::Null, Value::String("x".into())]);
        let order = vec!["id".to_owned(), "v".to_owned()];
        let err = build_update(
            "",
            "t",
            &columns,
            "v",
            &Value::String("y".into()),
            &row,
            &order,
            Dialect::Sqlite,
        )
        .unwrap_err();
        assert!(err.contains("NULL"));
    }
}
