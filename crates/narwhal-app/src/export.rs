//! Result-set exporters (CSV, JSON, INSERT).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use narwhal_core::{ColumnHeader, Row, Value};

/// Wire format produced by [`export_rows`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
    Insert,
}

impl ExportFormat {
    pub fn from_token(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "csv" => Some(Self::Csv),
            "json" => Some(Self::Json),
            "insert" => Some(Self::Insert),
            _ => None,
        }
    }

    pub fn default_extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Json => "json",
            Self::Insert => "sql",
        }
    }
}

/// A qualified table name of the form `schema.table` or just `table`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedName {
    pub schema: Option<String>,
    pub table: String,
}

impl std::fmt::Display for QualifiedName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.schema {
            Some(s) => write!(f, "{s}.{}", self.table),
            None => write!(f, "{}", self.table),
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
    #[error(
        "INSERT export requires a known source table; the query did not target a single table"
    )]
    NoSourceTable,
}

/// Write `rows` to `path` formatted according to `format`.
///
/// The file is fully buffered and flushed before the function returns.
/// For `ExportFormat::Insert`, `source_table` must be `Some`; otherwise
/// [`ExportError::NoSourceTable`] is returned and no file is created.
pub fn export_rows(
    columns: &[ColumnHeader],
    rows: &[Row],
    format: ExportFormat,
    path: &Path,
    source_table: Option<&QualifiedName>,
) -> Result<(), ExportError> {
    if let ExportFormat::Insert = format {
        let table = source_table.ok_or(ExportError::NoSourceTable)?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);
        write_insert(&mut writer, table, columns, rows)?;
        writer.flush()?;
        return Ok(());
    }

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
        ExportFormat::Insert => unreachable!(),
    }
    writer.flush()?;
    Ok(())
}

fn write_csv<W: Write>(
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
            // Bytes that are valid UTF-8 are emitted as a JSON string.
            // Bytes that are NOT valid UTF-8 are emitted as
            // {"$bytes": "<base64>"} so the round-trip survives.
            if let Ok(s) = std::str::from_utf8(b) {
                write_json_string(writer, s)?
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
    }
    Ok(())
}

/// Write one `INSERT INTO <table> (col1, col2, ...) VALUES (...);` per row.
fn write_insert<W: Write>(
    writer: &mut W,
    table: &QualifiedName,
    columns: &[ColumnHeader],
    rows: &[Row],
) -> Result<(), ExportError> {
    if rows.is_empty() {
        return Ok(());
    }

    // Build the column list once.
    let col_list: String = columns
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let table_name = table.to_string();

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

fn write_insert_value<W: Write>(writer: &mut W, value: &Value) -> Result<(), ExportError> {
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
    }
    Ok(())
}

/// Write a SQL single-quoted string, escaping embedded single quotes by
/// doubling them (`'` → `''`).
fn write_quoted_sql_string<W: Write>(writer: &mut W, s: &str) -> Result<(), ExportError> {
    writer.write_all(b"'")?;
    for ch in s.chars() {
        if ch == '\'' {
            writer.write_all(b"''")?;
        } else {
            let mut buf = [0u8; 4];
            writer.write_all(ch.encode_utf8(&mut buf).as_bytes())?;
        }
    }
    writer.write_all(b"'")?;
    Ok(())
}

/// Regex-based heuristic to extract a single-table source from SQL.
///
/// Matches patterns like:
/// - `SELECT ... FROM table_name`
/// - `SELECT ... FROM schema.table_name`
///
/// Returns `None` for multi-table queries, subqueries, or anything
/// that doesn't cleanly match a single unaliased table reference.
pub fn extract_source_table(sql: &str) -> Option<QualifiedName> {
    // Normalise: trim, remove trailing semicolons, collapse whitespace.
    let normalised = sql.trim().trim_end_matches(';').trim();
    let lower = normalised.to_ascii_lowercase();

    // Must start with SELECT (case-insensitive).
    if !lower.starts_with("select") {
        return None;
    }

    // Find FROM keyword — simple scan. We look for ` FROM ` as a word
    // boundary to avoid matching "INFORMATION_SCHEMA" or subqueries.
    // This is a heuristic, not a full SQL parser.
    let from_pos = find_from_keyword(&lower)?;
    let after_from = &normalised[from_pos + " from ".len()..].trim_start();

    // Extract the first identifier (possibly qualified as schema.table).
    let (ident, rest) = extract_first_identifier(after_from);

    if ident.is_empty() {
        return None;
    }

    // Check that what follows is a clause boundary or end-of-string.
    let rest_trimmed = rest.trim();
    if rest_trimmed.is_empty() || is_clause_boundary(rest_trimmed) {
        // Split schema.table if qualified.
        if let Some(dot_pos) = ident.find('.') {
            let schema = &ident[..dot_pos];
            let table = &ident[dot_pos + 1..];
            if !schema.is_empty() && !table.is_empty() {
                return Some(QualifiedName {
                    schema: Some(schema.to_owned()),
                    table: table.to_owned(),
                });
            }
        }
        return Some(QualifiedName {
            schema: None,
            table: ident.clone(),
        });
    }

    None
}

