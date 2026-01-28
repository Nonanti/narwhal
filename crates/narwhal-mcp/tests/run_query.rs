//! Integration tests for the `run_query` tool.
//!
//! Driven through the same `tokio::io::duplex` pipe the handshake tests
//! use so we exercise the real dispatch + tool-call path against a real
//! sqlite driver.

use std::sync::Arc;

use narwhal_config::{ConnectionsFile, CredentialStore, InMemoryStore};
use narwhal_core::{ConnectionConfig, ConnectionParams, SslMode};
use narwhal_history::{Journal, JournalReader};
use narwhal_mcp::{DriverRegistry, McpServer, ServerContext};
use serde_json::{json, Value};
use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Seed a sqlite database at `path` with two tables and a few rows so
/// every test starts from a known fixture.
fn seed_sqlite(path: &std::path::Path) {
    let conn = rusqlite::Connection::open(path).expect("open");
    conn.execute_batch(
        "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL);
         INSERT INTO users(name) VALUES ('alice'), ('bob'), ('carol');
         CREATE TABLE orders(id INTEGER PRIMARY KEY, user_id INTEGER, amount REAL);
         INSERT INTO orders(user_id, amount) VALUES (1, 9.5), (2, 10.0);",
    )
    .expect("seed");
}

/// Build a context pointing at the given seeded sqlite file.
fn ctx_for(path: &std::path::Path) -> ServerContext {
    let params = ConnectionParams {
        path: Some(path.to_string_lossy().into()),
        ssl_mode: SslMode::Disable,
        ..ConnectionParams::default()
    };
    let config = ConnectionConfig {
        id: uuid::Uuid::new_v4(),
        name: "demo".into(),
        driver: "sqlite".into(),
        params,
    };
    let connections = ConnectionsFile {
        connections: vec![config],
    };
    let drivers = Arc::new(DriverRegistry::with_defaults());
    let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryStore::new());
    ServerContext::new(drivers, Arc::new(connections), credentials)
}

/// Drive a single JSON-RPC message through an in-process server and
/// return the response value.
async fn rpc_one(ctx: ServerContext, request: Value) -> Value {
    let (client_side, server_side) = duplex(64 * 1024);
    let (server_read, server_write) = tokio::io::split(server_side);
    let (client_read, mut client_write) = tokio::io::split(client_side);

    let server = McpServer::new(ctx);
    let task = tokio::spawn(async move {
        server
            .serve(server_read, server_write)
            .await
            .expect("serve");
    });

    let line = format!("{}\n", serde_json::to_string(&request).expect("encode"));
    client_write
        .write_all(line.as_bytes())
        .await
        .expect("write");
    client_write.shutdown().await.expect("shutdown");
    drop(client_write);

    let mut reader = BufReader::new(client_read).lines();
    let response = reader
        .next_line()
        .await
        .expect("read")
        .expect("server emits a response");
    task.await.expect("server task panicked");

    serde_json::from_str(&response).expect("response is JSON")
}

/// Shortcut for building a `tools/call` request for `run_query`.
fn call_run_query(args: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "run_query", "arguments": args}
    })
}

/// Extract and parse the JSON body returned in `content[0].text`.
fn tool_body(response: &Value) -> Value {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool emits text");
    serde_json::from_str(text).expect("tool body is JSON")
}

#[tokio::test]
async fn select_returns_rows_with_columns_and_count() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(
        ctx_for(&path),
        call_run_query(json!({
            "connection": "demo",
            "sql": "SELECT id, name FROM users ORDER BY id"
        })),
    )
    .await;

    assert_ne!(response["result"]["isError"], true, "must not error");
    let body = tool_body(&response);
    assert_eq!(body["read_only"], true);
    assert_eq!(body["row_count"], 3);
    assert_eq!(body["truncated"], false);
    let cols: Vec<&str> = body["columns"]
        .as_array()
        .expect("columns")
        .iter()
        .map(|c| c["name"].as_str().expect("name"))
        .collect();
    assert_eq!(cols, vec!["id", "name"]);
    let names: Vec<&str> = body["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| row[1].as_str().expect("name col"))
        .collect();
    assert_eq!(names, vec!["alice", "bob", "carol"]);
}

