//! Integration tests for `:export csv|json|insert <path>`.
//!
//! Each test creates an `AppCore` with an in-memory SQLite session,
//! seeds a result set, then invokes `:export` to verify file output.

use std::path::PathBuf;

use narwhal_app::core::{AppCore, ResultState};
use narwhal_app::DriverRegistry;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams, Row};
use tempfile::TempDir;
use uuid::Uuid;

fn fixture(database_path: PathBuf) -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "export-test".into(),
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(database_path.to_string_lossy().into_owned()),
                ..Default::default()
            },
        }],
    };
    (registry, connections)
}

/// Seed a simple table and run a query that returns rows.
async fn seed_and_query(core: &mut AppCore, db_path: PathBuf, sql: &str) {
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, bio TEXT);
             INSERT INTO users VALUES (1, 'alice', NULL);
             INSERT INTO users VALUES (2, 'bob', 'hello, world');
             INSERT INTO users VALUES (3, 'carol', 'she said \"hi\"');",
        )
        .unwrap();
    }
    core.execute_command("open export-test");
    core.insert_into_editor(sql);
    core.execute_command("run");
    core.drain_run_updates().await;
}

fn get_rows(core: &AppCore) -> Vec<Row> {
    match &core.tabs()[core.active_tab()].results().active_state() {
        ResultState::Rows { rows, .. } => rows.clone(),
        _ => panic!("expected Rows result"),
    }
}

// ---------------------------------------------------------------------------
// Test 1: CSV round-trip with special characters
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn csv_round_trip_with_special_chars() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);

    seed_and_query(
        &mut core,
        db_path,
        "SELECT id, name, bio FROM users ORDER BY id",
    )
    .await;
    let rows = get_rows(&core);
    assert_eq!(rows.len(), 3);

    let out_path = dir.path().join("out.csv");
    core.execute_command(&format!("export csv {}", out_path.display()));

    let body = std::fs::read_to_string(&out_path).unwrap();

    // RFC 4180: CRLF line endings, quoted fields for special chars.
    // Header: id,name,bio\r\n
    // Row 1: 1,alice,\r\n (NULL → empty)
    // Row 2: 2,bob,"hello, world"\r\n (comma in bio)
    // Row 3: 3,carol,"she said ""hi"""\r\n (embedded quotes)
    assert!(body.starts_with("id,name,bio\r\n"));
    assert!(body.contains("1,alice,\r\n"));
    assert!(body.contains("2,bob,\"hello, world\"\r\n"));
    assert!(body.contains("3,carol,\"she said \"\"hi\"\"\"\r\n"));
}

// ---------------------------------------------------------------------------
// Test 2: CSV NULL becomes empty field
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn csv_null_becomes_empty_field() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);

    seed_and_query(
        &mut core,
        db_path,
        "SELECT id, name, bio FROM users ORDER BY id",
    )
    .await;

    let out_path = dir.path().join("out.csv");
    core.execute_command(&format!("export csv {}", out_path.display()));

    let body = std::fs::read_to_string(&out_path).unwrap();
    // First data row should have empty third field for NULL.
    let lines: Vec<&str> = body.split("\r\n").collect();
    assert!(lines.len() >= 2);
    // Row 1: "1,alice," — trailing comma, no value after
    assert!(
        lines[1].starts_with("1,alice,"),
        "NULL should become empty field, got line: '{}'",
        lines[1]
    );
    // The bio field (3rd) is empty — line ends with comma or has no third field content.
    let fields: Vec<&str> = lines[1].splitn(3, ',').collect();
    assert_eq!(fields[2], "", "NULL bio should be empty string");
}

