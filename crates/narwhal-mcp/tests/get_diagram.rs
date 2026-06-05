//! Integration tests for the `get_diagram` tool.

use std::sync::Arc;

use narwhal_config::{ConnectionsFile, DynCredentialStore, InMemoryStore};
use narwhal_core::{ConnectionConfig, ConnectionParams, SslMode};
use narwhal_mcp::{DriverRegistry, McpServer, ServerContext};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};

fn seed_sqlite(path: &std::path::Path) {
    let conn = rusqlite::Connection::open(path).expect("open");
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE users (
             id    INTEGER PRIMARY KEY,
             email TEXT NOT NULL UNIQUE
         );
         CREATE TABLE orders (
             id      INTEGER PRIMARY KEY,
             user_id INTEGER NOT NULL,
             status  TEXT,
             FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
         );
         CREATE TABLE order_items (
             order_id   INTEGER NOT NULL,
             product_id INTEGER NOT NULL,
             qty        INTEGER NOT NULL,
             PRIMARY KEY (order_id, product_id),
             FOREIGN KEY (order_id) REFERENCES orders(id) ON DELETE CASCADE
         );
         CREATE TABLE audit (
             id       INTEGER PRIMARY KEY,
             actor_id INTEGER,
             FOREIGN KEY (actor_id) REFERENCES users(id)
         );",
    )
    .expect("seed");
}

fn ctx_for(path: &std::path::Path) -> ServerContext {
    let params = ConnectionParams::with(|p| {
        p.path = Some(path.to_string_lossy().into());
        p.ssl_mode = SslMode::Disable;
    });
    let config = ConnectionConfig {
        id: uuid::Uuid::new_v4(),
        name: "demo".into(),
        driver: "sqlite".into(),
        params,
    };
    let drivers = Arc::new(DriverRegistry::with_defaults());
    let credentials: Arc<dyn DynCredentialStore> = Arc::new(InMemoryStore::new());
    ServerContext::new(
        drivers,
        Arc::new(ConnectionsFile {
            schema_version: None,
            logical_relations: Vec::new(),
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

fn call(connection: &str, arguments: Value) -> Value {
    let mut args = serde_json::json!({"connection": connection});
    if let Value::Object(extras) = arguments {
        let map = args.as_object_mut().expect("object");
        for (k, v) in extras {
            map.insert(k, v);
        }
    }
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "get_diagram", "arguments": args}
    })
}

#[tokio::test]
async fn full_schema_mermaid_renders_all_tables() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(ctx_for(&path), call("demo", json!({}))).await;
    assert_ne!(response["result"]["isError"], true);

    let payload = body(&response);
    assert_eq!(payload["connection"], "demo");
    assert_eq!(payload["format"], "mermaid");
    assert_eq!(payload["tables"], 4);
    assert_eq!(payload["edges"], 3);
    assert!(payload["focused_on"].is_null());

    let source = payload["source"].as_str().expect("source string");
    assert!(source.starts_with("---\ntitle:"));
    assert!(source.contains("erDiagram"));
    // Parent on the LEFT, child on the RIGHT.
    assert!(source.contains("main_users ||--o{ main_orders"));
    assert!(source.contains("main_orders ||--o{ main_order_items"));
    // Nullable FK → `|o--o{`.
    assert!(source.contains("main_users |o--o{ main_audit"));
}

#[tokio::test]
async fn dot_format_returns_digraph() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(ctx_for(&path), call("demo", json!({"format": "dot"}))).await;
    assert_ne!(response["result"]["isError"], true);

    let payload = body(&response);
    assert_eq!(payload["format"], "dot");
    let source = payload["source"].as_str().expect("source string");
    assert!(source.starts_with("digraph schema {"));
    assert!(source.contains("main_orders:user_id -> main_users:id"));
}

#[tokio::test]
async fn focused_table_limits_to_one_hop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(ctx_for(&path), call("demo", json!({"table": "orders"}))).await;
    assert_ne!(response["result"]["isError"], true);

    let payload = body(&response);
    assert_eq!(payload["focused_on"], "main.orders");
    // orders + users (out-edge) + order_items (in-edge) = 3 tables.
    assert_eq!(payload["tables"], 3);
    let source = payload["source"].as_str().expect("source");
    assert!(source.contains("main_orders {"));
    assert!(source.contains("main_users {"));
    assert!(source.contains("main_order_items {"));
    // audit is 2 hops away → must be excluded.
    assert!(!source.contains("main_audit {"));
}

