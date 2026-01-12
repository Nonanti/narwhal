//! `describe_schema` — return the schema tree of a named connection.
//!
//! Opens the connection lazily (with credential resolution + SSH tunnel as
//! needed), calls [`Connection::list_all_tables`], then closes. We do not
//! cache yet — the schema cache lives in the TUI's `AppCore`; sharing it
//! with the MCP path is a future refactor.

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};

use crate::context::ServerContext;
use crate::error::McpError;
use crate::tools::{Tool, ToolOutput};

pub struct DescribeSchemaTool;

#[derive(Serialize)]
struct SchemaView {
    name: String,
    tables: Vec<TableView>,
}

#[derive(Serialize)]
struct TableView {
    name: String,
    kind: &'static str,
}

#[async_trait]
impl Tool for DescribeSchemaTool {
    fn name(&self) -> &'static str {
        "describe_schema"
    }

    fn description(&self) -> &'static str {
        "Return the schema tree (schemas and their tables/views) of a \
         named connection. Use `list_connections` first to discover valid \
         names. The call opens a short-lived connection — no persistent \
         session is kept open between MCP calls."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "connection": {
                    "type": "string",
                    "description": "Connection name as returned by `list_connections`."
                }
            },
            "required": ["connection"],
            "additionalProperties": false,
        })
    }

    async fn call(&self, ctx: &ServerContext, arguments: Value) -> Result<ToolOutput, McpError> {
        let conn_name = arguments
            .get("connection")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing `connection` argument".into()))?;

        // Returning Ok(err) — the agent should treat "unknown connection"
        // as a recoverable user error, not a transport-level failure.
        let mut conn = match ctx.open_connection(conn_name).await {
            Ok(conn) => conn,
            Err(McpError::UnknownConnection(_)) => {
                return Ok(ToolOutput::err(format!(
                    "unknown connection: {conn_name}. Call `list_connections` to see valid names."
                )));
            }
            Err(other) => return Err(other),
        };

        let result = conn.list_all_tables().await;
        // Best-effort close. We intentionally swallow the close error
        // because the data we already fetched is still valid output.
        let _ = conn.close().await;

        let tree = match result {
            Ok(tree) => tree,
            Err(error) => {
                return Ok(ToolOutput::err(format!(
                    "schema introspection failed on `{conn_name}`: {error}"
                )));
            }
        };

        let view: Vec<SchemaView> = tree
            .into_iter()
            .map(|(schema, tables)| SchemaView {
                name: schema.name,
                tables: tables
                    .into_iter()
                    .map(|t| TableView {
                        name: t.name,
                        kind: table_kind_str(t.kind),
                    })
                    .collect(),
            })
            .collect();

        let text = serde_json::to_string_pretty(&json!({
            "connection": conn_name,
            "schemas": view,
        }))
        .unwrap_or_else(|_| "{}".to_string());

        Ok(ToolOutput::ok(text))
    }
}

fn table_kind_str(kind: narwhal_core::TableKind) -> &'static str {
    match kind {
        narwhal_core::TableKind::Table => "table",
        narwhal_core::TableKind::View => "view",
        // M14 added `#[non_exhaustive]`; future variants fall back to a
        // safe placeholder so the JSON stays valid.
        _ => "other",
    }
}