// ---------------------------------------------------------------------------
// Test 3: JSON array of objects
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn json_array_of_objects() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);

    seed_and_query(
        &mut core,
        db_path,
        "SELECT id, name, bio FROM users ORDER BY id",
    )
    .await;

    let out_path = dir.path().join("out.json");
    core.execute_command(&format!("export json {}", out_path.display()));

    let body = std::fs::read_to_string(&out_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    // NULL → null in JSON
    assert_eq!(arr[0]["bio"], serde_json::Value::Null);
    // Numbers stay numeric
    assert!(arr[0]["id"].is_number());
    // Strings stay strings
    assert_eq!(arr[1]["name"], "bob");
}

// ---------------------------------------------------------------------------
// Test 4: JSON invalid UTF-8 uses $bytes sentinel
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn json_invalid_utf8_uses_bytes_sentinel() {
    use narwhal_app::ExportFormat;
    use narwhal_core::{ColumnHeader, Value};

    // Directly test the export function with invalid UTF-8 bytes.
    let columns = vec![ColumnHeader {
        name: "data".into(),
        data_type: "BLOB".into(),
    }];
    let rows = vec![Row(vec![Value::Bytes(vec![0xFF, 0xFE])])];
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");

    narwhal_app::export::export_rows(&columns, &rows, ExportFormat::Json, &path, None).unwrap();

    let body = std::fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    let obj = parsed[0]["data"].as_object().unwrap();
    assert!(
        obj.contains_key("$bytes"),
        "expected $bytes key for invalid UTF-8"
    );
}

// ---------------------------------------------------------------------------
// Test 5: INSERT single-table round trip
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insert_single_table_round_trip() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);

    // SELECT * FROM users is a single-table query → source_table should be detected.
    seed_and_query(&mut core, db_path, "SELECT * FROM users ORDER BY id").await;

    // Verify source_table was detected
    match &core.tabs()[core.active_tab()].results().active_state() {
        ResultState::Rows { source_table, .. } => {
            assert!(
                source_table.is_some(),
                "source_table should be detected for SELECT * FROM users"
            );
            let table = source_table.as_ref().unwrap();
            assert_eq!(table.table, "users");
        }
        other => panic!("expected Rows, got {other:?}"),
    }

    let out_path = dir.path().join("out.sql");
    core.execute_command(&format!("export insert {}", out_path.display()));

    let body = std::fs::read_to_string(&out_path).unwrap();

    // Parse the INSERT statements back into SQLite and verify.
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE users (id INTEGER, name TEXT, bio TEXT);")
        .unwrap();
    conn.execute_batch(&body).unwrap();

    let mut stmt = conn
        .prepare("SELECT id, name, bio FROM users ORDER BY id")
        .unwrap();
    let rows: Vec<(i64, String, Option<String>)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].0, 1);
    assert_eq!(rows[0].1, "alice");
    assert_eq!(rows[0].2, None); // NULL
    assert_eq!(rows[1].1, "bob");
    assert_eq!(rows[2].1, "carol");
    assert_eq!(rows[2].2, Some("she said \"hi\"".into()));
}

// ---------------------------------------------------------------------------
// Test 6: INSERT without source table errors
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insert_without_source_table_errors() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);

    // SELECT 1+1 is a computed expression — no source_table.
    seed_and_query(&mut core, db_path, "SELECT 1+1 AS result").await;

    // Verify source_table is None
    match &core.tabs()[core.active_tab()].results().active_state() {
        ResultState::Rows { source_table, .. } => {
            assert!(
                source_table.is_none(),
                "source_table should be None for SELECT 1+1"
            );
        }
        other => panic!("expected Rows, got {other:?}"),
    }

    let out_path = dir.path().join("out.sql");
    assert!(!out_path.exists(), "file must not exist before export");
    core.execute_command(&format!("export insert {}", out_path.display()));

    // File must NOT have been created.
    assert!(
        !out_path.exists(),
        "file must not be created when source_table is None"
    );

    // Status should indicate the error.
    let status = core.status_message();
    assert!(
        status.contains("export failed"),
        "expected export failure message, got: {status}"
    );
}
