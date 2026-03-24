//! `list_connections` — return the named connections available in the
//! current `connections.toml`.
//!
//! Pure metadata: no credentials are loaded and no network IO is performed.
//! Agents call this first to discover what they can target before issuing
//! schema or query calls.

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};

use crate::context::ServerContext;
use crate::error::McpError;
use crate::tools::{Tool, ToolOutput};

pub struct ListConnectionsTool;

#[derive(Serialize)]
struct ConnectionView {
    name: String,
    driver: String,
    /// Best-effort summary so the agent can disambiguate two connections
    /// to the same driver — e.g. `127.0.0.1:5432/prod` vs `staging.db/prod`.
    target: String,
    /// `true` when the connection is gated behind an SSH tunnel. Agents
    /// can use this to set saner timeouts.
    ssh: bool,
}

#[async_trait]
impl Tool for ListConnectionsTool {
    fn name(&self) -> &'static str {
        "list_connections"
    }

    fn description(&self) -> &'static str {
        "List every named database connection narwhal knows about. \
         Returns connection name, driver, a target summary (host:port/db \
         or file path) and whether the connection is tunneled through SSH. \
         Call this first to discover what `describe_schema` and other tools \
         can target."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        })
    }

    async fn call(&self, ctx: &ServerContext, _arguments: Value) -> Result<ToolOutput, McpError> {
        // `visible_connections` already applies the workspace ACL when one
        // is attached. Without a workspace this returns every connection,
        // preserving the historical behaviour.
        let visible = ctx.visible_connections();
        let view: Vec<ConnectionView> = visible
            .iter()
            .map(|c| ConnectionView {
                name: c.name.clone(),
                driver: c.driver.clone(),
                target: summarise_target(c),
                ssh: c.params.ssh.is_some(),
            })
            .collect();

        // `unwrap_or_else` here is to fall back on a static string if
        // serialization fails — `serde_json::to_string_pretty` only fails
        // on values that contain non-string map keys, which our struct does
        // not, so the fallback is unreachable in practice.
        let text = serde_json::to_string_pretty(&json!({
            "connections": view,
            "count": view.len(),
        }))
        .unwrap_or_else(|_| "{\"connections\": [], \"count\": 0}".to_string());

        Ok(ToolOutput::ok(text))
    }
}

/// Build a short, human-readable description of the connection target.
///
/// File-local drivers (sqlite, duckdb) report the path; network drivers
/// report `host:port/database`. Missing fields fall back to placeholders so
/// the output never contains a literal `None`.
fn summarise_target(c: &narwhal_core::ConnectionConfig) -> String {
    if let Some(path) = c.params.path.as_deref() {
        return path.to_string();
    }
    let host = c.params.host.as_deref().unwrap_or("?");
    let port = c
        .params
        .port
        .map_or_else(|| "?".to_string(), |p| p.to_string());
    let db = c.params.database.as_deref().unwrap_or("?");
    format!("{host}:{port}/{db}")
}