#[tokio::test]
async fn qualified_table_overrides_schema_filter() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(
        ctx_for(&path),
        call(
            "demo",
            json!({"table": "main.users", "schema": "other_ignored"}),
        ),
    )
    .await;
    assert_ne!(response["result"]["isError"], true);
    let payload = body(&response);
    assert_eq!(payload["focused_on"], "main.users");
}

#[tokio::test]
async fn schema_filter_restricts_candidates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(ctx_for(&path), call("demo", json!({"schema": "main"}))).await;
    assert_ne!(response["result"]["isError"], true);
    let payload = body(&response);
    assert_eq!(payload["schema_filter"], "main");
    assert_eq!(payload["tables"], 4);
}

#[tokio::test]
async fn unknown_format_is_tool_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(ctx_for(&path), call("demo", json!({"format": "svg"}))).await;
    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("error text");
    assert!(text.contains("unknown format"));
}

#[tokio::test]
async fn unknown_table_is_tool_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(ctx_for(&path), call("demo", json!({"table": "ghost"}))).await;
    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("error text");
    assert!(text.contains("table not found"));
}

#[tokio::test]
async fn unknown_connection_is_tool_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(ctx_for(&path), call("nope", json!({}))).await;
    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("error text");
    assert!(text.contains("unknown connection"));
}

#[tokio::test]
async fn empty_schema_filter_is_tool_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(
        ctx_for(&path),
        call("demo", json!({"schema": "does_not_exist"})),
    )
    .await;
    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("error text");
    assert!(text.contains("no tables"));
}

#[tokio::test]
async fn workspace_logical_relation_renders_dashed() {
    use std::sync::Arc;
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("db.sqlite");
    seed_sqlite(&db_path);

    // Drop a workspace.toml next to the DB declaring a logical FK
    // from `audit.actor_id` to `users.id` (the FK is already in the
    // fixture; this exercises the orthogonal path — logical edge
    // added against a brand-new pair).
    let ws_dir = dir.path().join(".narwhal");
    std::fs::create_dir_all(&ws_dir).unwrap();
    std::fs::write(
        ws_dir.join("workspace.toml"),
        r#"
[[logical_relation]]
connection  = "demo"
from        = "audit.id"
to          = "orders.id"
cardinality = "many-to-one"
note        = "reconciled async"
"#,
    )
    .unwrap();

    // ServerContext must be told about the workspace root so the
    // logical-relation collector can find it.
    let mut ctx = ctx_for(&db_path);
    let ws = narwhal_mcp::Workspace::discover(dir.path())
        .expect("discover ok")
        .expect("workspace must be found");
    ctx = ctx.with_workspace(Arc::new(ws));

    let response = rpc_one(ctx, call("demo", json!({}))).await;
    assert_ne!(response["result"]["isError"], true);

    let payload = body(&response);
    let source = payload["source"].as_str().expect("source");
    // 4 tables + 1 extra logical edge.
    assert_eq!(payload["edges"], 4, "payload: {payload}");
    // Dashed `..` notation + `[logical]` suffix on the label.
    assert!(
        source.contains("main_orders }o..|| main_audit : \"id [logical]\""),
        "logical edge missing:\n{source}"
    );
}

#[tokio::test]
async fn workspace_logical_relation_unknown_table_is_warned() {
    use std::sync::Arc;
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("db.sqlite");
    seed_sqlite(&db_path);

    let ws_dir = dir.path().join(".narwhal");
    std::fs::create_dir_all(&ws_dir).unwrap();
    std::fs::write(
        ws_dir.join("workspace.toml"),
        r#"
[[logical_relation]]
connection  = "demo"
from        = "ghost.x"
to          = "users.id"
cardinality = "many-to-one"
"#,
    )
    .unwrap();

    let mut ctx = ctx_for(&db_path);
    let ws = narwhal_mcp::Workspace::discover(dir.path())
        .expect("discover ok")
        .expect("workspace must be found");
    ctx = ctx.with_workspace(Arc::new(ws));

    let response = rpc_one(ctx, call("demo", json!({}))).await;
    // Bad logical relation only logs; the call still succeeds with
    // the valid subset (here: 0 logical edges = the original 3).
    assert_ne!(response["result"]["isError"], true);
    let payload = body(&response);
    assert_eq!(payload["edges"], 3, "unknown-table logical must be dropped");
}

#[tokio::test]
async fn tool_appears_in_tools_list() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    seed_sqlite(&path);

    let response = rpc_one(
        ctx_for(&path),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        }),
    )
    .await;
    let tools = response["result"]["tools"].as_array().expect("tools");
    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    assert!(names.contains(&"get_diagram"), "names = {names:?}");
}
