//! Streaming JSON array writer.

use std::io::Write;

use narwhal_core::{ColumnHeader, Row, Value};

use super::error::ExportError;

pub(super) fn write_json<W: Write>(
    writer: &mut W,
    columns: &[ColumnHeader],
    rows: &[Row],
) -> Result<(), ExportError> {
    writer.write_all(b"[")?;
    let mut first_row = true;
    for row in rows {
        if !first_row {
            writer.write_all(b",")?;
        }
        first_row = false;
        writer.write_all(b"{")?;
        let mut first_col = true;
        for (column, value) in columns.iter().zip(row.0.iter()) {
            if !first_col {
                writer.write_all(b",")?;
            }
            first_col = false;
            write_json_string(writer, &column.name)?;
            writer.write_all(b":")?;
            write_json_value(writer, value)?;
        }
        writer.write_all(b"}")?;
    }
    writer.write_all(b"]\n")?;
    Ok(())
}

pub(super) fn write_json_string<W: Write>(writer: &mut W, s: &str) -> Result<(), ExportError> {
    writer.write_all(b"\"")?;
    for ch in s.chars() {
        match ch {
            '"' => writer.write_all(b"\\\"")?,
            '\\' => writer.write_all(b"\\\\")?,
            '\n' => writer.write_all(b"\\n")?,
            '\r' => writer.write_all(b"\\r")?,
            '\t' => writer.write_all(b"\\t")?,
            c if (c as u32) < 0x20 => {
                write!(writer, "\\u{:04x}", c as u32)
                    .map_err(|e| ExportError::Serialise(e.to_string()))?;
            }
            c => {
                let mut buf = [0u8; 4];
                writer.write_all(c.encode_utf8(&mut buf).as_bytes())?;
            }
        }
    }
    writer.write_all(b"\"")?;
    Ok(())
}

pub(super) fn write_json_value<W: Write>(writer: &mut W, value: &Value) -> Result<(), ExportError> {
    match value {
        Value::Null => writer.write_all(b"null")?,
        Value::Bool(b) => writer.write_all(if *b { b"true" } else { b"false" })?,
        Value::Int(i) => {
            write!(writer, "{i}").map_err(|e| ExportError::Serialise(e.to_string()))?;
        }
        Value::Float(f) => {
            if f.is_finite() {
                write!(writer, "{f}").map_err(|e| ExportError::Serialise(e.to_string()))?;
            } else {
                writer.write_all(b"null")?;
            }
        }
        Value::Bytes(b) => {
            // Bytes that are valid UTF-8 are emitted as a JSON string.
            // Bytes that are NOT valid UTF-8 are emitted as
            // {"$bytes": "<base64>"} so the round-trip survives.
            if let Ok(s) = std::str::from_utf8(b) {
                write_json_string(writer, s)?;
            } else {
                let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b);
                writer.write_all(b"{\"$bytes\":\"")?;
                writer.write_all(encoded.as_bytes())?;
                writer.write_all(b"\"}")?;
            }
        }
        Value::Json(v) => {
            let rendered = v.to_string();
            writer.write_all(rendered.as_bytes())?;
        }
        Value::String(s) | Value::Unknown(s) => {
            write_json_string(writer, s)?;
        }
        Value::Date(_)
        | Value::Time(_)
        | Value::DateTime(_)
        | Value::Timestamp(_)
        | Value::Uuid(_) => {
            write_json_string(writer, &value.render())?;
        }
        // Future Value variants: serialise rendered form as a JSON string.
        _ => {
            write_json_string(writer, &value.render())?;
        }
    }
    Ok(())
}