/// Find the position of the first top-level ` FROM ` keyword in the
/// lowercased SQL string. Skips over parenthesised subqueries.
fn find_from_keyword(lower: &str) -> Option<usize> {
    let bytes = lower.as_bytes();
    let len = bytes.len();
    let keyword = b" from ";
    let mut depth = 0usize;

    let mut i = 0;
    while i + keyword.len() <= len {
        if bytes[i] == b'(' {
            depth += 1;
            i += 1;
            continue;
        }
        if bytes[i] == b')' {
            depth = depth.saturating_sub(1);
            i += 1;
            continue;
        }
        if depth == 0 && lower[i..].starts_with(" from ") {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Extract the first SQL identifier from `input`, returning the
/// identifier and the remaining text. An identifier may be
/// `schema.table` (two identifiers joined by a dot).
fn extract_first_identifier(input: &str) -> (String, &str) {
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut end = 0;

    // First segment: [a-zA-Z_][a-zA-Z0-9_]*
    if end < len && (bytes[end].is_ascii_alphabetic() || bytes[end] == b'_') {
        end += 1;
        while end < len && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
    } else {
        return (String::new(), input);
    }

    // Optional dot + second segment
    if end < len && bytes[end] == b'.' {
        let after_dot = end + 1;
        if after_dot < len && (bytes[after_dot].is_ascii_alphabetic() || bytes[after_dot] == b'_') {
            end = after_dot + 1;
            while end < len && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
        }
    }

    (input[..end].to_owned(), &input[end..])
}

/// Check whether `rest` starts with a SQL clause keyword, indicating
/// that the previous identifier was the sole table reference.
fn is_clause_boundary(rest: &str) -> bool {
    let lower = rest.to_ascii_lowercase();
    let keywords = [
        "where ",
        "where\t",
        "where\n",
        "group ",
        "having ",
        "order ",
        "limit ",
        "offset ",
        "union ",
        "intersect ",
        "except ",
        "for ",
        "lock ",
        ";",
    ];
    keywords.iter().any(|kw| lower.starts_with(kw))
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
    fn csv_round_trip_with_special_chars() {
        let (columns, rows) = fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv");
        export_rows(&columns, &rows, ExportFormat::Csv, &path, None).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();

        // RFC 4180: CRLF line endings, quoted fields for special chars.
        assert_eq!(
            body,
            "id,name,tag\r\n1,alice,\r\n2,\"she said \"\"hi\"\"\",\"with, comma\"\r\n"
        );
    }

    #[test]
    fn csv_null_becomes_empty_field() {
        let columns = vec![
            ColumnHeader {
                name: "a".into(),
                data_type: "INT".into(),
            },
            ColumnHeader {
                name: "b".into(),
                data_type: "INT".into(),
            },
        ];
        let rows = vec![Row(vec![Value::Int(1), Value::Null])];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv");
        export_rows(&columns, &rows, ExportFormat::Csv, &path, None).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        // NULL becomes empty field — "1," not "1,NULL"
        assert_eq!(body, "a,b\r\n1,\r\n");
    }

    #[test]
    fn json_array_of_objects() {
        let (columns, rows) = fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.json");
        export_rows(&columns, &rows, ExportFormat::Json, &path, None).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();

        // Verify structure
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], 1);
        assert_eq!(arr[0]["name"], "alice");
        assert_eq!(arr[0]["tag"], serde_json::Value::Null);
        assert_eq!(arr[1]["name"], "she said \"hi\"");
        assert_eq!(arr[1]["tag"], "with, comma");
    }

    #[test]
    fn json_invalid_utf8_uses_bytes_sentinel() {
        let columns = vec![ColumnHeader {
            name: "data".into(),
            data_type: "BLOB".into(),
        }];
        // Invalid UTF-8 bytes: 0xFF is never valid UTF-8
        let rows = vec![Row(vec![Value::Bytes(vec![0xFF, 0xFE, 0x00, 0x01])])];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.json");
        export_rows(&columns, &rows, ExportFormat::Json, &path, None).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        // Should be {"$bytes": "..."} object
        let obj = parsed[0]["data"].as_object().unwrap();
        assert!(obj.contains_key("$bytes"));
        let b64 = obj["$bytes"].as_str().unwrap();
        // Decode and verify round-trip
        let decoded =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64).unwrap();
        assert_eq!(decoded, vec![0xFF, 0xFE, 0x00, 0x01]);
    }

    #[test]
    fn insert_single_table_round_trip() {
        let columns = vec![
            ColumnHeader {
                name: "id".into(),
                data_type: "INTEGER".into(),
            },
            ColumnHeader {
                name: "name".into(),
                data_type: "TEXT".into(),
            },
        ];
        let rows = vec![
            Row(vec![Value::Int(1), Value::String("alice".into())]),
            Row(vec![Value::Int(2), Value::String("bob's place".into())]),
            Row(vec![Value::Null, Value::Null]),
        ];
        let table = QualifiedName {
            schema: None,
            table: "users".into(),
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.sql");
        export_rows(&columns, &rows, ExportFormat::Insert, &path, Some(&table)).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        // Verify the statements parse in SQLite
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE users (id INTEGER, name TEXT);
             DELETE FROM users;",
        )
        .unwrap();
        conn.execute_batch(&body).unwrap();

        // Verify round-trip
        let mut stmt = conn
            .prepare("SELECT id, name FROM users ORDER BY rowid")
            .unwrap();
        let result_rows: Vec<(Option<i64>, Option<String>)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(result_rows.len(), 3);
        assert_eq!(result_rows[0], (Some(1), Some("alice".into())));
        assert_eq!(result_rows[1], (Some(2), Some("bob's place".into())));
        assert_eq!(result_rows[2], (None, None));
    }

    #[test]
    fn insert_without_source_table_errors() {
        let columns = vec![ColumnHeader {
            name: "x".into(),
            data_type: "INT".into(),
        }];
        let rows = vec![Row(vec![Value::Int(42)])];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.sql");
        let result = export_rows(&columns, &rows, ExportFormat::Insert, &path, None);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ExportError::NoSourceTable),
            "expected NoSourceTable, got {err:?}"
        );
        // File must NOT have been created.
        assert!(!path.exists(), "file must not be created on error");
    }

    #[test]
    fn format_from_token_is_case_insensitive() {
        assert_eq!(ExportFormat::from_token("CSV"), Some(ExportFormat::Csv));
        assert_eq!(ExportFormat::from_token("Json"), Some(ExportFormat::Json));
        assert_eq!(
            ExportFormat::from_token("INSERT"),
            Some(ExportFormat::Insert)
        );
        assert_eq!(ExportFormat::from_token("xml"), None);
    }

    #[test]
    fn extract_source_table_simple() {
        assert_eq!(
            extract_source_table("SELECT * FROM users"),
            Some(QualifiedName {
                schema: None,
                table: "users".into()
            })
        );
    }

    #[test]
    fn extract_source_table_qualified() {
        assert_eq!(
            extract_source_table("SELECT id, name FROM public.users WHERE id > 5"),
            Some(QualifiedName {
                schema: Some("public".into()),
                table: "users".into()
            })
        );
    }

    #[test]
    fn extract_source_table_multi_table_returns_none() {
        assert_eq!(
            extract_source_table("SELECT * FROM users JOIN orders ON users.id = orders.user_id"),
            None
        );
    }

    #[test]
    fn extract_source_table_non_select_returns_none() {
        assert_eq!(extract_source_table("INSERT INTO foo VALUES (1)"), None);
    }

    #[test]
    fn extract_source_table_with_subquery() {
        // The FROM should skip over the parenthesised subquery.
        assert_eq!(
            extract_source_table("SELECT * FROM (SELECT 1) AS sub"),
            None
        );
    }

    #[test]
    fn csv_quotes_and_escapes_and_drops_nulls() {
        let (columns, rows) = fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv");
        export_rows(&columns, &rows, ExportFormat::Csv, &path, None).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            body,
            "id,name,tag\r\n1,alice,\r\n2,\"she said \"\"hi\"\"\",\"with, comma\"\r\n"
        );
    }

    #[test]
    fn json_emits_objects_with_real_null() {
        let (columns, rows) = fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.json");
        export_rows(&columns, &rows, ExportFormat::Json, &path, None).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            body,
            r#"[{"id":1,"name":"alice","tag":null},{"id":2,"name":"she said \"hi\"","tag":"with, comma"}]
"#
        );
    }
}
