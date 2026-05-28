//! Stdio-based MCP server loop.
//!
//! Reads JSON-RPC messages one per line from stdin, dispatches to either
//! the `initialize` handshake, `tools/list`, `tools/call`, or a small set
//! of notifications, and writes responses one per line to stdout.
//!
//! Logging goes to stderr (or the tracing layer the host wired up) — never
//! to stdout, since stdout is the transport channel.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::context::ServerContext;
use crate::error::McpError;
use crate::protocol::{
    Content, InitializeParams, InitializeResult, MCP_PROTOCOL_VERSION, Request, Response, RpcError,
    ServerCapabilities, ServerInfo, ToolsCallParams, ToolsCallResult, ToolsListResult,
};
use crate::tools::ToolRegistry;

const SERVER_NAME: &str = "narwhal";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Hard cap on the size of a single inbound JSON-RPC frame.
///
/// 1 MiB is comfortable for any legitimate MCP call (`tools/list` of
/// every tool with all schemas inlined sits well under 50 KiB). The cap
/// is enforced at the *read* layer (`AsyncReadExt::take`), not after the
/// line is already in memory, so a client streaming bytes without a
/// newline can never allocate more than this.
const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Configured but not-yet-running server. Build via [`McpServer::new`] and
/// call [`McpServer::serve_stdio`] to take over stdin/stdout.
pub struct McpServer {
    ctx: ServerContext,
    tools: Arc<ToolRegistry>,
}

impl McpServer {
    pub fn new(ctx: ServerContext) -> Self {
        Self {
            ctx,
            tools: Arc::new(ToolRegistry::with_defaults()),
        }
    }

    /// construct the server with a *pre-populated* tool
    /// registry. The host caller (typically the `narwhal mcp` binary)
    /// builds the registry first, registers dynamic plugin tools via
    /// [`ToolRegistry::register_dynamic`], then hands the result to
    /// the server. This is the v2.0 wiring path; the v2.1 follow-up
    /// will move the registration call inside narwhal-plugin so the
    /// WASM-side `mcp::register-tools` export is auto-invoked.
    pub fn with_tools(ctx: ServerContext, tools: ToolRegistry) -> Self {
        Self {
            ctx,
            tools: Arc::new(tools),
        }
    }

    /// Run the server on the current process's stdin/stdout pair.
    ///
    /// Returns `Ok(())` when stdin closes cleanly (EOF). Returns `Err` only
    /// on a fatal IO error — protocol-level errors are surfaced through the
    /// JSON-RPC response stream and never bubble out here.
    pub async fn serve_stdio(self) -> std::io::Result<()> {
        self.serve(tokio::io::stdin(), tokio::io::stdout()).await
    }

    /// Run the server against an arbitrary reader/writer pair.
    ///
    /// Splitting `serve_stdio` from a transport-generic `serve` lets the
    /// integration tests pipe JSON-RPC traffic through `tokio::io::duplex`
    /// without spawning a subprocess.
    ///
    /// **Per-frame cap**: the reader is wrapped with `take(MAX_LINE_BYTES + 1)`
    /// so a client sending an unbounded stream without a newline can
    /// never allocate more than the cap. The previous implementation
    /// only checked length *after* a full line landed in memory, which
    /// let a hostile/buggy client OOM the process before the check fired.
    pub async fn serve<R, W>(self, reader: R, mut writer: W) -> std::io::Result<()>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        // +1 so we can DISTINGUISH "exactly at the cap" from "overflow":
        // if `next_line` ever returns a string of length MAX_LINE_BYTES+1
        // we know the cap was reached and the rest was truncated.
        let mut lines = BufReader::new(reader.take((MAX_LINE_BYTES as u64) + 1)).lines();

        tracing::info!(
            server = SERVER_NAME,
            version = SERVER_VERSION,
            protocol = MCP_PROTOCOL_VERSION,
            tools = self.tools.descriptors().len(),
            "MCP server ready"
        );