#[tokio::test]
async fn write_statement_rejected_in_read_only_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(
        ctx_for(&path),
        call_run_query(json!({
            "connection": "demo",
            "sql": "INSERT INTO users(name) VALUES ('mallory')"
        })),
    )
    .await;

    assert_eq!(
        response["result"]["isError"], true,
        "INSERT must be rejected by the guard"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("error text");
    assert!(text.contains("read-only guard"), "got: {text}");

    // Confirm the write did not happen.
    let conn = rusqlite::Connection::open(&path).expect("open");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM users WHERE name='mallory'", [], |r| {
            r.get(0)
        })
        .expect("count");
    assert_eq!(count, 0, "guarded write must not reach the database");
}

#[tokio::test]
async fn rollback_sandwich_unwinds_writes_that_sneak_past_guard() {
    // We can pretend a write `WITH` query (a common edge case the guard
    // permits because it starts with `WITH`) actually attempts to mutate
    // state on a driver that allowed it. SQLite is happy with
    // `WITH new AS (SELECT 1) INSERT INTO users(name) SELECT '...'` but
    // that statement begins with WITH so the guard lets it through. The
    // ROLLBACK sandwich is the second line of defence — confirm it
    // unwinds the write.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(
        ctx_for(&path),
        call_run_query(json!({
            "connection": "demo",
            "sql": "WITH new(n) AS (VALUES ('mallory')) INSERT INTO users(name) SELECT n FROM new"
        })),
    )
    .await;

    // The statement *might* succeed (SQLite happily INSERTs inside a
    // transaction) or fail (depending on whether sqlite considers the
    // WITH-prefixed INSERT legal under our BEGIN). Either way, the row
    // must not be visible after we close the sandbox.
    let _ = response;
    let conn = rusqlite::Connection::open(&path).expect("open");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM users WHERE name='mallory'", [], |r| {
            r.get(0)
        })
        .expect("count");
    assert_eq!(count, 0, "ROLLBACK sandwich must unwind the write");
}

#[tokio::test]
async fn read_only_false_bypasses_guard_and_persists() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(
        ctx_for(&path),
        call_run_query(json!({
            "connection": "demo",
            "sql": "INSERT INTO users(name) VALUES ('dave')",
            "read_only": false
        })),
    )
    .await;

    assert_ne!(response["result"]["isError"], true, "must succeed");
    let body = tool_body(&response);
    assert_eq!(body["read_only"], false);

    let conn = rusqlite::Connection::open(&path).expect("open");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM users WHERE name='dave'", [], |r| {
            r.get(0)
        })
        .expect("count");
    assert_eq!(count, 1, "explicit write must persist");
}

#[tokio::test]
async fn limit_truncates_and_sets_flag() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(
        ctx_for(&path),
        call_run_query(json!({
            "connection": "demo",
            "sql": "SELECT id, name FROM users",
            "limit": 2
        })),
    )
    .await;

    let body = tool_body(&response);
    assert_eq!(body["row_count"], 3, "row_count reports the true total");
    assert_eq!(body["truncated"], true);
    assert_eq!(body["rows"].as_array().expect("rows").len(), 2);
    assert_eq!(body["limit"], 2);
}

#[tokio::test]
async fn syntax_error_is_tool_level_error_not_jsonrpc() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(
        ctx_for(&path),
        call_run_query(json!({
            "connection": "demo",
            "sql": "SELECT * FROM no_such_table"
        })),
    )
    .await;

    assert_eq!(response["result"]["isError"], true);
    assert!(
        response.get("error").is_none(),
        "must not be a JSON-RPC error"
    );
}

