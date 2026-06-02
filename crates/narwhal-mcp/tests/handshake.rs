//! End-to-end JSON-RPC handshake tests against an in-process `McpServer`.
//!
//! Uses `tokio::io::duplex` to pipe JSON-RPC traffic through the server's
//! transport-generic `serve` entry point. No subprocess, no real stdio —
//! just two byte streams crossed over.
//!
//! Each test drives an isolated `McpServer` instance against a real
//! sqlite connection so the driver path is exercised end-to-end.

use std::sync::Arc;

use narwhal_config::{ConnectionsFile, CredentialStore, InMemoryStore};
use narwhal_core::{ConnectionConfig, ConnectionParams, SslMode};
use narwhal_mcp::{DriverRegistry, McpServer, ServerContext};
use serde_json::{json, Value};
use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Build a `ServerContext` populated with a single in-memory sqlite
/// connection. `SQLite` needs no on-disk file when the path is `:memory:`,
/// so each test stays hermetic and gets a fresh database.
fn ctx_with_in_memory_sqlite() -> ServerContext {
    let params = ConnectionParams::with(|p| {
        p.path = Some(":memory:".into());
        p.ssl_mode = SslMode::Disable;
    });
    let config = ConnectionConfig {
        id: uuid::Uuid::new_v4(),
        name: "mem".into(),
        driver: "sqlite".into(),
        params,
    };
    ctx_with_connections(vec![config])
}

fn ctx_with_connections(connections: Vec<ConnectionConfig>) -> ServerContext {
    let file = ConnectionsFile {
        logical_relations: Vec::new(),
        connections,
    };
    let drivers = Arc::new(DriverRegistry::with_defaults());
    let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryStore::new());
    ServerContext::new(drivers, Arc::new(file), credentials)
}

/// Drive a list of JSON-RPC messages through a server and collect every
/// response. Notifications (no `id`) produce no response by design and are
/// therefore not represented in the returned vector.
///
/// Wiring: `duplex` returns a peered pair where writes to one half surface
/// as reads on the other. We pin the server to one half, the test driver
/// to the other, and split each side into independent read/write halves
/// so the server's `serve(R, W)` signature is satisfiable.
async fn roundtrip(ctx: ServerContext, messages: &[Value]) -> Vec<Value> {
    let (client_side, server_side) = duplex(16 * 1024);
    let (server_read, server_write) = tokio::io::split(server_side);
    let (client_read, mut client_write) = tokio::io::split(client_side);

    let server = McpServer::new(ctx);
    let server_task = tokio::spawn(async move {
        server
            .serve(server_read, server_write)
            .await
            .expect("server loop");
    });

    for msg in messages {
        let line = format!("{}\n", serde_json::to_string(msg).expect("encode"));
        client_write
            .write_all(line.as_bytes())
            .await
            .expect("write");
    }
    // Graceful half-close: `drop` alone is not always enough for the
    // peer's read half to observe EOF on `tokio::io::split` pairs, but an
    // explicit `shutdown` shuts the write side of the duplex which the
    // server then sees as the end of stream.
    client_write
        .shutdown()
        .await
        .expect("shutdown client write");
    drop(client_write);

    let mut responses = Vec::new();
    let mut reader = BufReader::new(client_read).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("server emitted non-JSON line {line:?}: {e}"));
        responses.push(value);
    }

    server_task.await.expect("server task panicked");
    responses
}

#[tokio::test]
async fn initialize_returns_server_info_and_capabilities() {
    let responses = roundtrip(
        ctx_with_in_memory_sqlite(),
        &[json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"}
            }
        })],
    )
    .await;

    assert_eq!(responses.len(), 1, "expected exactly one response");
    let result = responses[0]
        .get("result")
        .expect("initialize must return result");
    assert_eq!(result["protocolVersion"], "2024-11-05");
    assert_eq!(result["serverInfo"]["name"], "narwhal");
    assert!(
        result["capabilities"]["tools"].is_object(),
        "tools capability must be advertised"
    );
}

