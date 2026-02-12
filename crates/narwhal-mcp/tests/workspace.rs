//! Workspace ACL behaviour at the MCP layer.
//!
//! Builds two connections in a single `ConnectionsFile`, attaches a
//! workspace that only allow-lists one of them, and confirms every tool
//! path observes the ACL. Also exercises the `allow_writes = false`
//! switch by verifying that `run_query` with `read_only=false` is
//! rejected when the workspace forbids writes.

use std::sync::Arc;

use narwhal_config::{ConnectionsFile, CredentialStore, InMemoryStore};
use narwhal_core::{ConnectionConfig, ConnectionParams, SslMode};
use narwhal_mcp::workspace::{Workspace, WorkspaceFile};
use narwhal_mcp::{DriverRegistry, McpServer, ServerContext};
use serde_json::{json, Value};
use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

fn seed_sqlite(path: &std::path::Path) {
    let conn = rusqlite::Connection::open(path).expect("open");
    conn.execute_batch(
        "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT);
         INSERT INTO users(name) VALUES ('alice');",
    )
    .expect("seed");
}

/// Two named connections — `staging` and `prod` — both pointing at the
/// same on-disk file (the workspace ACL is independent of the driver).
fn two_connections(path: &std::path::Path) -> Vec<ConnectionConfig> {
    let params = ConnectionParams {
        path: Some(path.to_string_lossy().into()),
        ssl_mode: SslMode::Disable,
        ..ConnectionParams::default()
    };
    vec![
        ConnectionConfig {
            id: uuid::Uuid::new_v4(),
            name: "staging".into(),
            driver: "sqlite".into(),
            params: params.clone(),
        },
        ConnectionConfig {
            id: uuid::Uuid::new_v4(),
            name: "prod".into(),
            driver: "sqlite".into(),
            params,
        },
    ]
}

fn ctx_with(connections: Vec<ConnectionConfig>, workspace: Option<Workspace>) -> ServerContext {
    let drivers = Arc::new(DriverRegistry::with_defaults());
    let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryStore::new());
    let mut ctx = ServerContext::new(
        drivers,
        Arc::new(ConnectionsFile { connections }),
        credentials,
    );
    if let Some(ws) = workspace {
        ctx = ctx.with_workspace(Arc::new(ws));
    }
    ctx
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
    let response = reader.next_line().await.expect("read").expect("response");
    task.await.expect("server task panicked");
    serde_json::from_str(&response).expect("response is JSON")
}

fn body(response: &Value) -> Value {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool emits text");
    serde_json::from_str(text).expect("body is JSON")
}

#[tokio::test]
async fn list_connections_hides_disallowed_entries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db.sqlite");
    seed_sqlite(&db);

    let workspace = Workspace {
        root: dir.path().to_path_buf(),
        file: WorkspaceFile {
            allowed_connections: vec!["staging".into()],
            allow_writes: true,
        },
    };

    let response = rpc_one(
        ctx_with(two_connections(&db), Some(workspace)),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "list_connections", "arguments": {}}
        }),
    )
    .await;

    let payload = body(&response);
    assert_eq!(payload["count"], 1, "only one connection is visible");
    assert_eq!(payload["connections"][0]["name"], "staging");
}

#[tokio::test]
async fn empty_allow_list_means_everything_is_visible() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db.sqlite");
    seed_sqlite(&db);

    let workspace = Workspace {
        root: dir.path().to_path_buf(),
        file: WorkspaceFile {
            allowed_connections: vec![],
            allow_writes: true,
        },
    };

    let response = rpc_one(
        ctx_with(two_connections(&db), Some(workspace)),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "list_connections", "arguments": {}}
        }),
    )
    .await;

    assert_eq!(body(&response)["count"], 2, "empty ACL exposes everything");
}

