//! `describe_table` — return the full `TableSchema` (columns, indexes,
//! foreign keys, unique constraints) of a single table, plus the engine
//! DDL when the driver can produce it.
//!
//! Complements `describe_schema`, which is a flat tree of names. Agents
//! typically call `describe_schema` first to discover targets and then
//! `describe_table` for the ones they actually care about — keeps the
//! per-call payload small even on databases with thousands of tables.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::context::ServerContext;
use crate::error::McpError;
use crate::tools::{Tool, ToolOutput};

pub struct DescribeTableTool;

impl DescribeTableTool {
    const NAME: &'static str = "describe_table";
    const DESCRIPTION: &'static str = "Return the full structure of a single table or view: columns \
         (with type, nullability, primary-key flag, default), indexes, \
         foreign keys, unique constraints, and — when the driver \
         supports it — the engine-native CREATE statement. Use \
         `describe_schema` first to discover schema/table names.";
}

#[async_trait]
impl Tool for DescribeTableTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        Self::DESCRIPTION
    }

    fn descriptor_name(&self) -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(Self::NAME)
    }

    fn descriptor_description(&self) -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(Self::DESCRIPTION)
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "connection": {
                    "type": "string",
                    "description": "Connection name as returned by `list_connections`."
                },
                "schema": {
                    "type": "string",
                    "description": "Logical schema/namespace. Optional for engines with only one (sqlite → `main`, duckdb → `main`); required for postgres/mysql/clickhouse."
                },
                "table": {
                    "type": "string",
                    "description": "Table or view name."
                }
            },
            "required": ["connection", "table"],
            "additionalProperties": false,
        })
    }

    async fn call(&self, ctx: &ServerContext, arguments: Value) -> Result<ToolOutput, McpError> {
        let conn_name = arguments
            .get("connection")
            .and_then(Value::as_str)
            .ok_or_else(|| McpError::InvalidParams("missing `connection` argument".into()))?;
        let table = arguments
            .get("table")
            .and_then(Value::as_str)
            .ok_or_else(|| McpError::InvalidParams("missing `table` argument".into()))?;
        // `schema` is optional; we default to the engine's default schema
        // when the agent omits it. `describe_table` on every driver
        // tolerates an empty / default schema string the same way the
        // sidebar does.
        let schema = arguments
            .get("schema")
            .and_then(Value::as_str)
            .unwrap_or("");

        let mut conn = match ctx.open_connection(conn_name).await {
            Ok(c) => c,
            Err(McpError::UnknownConnection(_)) => {
                return Ok(ToolOutput::err(format!(
                    "unknown connection: {conn_name}. Call `list_connections` to see valid names."
                )));
            }
            Err(other) => return Err(other),
        };

        let describe_result = conn.describe_table(schema, table).await;
        // Best-effort DDL — drivers that don't implement `fetch_ddl`
        // return `Error::Unsupported`, which we silently elide from the
        // payload instead of failing the whole call.
        let ddl_result = match describe_result {
            Ok(_) => conn.fetch_ddl(schema, table).await.ok(),
            Err(_) => None,
        };
        let _ = conn.close().await;

        let schema_data = match describe_result {
            Ok(s) => s,
            Err(error) => {
                return Ok(ToolOutput::err(format!(
                    "describe_table failed on `{conn_name}.{schema}.{table}`: {error}"
                )));
            }
        };

        // `TableSchema` already derives `Serialize` — its on-the-wire shape
        // matches what an agent expects (snake_case-ish field names from
        // serde defaults). We embed it directly under the top-level
        // envelope.
        let mut payload =
            serde_json::to_value(&schema_data).map_err(|e| McpError::Internal(e.to_string()))?;
        if let Value::Object(map) = &mut payload {
            map.insert("connection".into(), json!(conn_name));
            if let Some(ddl) = ddl_result {
                map.insert("ddl".into(), Value::String(ddl));
            }
        }

        // H2: clamp the body — raw DDL plus thousands of columns on a
        // wide table can easily exceed the agent's response budget.
        let body = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string());
        Ok(ToolOutput::ok(body))
    }
}
