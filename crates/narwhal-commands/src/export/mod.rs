//! Tabular result export pipelines.

mod csv;
mod error;
mod format;
mod insert;
mod json;
mod markdown;
mod parquet;
mod quoting;
mod source;
mod table;
mod tsv;

pub use error::ExportError;
pub use format::{ExportFormat, ExportOptions, MarkdownOptions, ParquetCompression, QualifiedName};
pub use source::extract_source_table;

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use narwhal_core::{ColumnHeader, Row};

/// Write `rows` to `path` in the requested format.
///
/// `options` is threaded through to format writers that read it
/// (Parquet compression, Markdown row cap). Formats that don't read
/// any option ignore the argument; the type bag keeps the public
/// surface stable as new options arrive.
pub fn export_rows(
    columns: &[ColumnHeader],
    rows: &[Row],
    format: ExportFormat,
    path: &Path,
    source_table: Option<&QualifiedName>,
    options: &ExportOptions,
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

    // T1-T4-B: Parquet manages its own staging+rename atomically and
    // takes a path rather than a `Write` (the writer needs to own the
    // file so it can flush the footer on close).
    if format == ExportFormat::Parquet {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        return parquet::write_parquet(columns, rows, path, options.parquet_compression);
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
        ExportFormat::Markdown => {
            markdown::write_markdown(&mut writer, columns, rows, options.markdown)?;
        }
        // Sprint 6 (M10) / T1-T4-B: `Insert` and `Parquet` are handled
        // by early returns above; reaching this arm means a refactor
        // missed wiring a new format. Convert the impossible-state
        // into a typed error so a future bug surfaces as `Err`.
        ExportFormat::Insert => return Err(ExportError::NoSourceTable),
        ExportFormat::Parquet => unreachable!("parquet handled above"),
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
/// must provide via [`export_rows`] instead. `Parquet` is rejected
/// because the writer needs to seek/own its sink for the footer flush
/// — use [`export_rows`] with a real path instead.
pub fn write_format<W: Write>(
    writer: &mut W,
    format: ExportFormat,
    columns: &[ColumnHeader],
    rows: &[Row],
) -> Result<(), ExportError> {
    write_format_with_options(writer, format, columns, rows, &ExportOptions::default())
}

/// Streaming sink writer that respects format-specific options. Same
/// caveats as [`write_format`] — `Insert` / `Parquet` are rejected.
pub fn write_format_with_options<W: Write>(
    writer: &mut W,
    format: ExportFormat,
    columns: &[ColumnHeader],
    rows: &[Row],
    options: &ExportOptions,
) -> Result<(), ExportError> {
    match format {
        ExportFormat::Csv => csv::write_csv(writer, columns, rows),
        ExportFormat::Json => json::write_json(writer, columns, rows),
        ExportFormat::Tsv => tsv::write_tsv(writer, columns, rows),
        ExportFormat::Table => table::write_table(writer, columns, rows),
        ExportFormat::Markdown => markdown::write_markdown(writer, columns, rows, options.markdown),
        ExportFormat::Insert => Err(ExportError::NoSourceTable),
        // Parquet's footer + magic bytes mean we need to flush after
        // every row group. The current pipeline buffers stdout, which
        // makes that infeasible without owning the sink. Refuse here
        // and direct the caller at `export_rows`.
        ExportFormat::Parquet => Err(ExportError::Serialise(
            "parquet export cannot stream to a generic Write sink — use export_rows with a file path"
                .to_owned(),
        )),
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
        export_rows(
            &columns,
            &rows,
            ExportFormat::Csv,
            &path,
            None,
            &ExportOptions::default(),
        )
        .unwrap();
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
        export_rows(
            &columns,
            &rows,
            ExportFormat::Csv,
            &path,
            None,
            &ExportOptions::default(),
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        // NULL becomes empty field — "1," not "1,NULL"
        assert_eq!(body, "a,b\r\n1,\r\n");
    }

    #[test]
    fn json_array_of_objects() {
        let (columns, rows) = fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.json");
        export_rows(
            &columns,
            &rows,
            ExportFormat::Json,
            &path,
            None,
            &ExportOptions::default(),
        )
        .unwrap();
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
        export_rows(
            &columns,
            &rows,
            ExportFormat::Json,
            &path,
            None,
            &ExportOptions::default(),
        )
        .unwrap();
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
        export_rows(
            &columns,
            &rows,
            ExportFormat::Insert,
            &path,
            Some(&table),
            &ExportOptions::default(),
        )
        .unwrap();
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
        let result = export_rows(
            &columns,
            &rows,
            ExportFormat::Insert,
            &path,
            None,
            &ExportOptions::default(),
        );
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
        export_rows(
            &columns,
            &rows,
            ExportFormat::Csv,
            &path,
            None,
            &ExportOptions::default(),
        )
        .unwrap();
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
        export_rows(
            &columns,
            &rows,
            ExportFormat::Json,
            &path,
            None,
            &ExportOptions::default(),
        )
        .unwrap();
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
        export_rows(
            &columns,
            &rows,
            ExportFormat::Insert,
            &path,
            Some(&table),
            &ExportOptions::default(),
        )
        .unwrap();
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
        export_rows(
            &columns,
            &rows,
            ExportFormat::Csv,
            &path,
            None,
            &ExportOptions::default(),
        )
        .unwrap();
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
        // T1-T4-B: parquet + markdown aliases
        assert_eq!(
            ExportFormat::from_token("parquet"),
            Some(ExportFormat::Parquet)
        );
        assert_eq!(ExportFormat::from_token("pq"), Some(ExportFormat::Parquet));
        assert_eq!(
            ExportFormat::from_token("markdown"),
            Some(ExportFormat::Markdown)
        );
        assert_eq!(ExportFormat::from_token("md"), Some(ExportFormat::Markdown));
        assert_eq!(ExportFormat::from_token("MD"), Some(ExportFormat::Markdown));
    }

    // -- T1-T4-B Markdown writer --------------------------------------------

    #[test]
    fn markdown_emits_gfm_table_with_header_and_alignment() {
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
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.md");
        export_rows(
            &columns,
            &rows,
            ExportFormat::Markdown,
            &path,
            None,
            &ExportOptions::default(),
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        // Header row.
        assert!(body.starts_with("| id | name |\n"), "got: {body:?}");
        // Separator: id is INTEGER → right-align, name is TEXT → left.
        let separator = body.lines().nth(1).unwrap();
        assert_eq!(separator, "| ---: | :--- |");
        // Data rows.
        assert!(body.contains("| 1 | alice |"));
        assert!(body.contains("| 2 | bob |"));
    }

    #[test]
    fn markdown_escapes_pipe_and_newline_and_backslash() {
        let columns = vec![ColumnHeader {
            name: "val".into(),
            data_type: "TEXT".into(),
        }];
        let rows = vec![
            Row(vec![Value::String("a|b".into())]),
            Row(vec![Value::String("line1\nline2".into())]),
            Row(vec![Value::String(r"back\slash".into())]),
        ];
        let mut buf = Vec::new();
        write_format(&mut buf, ExportFormat::Markdown, &columns, &rows).unwrap();
        let body = String::from_utf8(buf).unwrap();
        assert!(body.contains(r"| a\|b |"), "pipe must escape: {body}");
        assert!(
            body.contains("| line1<br>line2 |"),
            "newline must escape to <br>: {body}"
        );
        assert!(
            body.contains(r"| back\\slash |"),
            "backslash must double-escape: {body}"
        );
    }

    #[test]
    fn markdown_null_renders_as_sentinel() {
        let columns = vec![ColumnHeader {
            name: "v".into(),
            data_type: "TEXT".into(),
        }];
        let rows = vec![Row(vec![Value::Null])];
        let mut buf = Vec::new();
        write_format(&mut buf, ExportFormat::Markdown, &columns, &rows).unwrap();
        let body = String::from_utf8(buf).unwrap();
        assert!(
            body.contains("| (null) |"),
            "null must render as (null): {body}"
        );
    }

    #[test]
    fn markdown_truncates_at_row_limit_and_appends_marker() {
        let columns = vec![ColumnHeader {
            name: "n".into(),
            data_type: "INTEGER".into(),
        }];
        let rows: Vec<Row> = (0..10).map(|i| Row(vec![Value::Int(i)])).collect();
        let options = ExportOptions {
            markdown: MarkdownOptions { row_limit: Some(3) },
            ..ExportOptions::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capped.md");
        export_rows(
            &columns,
            &rows,
            ExportFormat::Markdown,
            &path,
            None,
            &options,
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        // First three rows present, the rest replaced by the marker.
        assert!(body.contains("| 0 |"));
        assert!(body.contains("| 1 |"));
        assert!(body.contains("| 2 |"));
        assert!(!body.contains("| 3 |"), "row 3 must be truncated: {body}");
        assert!(
            body.contains("_\u{2026}7 more rows truncated_"),
            "truncation marker missing: {body}"
        );
    }

    #[test]
    fn markdown_no_truncate_dumps_every_row() {
        let columns = vec![ColumnHeader {
            name: "n".into(),
            data_type: "INTEGER".into(),
        }];
        let rows: Vec<Row> = (0..2500).map(|i| Row(vec![Value::Int(i)])).collect();
        let options = ExportOptions::markdown_full();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("full.md");
        export_rows(
            &columns,
            &rows,
            ExportFormat::Markdown,
            &path,
            None,
            &options,
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        // 2 header lines + 2500 data lines + 0 truncation lines.
        assert_eq!(body.lines().filter(|l| l.starts_with("| ")).count(), 2502);
        assert!(!body.contains("more rows truncated"));
    }

    #[test]
    fn markdown_handles_empty_columns_gracefully() {
        let columns: Vec<ColumnHeader> = Vec::new();
        let rows: Vec<Row> = Vec::new();
        let mut buf = Vec::new();
        write_format(&mut buf, ExportFormat::Markdown, &columns, &rows).unwrap();
        let body = String::from_utf8(buf).unwrap();
        assert_eq!(body, "_no result to export_\n");
    }

    // -- T1-T4-B Parquet writer ---------------------------------------------

    #[test]
    fn parquet_round_trip_scalar_types() {
        use ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
        use arrow_array::Array;

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
                name: "score".into(),
                data_type: "DOUBLE".into(),
            },
            ColumnHeader {
                name: "active".into(),
                data_type: "BOOLEAN".into(),
            },
        ];
        let rows = vec![
            Row(vec![
                Value::Int(1),
                Value::String("alice".into()),
                Value::Float(3.5),
                Value::Bool(true),
            ]),
            Row(vec![
                Value::Int(2),
                Value::Null,
                Value::Float(2.5),
                Value::Bool(false),
            ]),
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.parquet");
        export_rows(
            &columns,
            &rows,
            ExportFormat::Parquet,
            &path,
            None,
            &ExportOptions::default(),
        )
        .unwrap();

        // Read it back via the arrow-rs reader and verify the values
        // survived the trip. Use the standard reader; if this changes
        // shape, the bug is in our writer, not the test.
        let file = std::fs::File::open(&path).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let schema = builder.schema().clone();
        let mut reader = builder.build().unwrap();
        let batch = reader.next().unwrap().unwrap();
        assert_eq!(schema.fields().len(), 4);
        assert_eq!(schema.field(0).name(), "id");
        assert_eq!(schema.field(1).name(), "name");
        assert_eq!(batch.num_rows(), 2);
        // Verify id column.
        let id_col = batch
            .column(0)
            .as_any()
            .downcast_ref::<arrow_array::Int64Array>()
            .expect("id is Int64");
        assert_eq!(id_col.value(0), 1);
        assert_eq!(id_col.value(1), 2);
        // Verify name column (null in row 2).
        let name_col = batch
            .column(1)
            .as_any()
            .downcast_ref::<arrow_array::StringArray>()
            .expect("name is Utf8");
        assert_eq!(name_col.value(0), "alice");
        assert!(name_col.is_null(1));
        // Verify score (Float64).
        let score_col = batch
            .column(2)
            .as_any()
            .downcast_ref::<arrow_array::Float64Array>()
            .expect("score is Float64");
        assert!((score_col.value(0) - 3.5).abs() < 1e-9);
        // Verify active (Bool).
        let active_col = batch
            .column(3)
            .as_any()
            .downcast_ref::<arrow_array::BooleanArray>()
            .expect("active is Bool");
        assert!(active_col.value(0));
        assert!(!active_col.value(1));
    }

    #[test]
    fn parquet_zstd_compression_produces_smaller_file() {
        // Highly compressible payload so we can be confident the
        // compression flag is taking effect (uncompressed will be
        // much larger than snappy/zstd).
        let columns = vec![ColumnHeader {
            name: "text".into(),
            data_type: "TEXT".into(),
        }];
        let payload = "a".repeat(2048);
        let rows: Vec<Row> = (0..200)
            .map(|_| Row(vec![Value::String(payload.clone())]))
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let uncompressed = dir.path().join("u.parquet");
        let zstd_path = dir.path().join("z.parquet");
        export_rows(
            &columns,
            &rows,
            ExportFormat::Parquet,
            &uncompressed,
            None,
            &ExportOptions::parquet(ParquetCompression::None),
        )
        .unwrap();
        export_rows(
            &columns,
            &rows,
            ExportFormat::Parquet,
            &zstd_path,
            None,
            &ExportOptions::parquet(ParquetCompression::Zstd),
        )
        .unwrap();
        let u_size = std::fs::metadata(&uncompressed).unwrap().len();
        let z_size = std::fs::metadata(&zstd_path).unwrap().len();
        // Parquet's dictionary encoding already squashes the repetitive
        // payload heavily, so the codec-on-top compression ratio is
        // modest. A 2x improvement is the conservative floor that
        // still proves the flag does *something*.
        assert!(
            z_size * 2 < u_size,
            "zstd should at least halve repetitive payload: u={u_size} z={z_size}"
        );
    }

    #[test]
    fn parquet_widens_mixed_numeric_column_to_float64() {
        use ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
        use arrow_array::Array;

        let columns = vec![ColumnHeader {
            name: "n".into(),
            data_type: "NUMERIC".into(),
        }];
        let rows = vec![
            Row(vec![Value::Int(1)]),
            Row(vec![Value::Float(1.5)]),
            Row(vec![Value::Int(4)]),
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mixed.parquet");
        export_rows(
            &columns,
            &rows,
            ExportFormat::Parquet,
            &path,
            None,
            &ExportOptions::default(),
        )
        .unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let batch = builder.build().unwrap().next().unwrap().unwrap();
        let col = batch
            .column(0)
            .as_any()
            .downcast_ref::<arrow_array::Float64Array>()
            .expect("mixed int+float widens to Float64");
        assert!((col.value(0) - 1.0).abs() < 1e-9);
        assert!((col.value(1) - 1.5).abs() < 1e-9);
        assert!((col.value(2) - 4.0).abs() < 1e-9);
    }

    #[test]
    fn parquet_atomic_write_no_partial_file_on_failure() {
        // Try to write into a non-existent absolute path under a file
        // (not a directory) — forces rename to fail. Validate the
        // target path doesn't end up with a partial .parquet file.
        let columns = vec![ColumnHeader {
            name: "x".into(),
            data_type: "INT".into(),
        }];
        let rows = vec![Row(vec![Value::Int(1)])];
        // Create a directory we can target a child of, but make the
        // child path a directory itself so std::fs::rename fails.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.parquet");
        std::fs::create_dir_all(&target).unwrap();
        let result = export_rows(
            &columns,
            &rows,
            ExportFormat::Parquet,
            &target,
            None,
            &ExportOptions::default(),
        );
        assert!(result.is_err(), "rename onto existing directory must error");
        // No leftover staging file under the parent dir.
        let staging = dir.path().join(".out.parquet.tmp");
        assert!(
            !staging.exists(),
            "staging file must be cleaned up on failure"
        );
    }

    #[test]
    fn parquet_round_trips_dates_and_timestamps_as_utc_microseconds() {
        use ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
        use arrow_array::Array;
        use chrono::{NaiveDate, TimeZone, Utc};

        let columns = vec![
            ColumnHeader {
                name: "d".into(),
                data_type: "DATE".into(),
            },
            ColumnHeader {
                name: "ts".into(),
                data_type: "TIMESTAMPTZ".into(),
            },
        ];
        let date = NaiveDate::from_ymd_opt(2026, 6, 3).unwrap();
        let ts = Utc.with_ymd_and_hms(2026, 6, 3, 12, 34, 56).unwrap();
        let rows = vec![
            Row(vec![Value::Date(date), Value::Timestamp(ts)]),
            Row(vec![Value::Null, Value::Null]),
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ts.parquet");
        export_rows(
            &columns,
            &rows,
            ExportFormat::Parquet,
            &path,
            None,
            &ExportOptions::default(),
        )
        .unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        // Both fields should be Timestamp(microsecond, UTC). The brief
        // explicitly maps Date+Timestamp onto the same column type so
        // downstream readers don't need a per-field dispatch.
        for field in builder.schema().fields() {
            assert!(
                matches!(
                    field.data_type(),
                    arrow_schema::DataType::Timestamp(arrow_schema::TimeUnit::Microsecond, Some(_),)
                ),
                "field {} should be Timestamp(us, UTC) — got {:?}",
                field.name(),
                field.data_type(),
            );
        }
        let batch = builder.build().unwrap().next().unwrap().unwrap();
        let d_col = batch
            .column(0)
            .as_any()
            .downcast_ref::<arrow_array::TimestampMicrosecondArray>()
            .expect("date column is Timestamp");
        let ts_col = batch
            .column(1)
            .as_any()
            .downcast_ref::<arrow_array::TimestampMicrosecondArray>()
            .expect("timestamp column is Timestamp");
        assert_eq!(
            d_col.value(0),
            date.and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_micros()
        );
        assert_eq!(ts_col.value(0), ts.timestamp_micros());
        assert!(d_col.is_null(1));
        assert!(ts_col.is_null(1));
    }

    #[test]
    fn markdown_aligns_money_and_decimal_to_the_right() {
        let columns = vec![
            ColumnHeader {
                name: "amount".into(),
                data_type: "MONEY".into(),
            },
            ColumnHeader {
                name: "label".into(),
                data_type: "VARCHAR".into(),
            },
            ColumnHeader {
                name: "weight".into(),
                data_type: "DECIMAL(10,2)".into(),
            },
        ];
        let rows = vec![Row(vec![
            Value::String("$1.23".into()),
            Value::String("foo".into()),
            Value::String("4.50".into()),
        ])];
        let mut buf = Vec::new();
        write_format(&mut buf, ExportFormat::Markdown, &columns, &rows).unwrap();
        let body = String::from_utf8(buf).unwrap();
        // Both money + decimal carry numeric hints → right-aligned.
        let separator = body.lines().nth(1).unwrap();
        assert_eq!(separator, "| ---: | :--- | ---: |");
    }

    #[test]
    fn parquet_rejected_via_write_format_streaming_path() {
        let (columns, rows) = sample_columns_and_rows();
        let mut buf = Vec::new();
        let err = write_format(&mut buf, ExportFormat::Parquet, &columns, &rows).unwrap_err();
        assert!(
            matches!(err, ExportError::Serialise(_)),
            "parquet via write_format must surface a Serialise error: {err:?}"
        );
    }

    // -- M4.1: Parquet silent data loss warning ----------------------------------

    #[test]
    fn parquet_type_mismatch_drops_value_to_null_and_warns() {
        use ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
        use arrow_array::Array;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// A minimal subscriber that counts `WARN`-level events targeting
        /// `narwhal::export::parquet`.
        #[derive(Debug)]
        struct WarnCounter {
            count: AtomicUsize,
        }

        impl tracing::Subscriber for WarnCounter {
            fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
                meta.level() == &tracing::Level::WARN
                    && meta.target().starts_with("narwhal::export::parquet")
            }
            fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(0)
            }
            fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn event(&self, _: &tracing::Event<'_>) {
                self.count.fetch_add(1, Ordering::SeqCst);
            }
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
        }

        let counter = Arc::new(WarnCounter {
            count: AtomicUsize::new(0),
        });
        let _guard = tracing::subscriber::set_default(Arc::clone(&counter));

        // Construct a scenario where a Bool column receives a Float
        // value AFTER the schema sample window, triggering the
        // type-mismatch fallback. SCHEMA_SAMPLE = 100 rows, so we
        // need 101+ Bool rows before the mismatched Float.
        let columns = vec![ColumnHeader {
            name: "flag".into(),
            data_type: "BOOLEAN".into(),
        }];
        let mut rows: Vec<Row> = (0..101).map(|_| Row(vec![Value::Bool(true)])).collect();
        // Row 102: Float doesn't fit a Bool builder.
        rows.push(Row(vec![Value::Float(1.5)]));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mismatch.parquet");
        export_rows(
            &columns,
            &rows,
            ExportFormat::Parquet,
            &path,
            None,
            &ExportOptions::default(),
        )
        .unwrap();

        // Verify the mismatched value was dropped to null.
        let file = std::fs::File::open(&path).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let batch = builder.build().unwrap().next().unwrap().unwrap();
        assert_eq!(batch.num_rows(), 102);
        let col = batch
            .column(0)
            .as_any()
            .downcast_ref::<arrow_array::BooleanArray>()
            .expect("flag is Bool");
        assert!(col.value(0));
        assert!(col.is_null(101), "mismatched value must be dropped as null");

        // Verify the warning was emitted.
        let warn_count = counter.count.load(Ordering::SeqCst);
        assert_eq!(
            warn_count, 1,
            "expected exactly 1 warn about dropped value, got {warn_count}"
        );
    }
}