#[tokio::test]
async fn audit_journal_records_mcp_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("db.sqlite");
    seed_sqlite(&db_path);
    let history_path = dir.path().join("history.jsonl");

    let ctx = ctx_for(&db_path).with_journal(Arc::new(
        Journal::open(&history_path).await.expect("open journal"),
    ));

    let _ = rpc_one(
        ctx,
        call_run_query(json!({
            "connection": "demo",
            "sql": "SELECT 1"
        })),
    )
    .await;

    // Give the async append a moment to flush. The journal uses a Mutex
    // around an OpenOptions write so the await above guarantees order,
    // but the file system metadata may lag — we re-open to read.
    let reader = JournalReader::open(&history_path).expect("reader");
    let entries: Vec<_> = reader
        .into_iter()
        .collect::<Result<_, _>>()
        .expect("entries");
    assert_eq!(entries.len(), 1, "exactly one audit entry expected");
    let entry = &entries[0];
    assert_eq!(entry.source.as_deref(), Some("mcp"));
    assert_eq!(entry.connection_name.as_deref(), Some("demo"));
    assert_eq!(entry.driver.as_deref(), Some("sqlite"));
    assert!(
        entry.sql.contains("SELECT 1"),
        "audit must capture the SQL: {}",
        entry.sql
    );
}

#[tokio::test]
async fn bind_parameters_substitute_into_placeholders() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(
        ctx_for(&path),
        call_run_query(json!({
            "connection": "demo",
            "sql": "SELECT name FROM users WHERE name = ?",
            "params": ["alice"]
        })),
    )
    .await;

    assert_ne!(response["result"]["isError"], true);
    let body = tool_body(&response);
    assert_eq!(body["row_count"], 1);
    assert_eq!(body["rows"][0][0], "alice");
}

#[tokio::test]
async fn bind_parameters_avoid_sql_injection_in_string_param() {
    // A literal string containing a quote and a SQL fragment is treated
    // as data, not code. The query returns zero rows because no user
    // is literally named `'; DROP TABLE users; --`.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let injection = "'; DROP TABLE users; --";
    let response = rpc_one(
        ctx_for(&path),
        call_run_query(json!({
            "connection": "demo",
            "sql": "SELECT name FROM users WHERE name = ?",
            "params": [injection]
        })),
    )
    .await;

    assert_ne!(response["result"]["isError"], true);
    let body = tool_body(&response);
    assert_eq!(body["row_count"], 0, "injection payload must match no rows");

    // Confirm the table still exists — the parameter binding prevented
    // the DROP from being parsed as code.
    let conn = rusqlite::Connection::open(&path).expect("open");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
        .expect("count");
    assert_eq!(count, 3, "users table must still hold the seeded rows");
}

#[tokio::test]
async fn bind_parameter_count_mismatch_is_tool_error() {
    // Two placeholders, one param — the driver rejects, we surface as
    // a tool-level error so the agent retries with the right shape.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(
        ctx_for(&path),
        call_run_query(json!({
            "connection": "demo",
            "sql": "SELECT * FROM users WHERE id = ? AND name = ?",
            "params": [1]
        })),
    )
    .await;

    assert_eq!(response["result"]["isError"], true);
}

#[tokio::test]
async fn bind_bytes_param_via_base64_envelope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    {
        let conn = rusqlite::Connection::open(&path).expect("open");
        conn.execute_batch("CREATE TABLE blobs(id INTEGER PRIMARY KEY, payload BLOB);")
            .expect("seed");
    }

    // base64("hello") == "aGVsbG8="
    let response = rpc_one(
        ctx_for(&path),
        call_run_query(json!({
            "connection": "demo",
            "sql": "SELECT length(?)",
            "params": [{"$bytes_base64": "aGVsbG8="}]
        })),
    )
    .await;

    let body = tool_body(&response);
    assert_eq!(
        body["rows"][0][0], 5,
        "bytes round-tripped as a 5-byte BLOB"
    );
}

#[tokio::test]
async fn audit_marks_explicit_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("db.sqlite");
    seed_sqlite(&db_path);
    let history_path = dir.path().join("history.jsonl");

    let ctx = ctx_for(&db_path).with_journal(Arc::new(
        Journal::open(&history_path).await.expect("open journal"),
    ));

    let _ = rpc_one(
        ctx,
        call_run_query(json!({
            "connection": "demo",
            "sql": "INSERT INTO users(name) VALUES ('eve')",
            "read_only": false
        })),
    )
    .await;

    let reader = JournalReader::open(&history_path).expect("reader");
    let entries: Vec<_> = reader
        .into_iter()
        .collect::<Result<_, _>>()
        .expect("entries");
    let entry = &entries[0];
    assert!(
        entry.sql.starts_with("-- mcp: read_only=false"),
        "write entries must be marked: {}",
        entry.sql
    );
}
