//! T2-T5-C end-to-end test: dynamic plugin tools are listed by
//! `tools/list` and reachable via `tools/call`.
//!
//! No real WASM plugin is involved here \u2014 the WIT bridge is v2.1
//! work. Instead we register the dynamic tool through
//! `ToolRegistry::register_dynamic` directly, which is the same
//! registration path the future plugin host will use.

use std::sync::Arc;

use async_trait::async_trait;
use narwhal_config::{ConnectionsFile, CredentialError, CredentialStore};
use narwhal_mcp::tools::{DynamicTool, RegistrationOutcome, ToolOutput, ToolRegistry};
use narwhal_mcp::{DriverRegistry, McpServer, ServerContext};
use secrecy::SecretString;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};
use uuid::Uuid;

struct NoopStore;

#[async_trait]
impl CredentialStore for NoopStore {
    async fn get(&self, _: Uuid) -> Result<Option<SecretString>, CredentialError> {
        Ok(None)
    }
    async fn set(&self, _: Uuid, _: SecretString) -> Result<(), CredentialError> {
        Ok(())
    }
    async fn delete(&self, _: Uuid) -> Result<(), CredentialError> {
        Ok(())
    }
}

fn build_context() -> ServerContext {
    let file = ConnectionsFile {
        schema_version: None,
        logical_relations: Vec::new(),
        connections: Vec::new(),
    };
    ServerContext::new(
        Arc::new(DriverRegistry::with_defaults()),
        Arc::new(file),
        Arc::new(NoopStore),
    )
}

fn hello_mcp_tool() -> DynamicTool {
    DynamicTool {
        name: "hello_mcp".to_owned(),
        description: "Echo the agent's `name` argument back as `Hello, <name>`.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            },
            "required": ["name"]
        }),
        source: "example-plugin".to_owned(),
        handler: Arc::new(|_ctx, args| {
            Box::pin(async move {
                let name = args
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("anonymous");
                Ok(ToolOutput::ok(format!("Hello, {name}")))
            })
        }),
    }
}

async fn roundtrip(server: McpServer, messages: &[Value]) -> Vec<Value> {
    let (client_side, server_side) = duplex(16 * 1024);
    let (server_read, server_write) = tokio::io::split(server_side);
    let (client_read, mut client_write) = tokio::io::split(client_side);

    let task = tokio::spawn(async move {
        server
            .serve(server_read, server_write)
            .await
            .expect("serve");
    });

    for msg in messages {
        let line = format!("{}\n", serde_json::to_string(msg).expect("encode"));
        client_write
            .write_all(line.as_bytes())
            .await
            .expect("write");
    }
    client_write.shutdown().await.expect("shutdown");
    drop(client_write);

    let mut out = Vec::new();
    let mut reader = BufReader::new(client_read).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line).expect("json");
        out.push(value);
    }
    task.await.expect("task");
    out
}

#[tokio::test]
async fn dynamic_tool_appears_in_tools_list() {
    let mut registry = ToolRegistry::with_defaults();
    let outcome = registry.register_dynamic(hello_mcp_tool());
    assert_eq!(outcome, RegistrationOutcome::Registered);

    let server = McpServer::with_tools(build_context(), registry);
    let responses = roundtrip(
        server,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "0"}
                }
            }),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        ],
    )
    .await;

    let list = responses
        .iter()
        .find(|r| r.get("id") == Some(&json!(2)))
        .expect("tools/list response");
    let tools = list["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    assert!(
        names.contains(&"hello_mcp"),
        "dynamic tool missing from list; got: {names:?}"
    );
}

#[tokio::test]
async fn dynamic_tool_dispatches_via_tools_call() {
    let mut registry = ToolRegistry::with_defaults();
    registry.register_dynamic(hello_mcp_tool());
    let server = McpServer::with_tools(build_context(), registry);

    let responses = roundtrip(
        server,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "0"}
                }
            }),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "hello_mcp",
                    "arguments": { "name": "Berkant" }
                }
            }),
        ],
    )
    .await;

    let call = responses
        .iter()
        .find(|r| r.get("id") == Some(&json!(2)))
        .expect("tools/call response");
    let content = call["result"]["content"].as_array().expect("content array");
    let text = content
        .first()
        .and_then(|c| c.get("text"))
        .and_then(Value::as_str)
        .expect("text content");
    assert_eq!(text, "Hello, Berkant");
    // `is_error: false` is `skip_serializing_if`'d out, so the field
    // is simply absent on success; check that it isn't present *and*
    // truthy rather than reading the literal value.
    assert!(call["result"]["isError"].as_bool() != Some(true));
}

#[test]
fn collision_with_builtin_is_rejected_at_registration() {
    let mut registry = ToolRegistry::with_defaults();
    let mut bad = hello_mcp_tool();
    bad.name = "run_query".to_owned();
    let outcome = registry.register_dynamic(bad);
    assert_eq!(outcome, RegistrationOutcome::CollisionBuiltin);
}
