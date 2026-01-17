//! JSON-RPC 2.0 + Model Context Protocol message types.
//!
//! We deliberately keep this minimal — narwhal's MCP server only implements
//! the `tools` capability surface for now. Resources, prompts, sampling and
//! roots are out of scope until we have a concrete use-case.
//!
//! MCP protocol version targeted: `2024-11-05` (the first stable wire
//! format; clients negotiate the version on `initialize`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
pub const JSONRPC_VERSION: &str = "2.0";

/// A JSON-RPC 2.0 request or notification.
///
/// Requests carry an `id`; notifications omit it. We keep both in one type
/// so the dispatch loop can deserialize once and branch on `id.is_some()`.
#[derive(Debug, Deserialize)]
pub struct Request {
    #[serde(rename = "jsonrpc")]
    pub _jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

impl Request {
    /// `true` when this message expects a response (i.e. it is a request,
    /// not a notification).
    pub const fn is_request(&self) -> bool {
        self.id.is_some()
    }
}

/// A JSON-RPC 2.0 response — either `result` xor `error`, never both.
#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub const fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            result: Some(result),
            error: None,
        }
    }

    pub const fn error(id: Value, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// JSON-RPC 2.0 error object.
///
/// Standard codes used by this server:
/// - `-32700` Parse error
/// - `-32600` Invalid Request
/// - `-32601` Method not found
/// - `-32602` Invalid params
/// - `-32603` Internal error
#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn parse_error(msg: impl Into<String>) -> Self {
        Self {
            code: -32700,
            message: msg.into(),
            data: None,
        }
    }

    pub fn invalid_request(msg: impl Into<String>) -> Self {
        Self {
            code: -32600,
            message: msg.into(),
            data: None,
        }
    }

    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {method}"),
            data: None,
        }
    }

    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: msg.into(),
            data: None,
        }
    }

    pub fn internal_error(msg: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: msg.into(),
            data: None,
        }
    }
}

// `initialize` exchange

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    /// Protocol version the client wants to speak. We currently only
    /// support `2024-11-05`; we echo the client's choice back if it matches
    /// and fall back to our supported version otherwise.
    pub protocol_version: String,
    #[serde(default)]
    pub capabilities: Value,
    #[serde(default)]
    pub client_info: Option<ClientInfo>,
}

#[derive(Debug, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: &'static str,
    pub capabilities: ServerCapabilities,
    pub server_info: ServerInfo,
    /// Optional free-form text — clients (Claude Desktop, Cursor) surface
    /// this in their UI when first wiring the server up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

#[derive(Debug, Serialize, Default)]
pub struct ServerCapabilities {
    /// We advertise `tools` only. The empty struct signals "tools are
    /// supported, but listChanged notifications are not".
    pub tools: ToolsCapability,
}

#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCapability {
    /// `false` — narwhal's tool registry is static at start-up, so we
    /// never emit `tools/list_changed` notifications.
    pub list_changed: bool,
}

#[derive(Debug, Serialize)]
pub struct ServerInfo {
    pub name: &'static str,
    pub version: &'static str,
}

// `tools/list` and `tools/call`

#[derive(Debug, Serialize)]
pub struct ToolsListResult {
    pub tools: Vec<ToolDescriptor>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema describing the `arguments` object accepted by
    /// `tools/call`. Clients use this to render forms and to validate
    /// LLM-produced calls before dispatching.
    pub input_schema: Value,
}

#[derive(Debug, Deserialize)]
pub struct ToolsCallParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Option<Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCallResult {
    pub content: Vec<Content>,
    /// `true` when the tool executed but reported an error (e.g. the
    /// underlying SQL failed). Distinguishes from JSON-RPC level errors,
    /// which are surfaced via the `error` envelope instead.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
}

/// MCP content block. Only the textual variant is used for now; image and
/// resource references will follow once we ship binary-friendly tools.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Text { text: String },
}

impl Content {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text { text: s.into() }
    }
}