        while let Some(line) = lines.next_line().await? {
            // Defence in depth: if the cap was reached the take-wrapper
            // will swallow the trailing bytes silently and the parser
            // will choke on a truncated JSON frame. Detect and reject
            // BEFORE attempting to parse so the agent sees a clean error.
            if line.len() > MAX_LINE_BYTES {
                tracing::warn!(
                    len = line.len(),
                    max = MAX_LINE_BYTES,
                    "rejecting oversized JSON-RPC message (capped at read time)"
                );
                let response = Response::error(
                    Value::Null,
                    RpcError::invalid_request(format!(
                        "message exceeded {MAX_LINE_BYTES}-byte cap; connection aborted"
                    )),
                );
                let mut payload = serde_json::to_vec(&response).unwrap_or_default();
                payload.push(b'\n');
                writer.write_all(&payload).await?;
                writer.flush().await?;
                // Bail — once the stream lost framing we cannot trust
                // the next "line" boundary either. Hostile-client policy.
                tracing::warn!("closing transport after oversized frame");
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(response) = self.handle_line(trimmed).await {
                let mut payload = serde_json::to_vec(&response)
                    .unwrap_or_else(|_| br#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"serialization failed"},"id":null}"#.to_vec());
                payload.push(b'\n');
                writer.write_all(&payload).await?;
                writer.flush().await?;
            }
        }

        tracing::info!("reader closed — MCP server shutting down");
        Ok(())
    }

    /// Parse and dispatch one line. Returns `None` when the message is a
    /// notification (no response expected) or unparseable in a way that
    /// JSON-RPC says should be silently dropped.
    ///
    /// The hard size cap is enforced upstream in [`Self::serve`] via
    /// `AsyncReadExt::take` so a hostile client cannot OOM us before the
    /// post-read check ever runs.
    async fn handle_line(&self, line: &str) -> Option<Response> {
        let request: Request = match serde_json::from_str(line) {
            Ok(req) => req,
            Err(error) => {
                tracing::warn!(error = %error, line = %line, "failed to parse JSON-RPC message");
                // Parse errors *do* deserve a response per JSON-RPC, but
                // without an `id` we cannot address it. Use `null`.
                return Some(Response::error(
                    Value::Null,
                    RpcError::parse_error(format!("invalid JSON: {error}")),
                ));
            }
        };

        // Validate the jsonrpc version field before dispatching.
        if let Err(reason) = request.validate_jsonrpc() {
            tracing::warn!(%reason, "jsonrpc version mismatch");
            let id = request.id.clone().unwrap_or(Value::Null);
            return Some(Response::error(id, RpcError::invalid_request(reason)));
        }

        let is_request = request.is_request();
        let id = request.id.clone().unwrap_or(Value::Null);
        let result = self.dispatch(request).await;

        if !is_request {
            // Notification — drop the response per JSON-RPC spec, even on
            // dispatch errors. We've already logged anything interesting.
            if let Err(error) = result {
                tracing::warn!(error = %error, "notification handler reported error");
            }
            return None;
        }

        Some(match result {
            Ok(Some(value)) => Response::success(id, value),
            Ok(None) => Response::success(id, Value::Null),
            Err(error) => Response::error(id, rpc_error_from(&error)),
        })
    }

    /// Route by method name. Returns `Ok(None)` for notifications and
    /// methods that legitimately have no result payload.
    async fn dispatch(&self, request: Request) -> Result<Option<Value>, McpError> {
        match request.method.as_str() {
            "initialize" => {
                let params: InitializeParams = parse_params(request.params)?;
                Ok(Some(self.handle_initialize(params)?))
            }
            "notifications/initialized" | "initialized" => {
                // Notification with no body — client signalling end of
                // handshake. Nothing to do.
                Ok(None)
            }
            "ping" => Ok(Some(json!({}))),
            "tools/list" => Ok(Some(self.handle_tools_list()?)),
            "tools/call" => {
                let params: ToolsCallParams = parse_params(request.params)?;
                Ok(Some(self.handle_tools_call(params).await?))
            }
            "shutdown" => Ok(Some(Value::Null)),
            other => Err(McpError::Internal(format!("method not found: {other}"))),
        }
    }

    fn handle_initialize(&self, _params: InitializeParams) -> Result<Value, McpError> {
        let result = InitializeResult {
            protocol_version: MCP_PROTOCOL_VERSION,
            capabilities: ServerCapabilities::default(),
            server_info: ServerInfo {
                name: SERVER_NAME,
                version: SERVER_VERSION,
            },
            instructions: Some(
                "narwhal MCP server. Call `list_connections` first to see \
                 the named databases this narwhal install has configured, \
                 then `describe_schema` to introspect one."
                    .to_string(),
            ),
        };
        serde_json::to_value(result).map_err(|e| McpError::Internal(e.to_string()))
    }

    fn handle_tools_list(&self) -> Result<Value, McpError> {
        let result = ToolsListResult {
            tools: self.tools.descriptors(),
        };
        serde_json::to_value(result).map_err(|e| McpError::Internal(e.to_string()))
    }

    async fn handle_tools_call(&self, params: ToolsCallParams) -> Result<Value, McpError> {
        let tool = self
            .tools
            .find(&params.name)
            .ok_or_else(|| McpError::InvalidParams(format!("unknown tool: {}", params.name)))?;
        let arguments = params.arguments.unwrap_or(Value::Null);
        let output = tool.call(&self.ctx, arguments).await?;

        // Enforce the response cap centrally so dynamic
        // plugin-defined tools cannot blow past
        // the MCP host size budget. Built-in tools no longer need to
        // call `cap_response` themselves. The original `is_error`
        // flag is preserved across truncation — a truncated error
        // body is still an error.
        let (capped_text, _truncated) = crate::tools::cap_response(output.text, &params.name);

        let result = ToolsCallResult {
            content: vec![Content::text(capped_text)],
            is_error: output.is_error,
        };
        serde_json::to_value(result).map_err(|e| McpError::Internal(e.to_string()))
    }
}

/// Deserialize the params object into the tool-specific type, mapping the
/// failure into a JSON-RPC `invalid params` error.
fn parse_params<T: serde::de::DeserializeOwned>(params: Option<Value>) -> Result<T, McpError> {
    let value = params.unwrap_or(Value::Null);
    serde_json::from_value(value).map_err(|e| McpError::InvalidParams(e.to_string()))
}

/// Map our internal error variants onto the JSON-RPC error codes. The
/// mapping is intentionally narrow — anything we cannot classify becomes
/// an `internal error (-32603)` so the agent always gets *something*.
fn rpc_error_from(error: &McpError) -> RpcError {
    match error {
        McpError::InvalidParams(msg) => RpcError::invalid_params(msg.clone()),
        McpError::UnknownConnection(name) => {
            // Bubble up as invalid params — the agent picked a connection
            // that does not exist, which is its fault, not ours.
            RpcError::invalid_params(format!("unknown connection: {name}"))
        }
        McpError::Internal(msg) if msg.starts_with("method not found") => {
            // Re-classify as the proper JSON-RPC code; `dispatch` packs the
            // method name into the message so the user sees it.
            RpcError::method_not_found(msg.strip_prefix("method not found: ").unwrap_or(msg))
        }
        McpError::Internal(msg) => RpcError::internal_error(msg.clone()),
        McpError::Connection(e) => RpcError::internal_error(e.to_string()),
        McpError::Credential(e) => RpcError::internal_error(e.to_string()),
    }
}