#[tokio::test]
async fn tools_list_returns_v0_tools() {
    let responses = roundtrip(
        ctx_with_in_memory_sqlite(),
        &[json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})],
    )
    .await;

    let tools = responses[0]["result"]["tools"]
        .as_array()
        .expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().expect("tool name is a string"))
        .collect();
    assert!(
        names.contains(&"list_connections"),
        "list_connections must be exposed"
    );
    assert!(
        names.contains(&"describe_schema"),
        "describe_schema must be exposed"
    );
    for tool in tools {
        assert!(
            tool["inputSchema"].is_object(),
            "tool {} missing inputSchema",
            tool["name"]
        );
    }
}

#[tokio::test]
async fn list_connections_reports_configured_targets() {
    let responses = roundtrip(
        ctx_with_in_memory_sqlite(),
        &[json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "list_connections", "arguments": {}}
        })],
    )
    .await;

    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .expect("tool emits text content");
    let payload: Value = serde_json::from_str(text).expect("tool body is JSON");
    assert_eq!(payload["count"], 1);
    assert_eq!(payload["connections"][0]["name"], "mem");
    assert_eq!(payload["connections"][0]["driver"], "sqlite");
}

#[tokio::test]
async fn describe_schema_against_unknown_connection_is_tool_error() {
    let responses = roundtrip(
        ctx_with_in_memory_sqlite(),
        &[json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "describe_schema",
                "arguments": {"connection": "does-not-exist"}
            }
        })],
    )
    .await;

    let result = &responses[0]["result"];
    assert_eq!(result["isError"], true, "unknown conn must flag isError");
    let text = result["content"][0]["text"]
        .as_str()
        .expect("error must be text content");
    assert!(text.contains("unknown connection"));
}

#[tokio::test]
async fn unknown_method_returns_method_not_found() {
    let responses = roundtrip(
        ctx_with_in_memory_sqlite(),
        &[json!({"jsonrpc": "2.0", "id": 1, "method": "no/such/thing"})],
    )
    .await;

    let error = responses[0]
        .get("error")
        .expect("unknown method must be a JSON-RPC error");
    assert_eq!(error["code"], -32601, "method not found code");
}

#[tokio::test]
async fn unknown_tool_returns_invalid_params() {
    let responses = roundtrip(
        ctx_with_in_memory_sqlite(),
        &[json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "nope", "arguments": {}}
        })],
    )
    .await;

    let error = responses[0]
        .get("error")
        .expect("unknown tool must be a JSON-RPC error");
    assert_eq!(error["code"], -32602);
}

#[tokio::test]
async fn invalid_json_yields_parse_error_with_null_id() {
    // The roundtrip helper serializes valid JSON, so for the malformed
    // path we wire up the duplex pipe by hand and push raw bytes.
    let ctx = ctx_with_in_memory_sqlite();
    let (client_side, server_side) = duplex(16 * 1024);
    let (server_read, server_write) = tokio::io::split(server_side);
    let (client_read, mut client_write) = tokio::io::split(client_side);

    let server = McpServer::new(ctx);
    let task = tokio::spawn(async move {
        server
            .serve(server_read, server_write)
            .await
            .expect("serve");
    });

    client_write
        .write_all(b"this is not json\n")
        .await
        .expect("write");
    client_write.shutdown().await.expect("shutdown");
    drop(client_write);

    let mut reader = BufReader::new(client_read).lines();
    let line = reader
        .next_line()
        .await
        .expect("read")
        .expect("server emits a response for parse errors");
    let resp: Value = serde_json::from_str(&line).expect("response is JSON");
    assert_eq!(resp["error"]["code"], -32700, "parse error code");
    assert!(resp["id"].is_null(), "id must be null when unrecoverable");

    task.await.expect("server task panicked");
}