#[tokio::test]
async fn describe_schema_rejects_disallowed_connection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db.sqlite");
    seed_sqlite(&db);

    let workspace = Workspace {
        root: dir.path().to_path_buf(),
        file: WorkspaceFile {
            allowed_connections: vec!["staging".into()],
            allow_writes: true,
        },
    };

    let response = rpc_one(
        ctx_with(two_connections(&db), Some(workspace)),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "describe_schema",
                "arguments": {"connection": "prod"}
            }
        }),
    )
    .await;

    assert_eq!(
        response["result"]["isError"], true,
        "ACL violation must surface as a tool-level error"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("error text");
    assert!(
        text.contains("unknown connection"),
        "agent sees the same shape as a typo'd name: {text}"
    );
}

#[tokio::test]
async fn run_query_refuses_writes_when_workspace_forbids() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db.sqlite");
    seed_sqlite(&db);

    let workspace = Workspace {
        root: dir.path().to_path_buf(),
        file: WorkspaceFile {
            allowed_connections: vec![],
            allow_writes: false,
        },
    };

    let response = rpc_one(
        ctx_with(two_connections(&db), Some(workspace)),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "run_query",
                "arguments": {
                    "connection": "staging",
                    "sql": "INSERT INTO users(name) VALUES ('eve')",
                    "read_only": false
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("error text");
    assert!(
        text.contains("disallows writes"),
        "message must explain the workspace ACL: {text}"
    );

    let conn = rusqlite::Connection::open(&db).expect("open");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM users WHERE name='eve'", [], |r| {
            r.get(0)
        })
        .expect("count");
    assert_eq!(count, 0, "the write must not have happened");
}

#[tokio::test]
async fn run_query_read_only_still_works_under_strict_workspace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db.sqlite");
    seed_sqlite(&db);

    let workspace = Workspace {
        root: dir.path().to_path_buf(),
        file: WorkspaceFile {
            allowed_connections: vec!["staging".into()],
            allow_writes: false,
        },
    };

    let response = rpc_one(
        ctx_with(two_connections(&db), Some(workspace)),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "run_query",
                "arguments": {
                    "connection": "staging",
                    "sql": "SELECT name FROM users"
                }
            }
        }),
    )
    .await;

    assert_ne!(
        response["result"]["isError"], true,
        "read-only mode must remain available even in strict workspaces"
    );
    let payload = body(&response);
    assert_eq!(payload["row_count"], 1);
}

#[tokio::test]
async fn force_read_only_flag_rejects_writes_even_with_permissive_workspace() {
    // Reproduces C1: `narwhal --read-only mcp` MUST refuse `read_only=false`
    // even when the workspace ACL says `allow_writes = true`. Before the
    // fix, the global flag was silently dropped at the `run_mcp` entry
    // point and the write would land in the database.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db.sqlite");
    seed_sqlite(&db);

    let workspace = Workspace {
        root: dir.path().to_path_buf(),
        file: WorkspaceFile {
            allowed_connections: vec![],
            // Permissive workspace — normally would let writes through.
            allow_writes: true,
        },
    };

    let ctx = ctx_with(two_connections(&db), Some(workspace)).with_force_read_only(true);

    let response = rpc_one(
        ctx,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "run_query",
                "arguments": {
                    "connection": "staging",
                    "sql": "INSERT INTO users(name) VALUES ('mallory')",
                    "read_only": false
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("error text");
    assert!(
        text.contains("--read-only"),
        "error must explain the flag, not the workspace: {text}"
    );

    let conn = rusqlite::Connection::open(&db).expect("open");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM users WHERE name='mallory'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(count, 0, "the write must not have happened");
}

#[tokio::test]
async fn force_read_only_flag_still_allows_reads() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db.sqlite");
    seed_sqlite(&db);

    let ctx = ctx_with(two_connections(&db), None).with_force_read_only(true);

    let response = rpc_one(
        ctx,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "run_query",
                "arguments": {
                    "connection": "staging",
                    "sql": "SELECT name FROM users"
                }
            }
        }),
    )
    .await;

    assert_ne!(response["result"]["isError"], true);
    let payload = body(&response);
    assert_eq!(payload["row_count"], 1);
}
