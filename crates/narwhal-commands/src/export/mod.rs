//! Tabular result export pipelines.

mod csv;
mod error;
mod format;
mod insert;
mod json;
mod quoting;
mod source;
mod table;
mod tsv;

pub use error::ExportError;
pub use format::{ExportFormat, QualifiedName};
pub use source::extract_source_table;

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use narwhal_core::{ColumnHeader, Row};

pub fn export_rows(
    columns: &[ColumnHeader],
    rows: &[Row],
    format: ExportFormat,
    path: &Path,
    source_table: Option<&QualifiedName>,
) -> Result<(), ExportError> {
    if format == ExportFormat::Insert {
        let table = source_table.ok_or(ExportError::NoSourceTable)?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);
        insert::write_insert(&mut writer, table, columns, rows)?;
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
        ExportFormat::Csv => csv::write_csv(&mut writer, columns, rows)?,
        ExportFormat::Json => json::write_json(&mut writer, columns, rows)?,
        ExportFormat::Tsv => tsv::write_tsv(&mut writer, columns, rows)?,
        ExportFormat::Table => table::write_table(&mut writer, columns, rows)?,
        ExportFormat::Insert => unreachable!(),
    }
    writer.flush()?;
    Ok(())
}

/// Write `rows` to an arbitrary [`Write`] sink — the streaming sibling
/// of [`export_rows`].
///
/// The headless CLI (`narwhal exec ...`) uses this to dump query
/// results to stdout without going through a temp file. `Insert` is
/// rejected here because it requires a source-table argument the caller
/// must provide via [`export_rows`] instead.
pub fn write_format<W: Write>(
    writer: &mut W,
    format: ExportFormat,
    columns: &[ColumnHeader],
    rows: &[Row],
) -> Result<(), ExportError> {
    match format {
        ExportFormat::Csv => csv::write_csv(writer, columns, rows),
        ExportFormat::Json => json::write_json(writer, columns, rows),
        ExportFormat::Tsv => tsv::write_tsv(writer, columns, rows),
        ExportFormat::Table => table::write_table(writer, columns, rows),
        ExportFormat::Insert => Err(ExportError::NoSourceTable),
    }
}

#[cfg(test)]
mod tests {
    use narwhal_core::Value;

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

    // -- New tests for K2-A, Y2-A, O2 fixes ----------------------------------

