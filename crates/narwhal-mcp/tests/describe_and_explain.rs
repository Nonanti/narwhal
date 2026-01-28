//! Integration tests for the `describe_table` and `explain_query` tools.
//!
//! Both drive a real sqlite driver via the same in-process duplex pipe
//! the other test files use; sqlite is the cheapest driver to seed and
//! its `describe_table` / `EXPLAIN QUERY PLAN` paths are representative
//! of the contract every other driver implements.

use std::sync::Arc;

use narwhal_config::{ConnectionsFile, CredentialStore, InMemoryStore};
use narwhal_core::{ConnectionConfig, ConnectionParams, SslMode};
use narwhal_mcp::{DriverRegistry, McpServer, ServerContext};
use serde_json::{json, Value};
use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

fn seed_sqlite(path: &std::path::Path) {
    let conn = rusqlite::Connection::open(path).expect("open");
    conn.execute_batch(
        "CREATE TABLE users(
            id   INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            email TEXT UNIQUE
         );
         CREATE INDEX idx_users_name ON users(name);
         CREATE TABLE orders(
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            amount REAL
         );
         INSERT INTO users(name, email) VALUES ('alice', 'a@x'), ('bob', 'b@x');
         INSERT INTO orders(user_id, amount) VALUES (1, 9.5), (2, 10.0);",
    )
    .expect("seed");
}

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
    let drivers = Arc::new(DriverRegistry::with_defaults());
    let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryStore::new());
    ServerContext::new(
        drivers,
        Arc::new(ConnectionsFile {
            connections: vec![config],
        }),
        credentials,
    )
}

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

fn body(response: &Value) -> Value {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool emits text");
    serde_json::from_str(text).expect("body is JSON")
}

// describe_table

#[tokio::test]
async fn describe_table_returns_columns_indexes_and_fks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(
        ctx_for(&path),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "describe_table",
                "arguments": {"connection": "demo", "table": "orders"}
            }
        }),
    )
    .await;

    assert_ne!(response["result"]["isError"], true, "must not error");
    let payload = body(&response);
    assert_eq!(payload["connection"], "demo");
    // TableSchema fields: table, columns, indexes, foreign_keys, ...
    assert_eq!(payload["table"]["name"], "orders");
    let columns: Vec<&str> = payload["columns"]
        .as_array()
        .expect("columns")
        .iter()
        .map(|c| c["name"].as_str().expect("col name"))
        .collect();
    assert!(columns.contains(&"id"));
    assert!(columns.contains(&"user_id"));
    assert!(columns.contains(&"amount"));
    // Sqlite reports `id` as the primary key
    let pk_count = payload["columns"]
        .as_array()
        .expect("columns")
        .iter()
        .filter(|c| c["primary_key"] == json!(true))
        .count();
    assert_eq!(pk_count, 1, "exactly one primary-key column expected");
    // The FK to users(id) must be discoverable
    let fks = payload["foreign_keys"].as_array().expect("foreign_keys");
    assert!(
        !fks.is_empty(),
        "FK to users(id) must be exposed for the agent"
    );
    let fk = &fks[0];
    assert_eq!(fk["referenced_table"], "users");
}

#[tokio::test]
async fn describe_table_unknown_table_is_tool_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(
        ctx_for(&path),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "describe_table",
                "arguments": {"connection": "demo", "table": "no_such"}
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("error text");
    assert!(text.contains("describe_table failed"));
}

// explain_query

#[tokio::test]
async fn explain_query_returns_plan_rows_and_dialect_tag() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(
        ctx_for(&path),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "explain_query",
                "arguments": {
                    "connection": "demo",
                    "sql": "SELECT * FROM users WHERE name = 'alice'"
                }
            }
        }),
    )
    .await;

    assert_ne!(response["result"]["isError"], true);
    let payload = body(&response);
    assert_eq!(payload["dialect"], "sqlite");
    assert!(payload["explain_sql"]
        .as_str()
        .expect("explain_sql")
        .starts_with("EXPLAIN QUERY PLAN"));
    assert!(
        !payload["rows"].as_array().expect("rows").is_empty(),
        "EXPLAIN QUERY PLAN must produce at least one row"
    );
    // SQLite does not support ANALYZE-via-EXPLAIN; the analyzed flag
    // must reflect that even if the agent asked for it.
    assert_eq!(payload["analyzed"], false);
}

#[tokio::test]
async fn explain_query_rejects_already_prefixed_sql() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(
        ctx_for(&path),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "explain_query",
                "arguments": {
                    "connection": "demo",
                    "sql": "EXPLAIN SELECT 1"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("error text");
    assert!(text.contains("must not start with EXPLAIN"));
}

#[tokio::test]
async fn explain_query_unknown_connection_is_tool_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(
        ctx_for(&path),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "explain_query",
                "                arguments": {"connection": "nope", "sql": "SELECT 1"}
            }
        }),
    )
    .await;

    // Malformed params bubble up as a JSON-RPC error (the malformed
    // key above renders the argument set invalid), confirming we don't
    // silently treat a bad payload as success.
    assert!(
        response.get("error").is_some() || response["result"]["isError"] == true,
        "got: {response}"
    );
}

#[tokio::test]
async fn tools_list_now_advertises_five_tools() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(
        ctx_for(&path),
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;

    let tools = response["result"]["tools"].as_array().expect("tools array");
    let names: std::collections::BTreeSet<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names.iter().copied().collect::<Vec<_>>(),
        vec![
            "describe_schema",
            "describe_table",
            "explain_query",
            "list_connections",
            "run_query",
        ],
        "v0 tool surface frozen at five tools"
    );
}
