//! Tool registry and the [`Tool`] trait every executable command implements.
//!
//! Tools are kept stateless: they receive a borrowed handle to the shared
//! [`ServerContext`] on every call so the registry itself can stay
//! `Send + Sync` without `Mutex` ceremony.

use async_trait::async_trait;
use serde_json::Value;

use crate::context::ServerContext;
use crate::error::McpError;
use crate::protocol::ToolDescriptor;

mod describe_schema;
mod describe_table;
mod explain_query;
mod get_diagram;
mod list_connections;
mod run_query;

pub use describe_schema::DescribeSchemaTool;
pub use describe_table::DescribeTableTool;
pub use explain_query::ExplainQueryTool;
pub use get_diagram::GetDiagramTool;
pub use list_connections::ListConnectionsTool;
pub use run_query::RunQueryTool;

/// Hard ceiling on the serialised JSON body that a single tool call
/// may return.
///
/// MCP responses travel inline to the agent's host (Claude Desktop,
/// Cursor, Aider) which typically caps a single tool reply at ~512 KiB
/// before truncating or refusing. A `describe_schema` against a
/// 50k-table catalog, or an `EXPLAIN (VERBOSE, BUFFERS)` on a complex
/// query, can easily exceed that. Tools call [`cap_response`] on their
/// final serialised body to enforce the cap with a uniform
/// `truncated: true` marker so the agent knows to drill down.  (Bug H2 fix.)
pub const MAX_RESPONSE_BYTES: usize = 512 * 1024;

/// Truncate a serialised JSON body so it stays under
/// [`MAX_RESPONSE_BYTES`].
///
/// Returns `(body, truncated_flag)`. When the input is already under
/// the cap the body is returned untouched and the flag is `false`.
/// When it exceeds the cap we cannot keep the JSON structurally valid
/// (truncating mid-array breaks the parser), so we replace it with a
/// minimal JSON envelope that surfaces the truncation reason. Agents
/// then know to re-issue a narrower query.
pub fn cap_response(body: String, tool: &str) -> (String, bool) {
    if body.len() <= MAX_RESPONSE_BYTES {
        return (body, false);
    }
    let envelope = serde_json::json!({
        "truncated": true,
        "tool": tool,
        "reason": format!(
            "response body ({} bytes) exceeded MAX_RESPONSE_BYTES ({} bytes); \
             narrow the query (e.g. specific schema/table, smaller LIMIT) and retry",
            body.len(),
            MAX_RESPONSE_BYTES
        ),
        "original_byte_length": body.len(),
        "max_byte_length": MAX_RESPONSE_BYTES,
    });
    (
        serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{\"truncated\":true}".into()),
        true,
    )
}

/// A single MCP tool callable via `tools/call`.
///
/// `name()` doubles as the registry key and the on-the-wire identifier; it
/// must therefore be stable across releases.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Stable identifier the client passes to `tools/call`.
    fn name(&self) -> &'static str;

    /// Human-readable description shown in `tools/list`.
    fn description(&self) -> &'static str;

    /// JSON Schema for the `arguments` object accepted by this tool.
    fn input_schema(&self) -> Value;

    /// Convenience for assembling the on-the-wire descriptor.
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: self.name(),
            description: self.description(),
            input_schema: self.input_schema(),
        }
    }

    /// Execute the tool. Returning `Ok` with `is_error = true` reports a
    /// *tool-level* failure (e.g. SQL error); returning `Err` triggers a
    /// JSON-RPC `error` response — usually only for malformed arguments
    /// or unrecoverable internal errors.
    async fn call(&self, ctx: &ServerContext, arguments: Value) -> Result<ToolOutput, McpError>;
}

/// Output emitted by a tool. The dispatch layer wraps this into a
/// [`crate::protocol::ToolsCallResult`].
pub struct ToolOutput {
    pub text: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    pub fn err(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }
}

/// Static registry of every tool the server exposes.
///
/// We avoid a `HashMap` here because the set is tiny and the linear scan is
/// faster (and gives us deterministic `tools/list` ordering for free).
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    /// Registry preloaded with every tool bundled with narwhal-mcp.
    pub fn with_defaults() -> Self {
        Self {
            tools: vec![
                Box::new(ListConnectionsTool),
                Box::new(DescribeSchemaTool),
                Box::new(DescribeTableTool),
                Box::new(RunQueryTool),
                Box::new(ExplainQueryTool),
                Box::new(GetDiagramTool),
            ],
        }
    }

    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.tools.iter().map(|t| t.descriptor()).collect()
    }

    pub fn find(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(std::convert::AsRef::as_ref)
    }
}
