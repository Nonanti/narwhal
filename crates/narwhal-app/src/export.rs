//! Result-set exporters (CSV, JSON).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use narwhal_core::{ColumnHeader, Row, Value};

/// Wire format produced by [`export_rows`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
}

impl ExportFormat {
    pub fn from_token(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "csv" => Some(Self::Csv),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    pub fn default_extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Json => "json",
        }
    }
}

/// Errors produced while exporting.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialisation error: {0}")]
    Serialise(String),
}

/// Write `rows` to `path` formatted according to `format`.
///
/// The file is fully buffered and flushed before the function returns.
pub fn export_rows(
    columns: &[ColumnHeader],
    rows: &[Row],
    format: ExportFormat,
    path: &Path,
) -> Result<(), ExportError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);
    match format {
        ExportFormat::Csv => write_csv(&mut writer, columns, rows)?,
        ExportFormat::Json => write_json(&mut writer, columns, rows)?,
    }
    writer.flush()?;
    Ok(())
}

fn write_csv<W: Write>(
    writer: &mut W,
    columns: &[ColumnHeader],
    rows: &[Row],
) -> Result<(), ExportError> {
    let mut first = true;
    for column in columns {
        if !first {
            writer.write_all(b",")?;
        }
        write_csv_field(writer, &column.name)?;
        first = false;
    }
    writer.write_all(b"\n")?;
    for row in rows {
        let mut first = true;
        for value in &row.0 {
            if !first {
                writer.write_all(b",")?;
            }
            match value {
                Value::Null => {}
                other => write_csv_field(writer, &other.render())?,
            }
            first = false;
        }
        writer.write_all(b"\n")?;
    }
    Ok(())
}

fn write_csv_field<W: Write>(writer: &mut W, field: &str) -> Result<(), ExportError> {
    let needs_quoting = field.chars().any(|c| matches!(c, ',' | '"' | '\n' | '\r'));
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

fn write_json<W: Write>(
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

fn write_json_string<W: Write>(writer: &mut W, s: &str) -> Result<(), ExportError> {
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

fn write_json_value<W: Write>(writer: &mut W, value: &Value) -> Result<(), ExportError> {
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
            // Bytes are emitted as base16 strings inside a JSON string.
            let mut buf = String::with_capacity(b.len() * 2);
            for byte in b {
                use std::fmt::Write as _;
                let _ = write!(&mut buf, "{byte:02x}");
            }
            write_json_string(writer, &buf)?;
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
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Vec<ColumnHeader>, Vec<Row>) {
        let columns = vec![
            ColumnHeader {
                name: "id".into(),
                data_type: "INTEGER".into(),
            },
            ColumnHeader {
                name: "name".into(),
                data_type: "TEXT".into(),
            },
            ColumnHeader {
                name: "tag".into(),
                data_type: "TEXT".into(),
            },
        ];
        let rows = vec![
            Row(vec![
                Value::Int(1),
                Value::String("alice".into()),
                Value::Null,
            ]),
            Row(vec![
                Value::Int(2),
                Value::String("she said \"hi\"".into()),
                Value::String("with, comma".into()),
            ]),
        ];
        (columns, rows)
    }

    #[test]
    fn csv_quotes_and_escapes_and_drops_nulls() {
        let (columns, rows) = fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv");
        export_rows(&columns, &rows, ExportFormat::Csv, &path).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            body,
            "id,name,tag\n1,alice,\n2,\"she said \"\"hi\"\"\",\"with, comma\"\n"
        );
    }

    #[test]
    fn json_emits_objects_with_real_null() {
        let (columns, rows) = fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.json");
        export_rows(&columns, &rows, ExportFormat::Json, &path).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            body,
            r#"[{"id":1,"name":"alice","tag":null},{"id":2,"name":"she said \"hi\"","tag":"with, comma"}]
"#
        );
    }

    #[test]
    fn format_from_token_is_case_insensitive() {
        assert_eq!(ExportFormat::from_token("CSV"), Some(ExportFormat::Csv));
        assert_eq!(ExportFormat::from_token("Json"), Some(ExportFormat::Json));
        assert_eq!(ExportFormat::from_token("xml"), None);
    }
}
