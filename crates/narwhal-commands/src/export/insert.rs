//! SQL INSERT statement writer.

use std::io::Write;

use narwhal_core::{ColumnHeader, Row, Value};

use super::error::ExportError;
use super::format::QualifiedName;
use super::quoting::{quote_ident, write_quoted_sql_string};

pub(super) fn write_insert<W: Write>(
    writer: &mut W,
    table: &QualifiedName,
    columns: &[ColumnHeader],
    rows: &[Row],
) -> Result<(), ExportError> {
    if rows.is_empty() {
        return Ok(());
    }

    // Build the column list once, quoting every identifier so that
    // reserved words like `order` or `from` produce valid SQL.
    let col_list: String = columns
        .iter()
        .map(|c| quote_ident(&c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let table_name = match &table.schema {
        Some(s) => format!("{}.{}", quote_ident(s), quote_ident(&table.table)),
        None => quote_ident(&table.table),
    };

    for row in rows {
        write!(writer, "INSERT INTO {table_name} ({col_list}) VALUES (")
            .map_err(|e| ExportError::Serialise(e.to_string()))?;
        let mut first = true;
        for value in &row.0 {
            if !first {
                writer.write_all(b", ")?;
            }
            first = false;
            write_insert_value(writer, value)?;
        }
        writer.write_all(b");\n")?;
    }
    Ok(())
}

pub(super) fn write_insert_value<W: Write>(writer: &mut W, value: &Value) -> Result<(), ExportError> {
    match value {
        Value::Null => writer.write_all(b"NULL")?,
        Value::Bool(b) => {
            write!(writer, "{}", if *b { "TRUE" } else { "FALSE" })
                .map_err(|e| ExportError::Serialise(e.to_string()))?;
        }
        Value::Int(i) => {
            write!(writer, "{i}").map_err(|e| ExportError::Serialise(e.to_string()))?;
        }
        Value::Float(f) => {
            if f.is_finite() {
                write!(writer, "{f}").map_err(|e| ExportError::Serialise(e.to_string()))?;
            } else {
                writer.write_all(b"NULL")?;
            }
        }
        Value::Bytes(b) => {
            // Emit as X'<hex>' SQL blob literal.
            writer.write_all(b"X'")?;
            for byte in b {
                write!(writer, "{byte:02X}").map_err(|e| ExportError::Serialise(e.to_string()))?;
            }
            writer.write_all(b"'")?;
        }
        Value::Json(v) => {
            // Wrap JSON in single quotes, escaping internal quotes.
            let s = v.to_string();
            write_quoted_sql_string(writer, &s)?;
        }
        Value::String(s) | Value::Unknown(s) => {
            write_quoted_sql_string(writer, s)?;
        }
        Value::Date(_)
        | Value::Time(_)
        | Value::DateTime(_)
        | Value::Timestamp(_)
        | Value::Uuid(_) => {
            write_quoted_sql_string(writer, &value.render())?;
        }
        // Future Value variants: serialise rendered form as a SQL string literal.
        _ => {
            write_quoted_sql_string(writer, &value.render())?;
        }
    }
    Ok(())
}