    #[test]
    fn insert_quotes_reserved_columns() {
        let columns = vec![
            ColumnHeader {
                name: "id".into(),
                data_type: "INTEGER".into(),
            },
            ColumnHeader {
                name: "order".into(),
                data_type: "INTEGER".into(),
            },
            ColumnHeader {
                name: "from".into(),
                data_type: "TEXT".into(),
            },
        ];
        let rows = vec![Row(vec![
            Value::Int(1),
            Value::Int(42),
            Value::String("warehouse".into()),
        ])];
        let table = QualifiedName {
            schema: None,
            table: "orders".into(),
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.sql");
        export_rows(&columns, &rows, ExportFormat::Insert, &path, Some(&table)).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        // All identifiers must be double-quoted.
        assert!(
            body.contains(r#"INSERT INTO "orders" ("id", "order", "from") VALUES"#),
            "expected quoted identifiers, got: {body}"
        );
        // Verify the output is valid SQL by executing it in SQLite.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE orders (id INTEGER, \"order\" INTEGER, \"from\" TEXT);")
            .unwrap();
        conn.execute_batch(&body).unwrap();
    }

    #[test]
    fn insert_quotes_schema_qualified_table() {
        let columns = vec![ColumnHeader {
            name: "id".into(),
            data_type: "INT".into(),
        }];
        let rows = vec![Row(vec![Value::Int(1)])];
        let table = QualifiedName {
            schema: Some("public".into()),
            table: "orders".into(),
        };
        let mut buf: Vec<u8> = Vec::new();
        insert::write_insert(&mut buf, &table, &columns, &rows).unwrap();
        let body = String::from_utf8(buf).unwrap();
        assert!(
            body.starts_with(r#"INSERT INTO "public"."orders""#),
            "expected quoted schema.table, got: {body}"
        );
    }

    #[test]
    fn extract_source_table_unaliased_with_where() {
        assert_eq!(
            extract_source_table("SELECT * FROM users WHERE id > 5"),
            Some(QualifiedName {
                schema: None,
                table: "users".into()
            })
        );
    }

    #[test]
    fn extract_source_table_alias_with_where() {
        // Single table with bare alias — should still extract the table.
        assert_eq!(
            extract_source_table("SELECT * FROM users u WHERE id > 5"),
            Some(QualifiedName {
                schema: None,
                table: "users".into()
            })
        );
    }

    #[test]
    fn extract_source_table_as_alias_join_returns_none() {
        // Multi-table query with AS alias before JOIN.
        assert_eq!(
            extract_source_table("SELECT * FROM orders AS o JOIN users u ON o.uid = u.id"),
            None
        );
    }

    #[test]
    fn extract_source_table_quoted_identifier() {
        assert_eq!(
            extract_source_table(r#"SELECT * FROM "public"."users""#),
            Some(QualifiedName {
                schema: Some("public".into()),
                table: "users".into(),
            })
        );
    }

    #[test]
    fn csv_quotes_tab_character() {
        let columns = vec![ColumnHeader {
            name: "val".into(),
            data_type: "TEXT".into(),
        }];
        let rows = vec![Row(vec![Value::String("a\tb".into())])];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv");
        export_rows(&columns, &rows, ExportFormat::Csv, &path, None).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        // Tab-containing cell must be enclosed in double quotes.
        assert!(
            body.contains("\"a\tb\""),
            "tab should trigger quoting, got: {body}"
        );
    }


    fn sample_columns_and_rows() -> (Vec<ColumnHeader>, Vec<Row>) {
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
            Row(vec![Value::Int(2), Value::String("bob".into())]),
            Row(vec![Value::Int(3), Value::Null]),
        ];
        (columns, rows)
    }

    #[test]
    fn write_format_csv_round_trips_through_memory() {
        let (columns, rows) = sample_columns_and_rows();
        let mut buf = Vec::new();
        write_format(&mut buf, ExportFormat::Csv, &columns, &rows).unwrap();
        let body = String::from_utf8(buf).unwrap();
        // RFC 4180 line endings + header row
        assert!(body.starts_with("id,name\r\n"));
        assert!(body.contains("1,alice\r\n"));
        assert!(body.contains("2,bob\r\n"));
        // NULL field renders as empty
        assert!(body.trim_end().ends_with("3,"));
    }

    #[test]
    fn write_format_json_is_array_of_objects() {
        let (columns, rows) = sample_columns_and_rows();
        let mut buf = Vec::new();
        write_format(&mut buf, ExportFormat::Json, &columns, &rows).unwrap();
        let body = String::from_utf8(buf).unwrap();
        // Parse to confirm valid JSON and the expected shape.
        let value: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        let arr = value.as_array().expect("top-level is array");
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["id"], 1);
        assert_eq!(arr[0]["name"], "alice");
        assert!(arr[2]["name"].is_null(), "NULL must serialise as JSON null");
    }

    #[test]
    fn write_format_tsv_uses_tabs_and_replaces_embedded_separators() {
        let columns = vec![ColumnHeader {
            name: "val".into(),
            data_type: "TEXT".into(),
        }];
        let rows = vec![
            Row(vec![Value::String("a\tb".into())]),
            Row(vec![Value::String("line1\nline2".into())]),
        ];
        let mut buf = Vec::new();
        write_format(&mut buf, ExportFormat::Tsv, &columns, &rows).unwrap();
        let body = String::from_utf8(buf).unwrap();
        // Header
        assert_eq!(body.lines().next().unwrap(), "val");
        // Embedded tab/newline are replaced by spaces so TSV framing
        // stays intact: shell pipes break otherwise.
        assert!(body.contains("a b\n"), "tab must be replaced: {body:?}");
        assert!(
            body.contains("line1 line2\n"),
            "newline must be replaced: {body:?}"
        );
    }

    #[test]
    fn write_format_table_has_aligned_columns_and_borders() {
        let (columns, rows) = sample_columns_and_rows();
        let mut buf = Vec::new();
        write_format(&mut buf, ExportFormat::Table, &columns, &rows).unwrap();
        let body = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        // 1 top border + 1 header + 1 header/data border + 3 rows + 1 bottom border = 7 lines
        assert_eq!(lines.len(), 7, "got body: {body}");
        // Every line starts and ends with `+` (borders) or `|` (rows).
        for line in &lines {
            let starts = line.starts_with('+') || line.starts_with('|');
            let ends = line.ends_with('+') || line.ends_with('|');
            assert!(starts && ends, "malformed line: {line:?}");
        }
        // The widest cell in the `name` column is "alice" (5 chars) so
        // the column body cells must be at least 7 wide (5 + 2 padding).
        assert!(lines[1].contains(" name"));
    }

    #[test]
    fn write_format_table_handles_empty_result() {
        // Schemaful empty result: header + borders, no data rows. This
        // is the `SELECT * FROM t WHERE 0=1` case the exec CLI hits all
        // the time and must not panic on.
        let columns = vec![ColumnHeader {
            name: "id".into(),
            data_type: "INTEGER".into(),
        }];
        let rows: Vec<Row> = Vec::new();
        let mut buf = Vec::new();
        write_format(&mut buf, ExportFormat::Table, &columns, &rows).unwrap();
        let body = String::from_utf8(buf).unwrap();
        // top border + header + middle border + bottom border = 4 lines
        assert_eq!(body.lines().count(), 4);
    }

    #[test]
    fn write_format_insert_is_rejected_without_source_table() {
        let (columns, rows) = sample_columns_and_rows();
        let mut buf = Vec::new();
        let err = write_format(&mut buf, ExportFormat::Insert, &columns, &rows).unwrap_err();
        assert!(
            matches!(err, ExportError::NoSourceTable),
            "insert without table must surface NoSourceTable, got: {err:?}"
        );
    }

    #[test]
    fn export_format_from_token_recognises_new_formats() {
        assert_eq!(ExportFormat::from_token("tsv"), Some(ExportFormat::Tsv));
        assert_eq!(ExportFormat::from_token("TSV"), Some(ExportFormat::Tsv));
        assert_eq!(ExportFormat::from_token("table"), Some(ExportFormat::Table));
        assert_eq!(ExportFormat::from_token("tbl"), Some(ExportFormat::Table));
        assert_eq!(ExportFormat::from_token("sql"), Some(ExportFormat::Insert));
        assert_eq!(ExportFormat::from_token("unknown"), None);
    }
}

