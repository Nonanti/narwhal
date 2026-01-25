//! RFC 4180-ish CSV writer.

use std::io::Write;

use narwhal_core::{ColumnHeader, Row, Value};

use super::error::ExportError;

pub(super) fn write_csv<W: Write>(
    writer: &mut W,
    columns: &[ColumnHeader],
    rows: &[Row],
) -> Result<(), ExportError> {
    // Header row
    let mut first = true;
    for column in columns {
        if !first {
            writer.write_all(b",")?;
        }
        write_csv_field(writer, &column.name)?;
        first = false;
    }
    writer.write_all(b"\r\n")?;

    // Data rows — RFC 4180: CRLF line endings
    for row in rows {
        let mut first = true;
        for value in &row.0 {
            if !first {
                writer.write_all(b",")?;
            }
            match value {
                Value::Null => { /* empty field */ }
                other => write_csv_field(writer, &other.render())?,
            }
            first = false;
        }
        writer.write_all(b"\r\n")?;
    }
    Ok(())
}

pub(super) fn write_csv_field<W: Write>(writer: &mut W, field: &str) -> Result<(), ExportError> {
    let needs_quoting = field
        .chars()
        .any(|c| matches!(c, ',' | '"' | '\n' | '\r' | '\t'));
    if needs_quoting {
        writer.write_all(b"\"")?;
        for ch in field.chars() {
            if ch == '"' {
                writer.write_all(b"\"\"")?;
            } else {
                let mut buf = [0u8; 4];
                writer.write_all(ch.encode_utf8(&mut buf).as_bytes())?;
            }
        }
        writer.write_all(b"\"")?;
    } else {
        writer.write_all(field.as_bytes())?;
    }
    Ok(())
}