#[tokio::test]
async fn notification_produces_no_response_but_loop_keeps_running() {
    let responses = roundtrip(
        ctx_with_in_memory_sqlite(),
        &[
            // Initialized notification — no id, no response expected.
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            // Real request afterwards to confirm the loop kept running.
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        ],
    )
    .await;

    assert_eq!(
        responses.len(),
        1,
        "notification must produce zero responses, the request one"
    );
    assert_eq!(responses[0]["id"], 1);
}

#[tokio::test]
async fn describe_schema_reads_real_sqlite_tables() {
    // Use a temp file so we can pre-populate tables before running the
    // tool. SQLite's `:memory:` is per-connection, so a file is needed
    // for the seed to be visible from the server's freshly-dialed
    // connection.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("describe.db");
    {
        let conn = rusqlite::Connection::open(&path).expect("open");
        conn.execute_batch(
            "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE orders(id INTEGER PRIMARY KEY, user_id INTEGER);
             CREATE VIEW v AS SELECT id FROM users;",
        )
        .expect("seed");
    }

    let params = ConnectionParams::with(|p| {
        p.path = Some(path.to_string_lossy().into());
        p.ssl_mode = SslMode::Disable;
    });
    let config = ConnectionConfig {
        id: uuid::Uuid::new_v4(),
        name: "seeded".into(),
        driver: "sqlite".into(),
        params,
    };

    let responses = roundtrip(
        ctx_with_connections(vec![config]),
        &[json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "describe_schema",
                "arguments": {"connection": "seeded"}
            }
        })],
    )
    .await;

    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .expect("tool emits text");
    let payload: Value = serde_json::from_str(text).expect("body is JSON");
    let kinds: Vec<(&str, &str)> = payload["schemas"][0]["tables"]
        .as_array()
        .expect("tables array")
        .iter()
        .map(|t| {
            (
                t["name"].as_str().expect("name"),
                t["kind"].as_str().expect("kind"),
            )
        })
        .collect();
    assert!(kinds.contains(&("users", "table")));
    assert!(kinds.contains(&("orders", "table")));
    assert!(
        kinds.contains(&("v", "view")),
        "views must be labelled as `view`"
    );
}

#[tokio::test]
async fn oversized_frame_aborts_transport_without_oom() {
    // Reproduces C2: a client streaming bytes without a newline used
    // to be buffered unbounded before the post-read length check fired,
    // so a sufficiently large payload could OOM the process. After the
    // fix `AsyncReadExt::take(MAX + 1)` caps the read at the I/O layer
    // and the transport closes cleanly with an error response.
    //
    // We send ~1.5 MiB of `a` with NO trailing newline. The server must:
    // (1) NOT block forever waiting for `\n`,
    // (2) NOT allocate the full payload,
    // (3) emit an error response, then close.
    let ctx = ctx_with_in_memory_sqlite();
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

    // Stream 1.5 MiB without a newline. Use a write loop because the
    // duplex buffer is only 64 KiB — the server's take-wrapper drains
    // it as we go.
    let payload = vec![b'a'; 64 * 1024];
    let mut written = 0usize;
    let target = 1_500_000usize;
    while written < target {
        match client_write.write(&payload).await {
            Ok(0) => break,
            Ok(n) => written += n,
            // EPIPE / similar — server already aborted the transport,
            // which is the expected outcome.
            Err(_) => break,
        }
    }
    let _ = client_write.shutdown().await;
    drop(client_write);

    let mut reader = BufReader::new(client_read).lines();
    // The server must surface a structured error before closing.
    let line = reader
        .next_line()
        .await
        .expect("read first frame")
        .expect("server must emit an error frame for the oversized payload");
    let resp: Value = serde_json::from_str(&line).expect("response is JSON");
    assert_eq!(
        resp["error"]["code"], -32600,
        "oversized frame must surface as invalid_request: got {resp}"
    );
    let msg = resp["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("cap") || msg.contains("large"),
        "error message must explain the cap: {msg}"
    );

    task.await.expect("server task panicked");
}
