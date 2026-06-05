//! Tool registry and the [`Tool`] trait every executable command implements.
//!
//! Tools are kept stateless: they receive a borrowed handle to the shared
//! [`ServerContext`] on every call so the registry itself can stay
//! `Send + Sync` without `Mutex` ceremony.

use std::sync::Arc;

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
/// How many bytes of the original body to keep verbatim inside the
/// truncation envelope. Small enough to round-trip safely under the
/// overall response cap; large enough that the agent can usually
/// diagnose the underlying error.
const CAP_SNIPPET_BYTES: usize = 4 * 1024;

pub fn cap_response(body: String, tool: &str) -> (String, bool) {
    if body.len() <= MAX_RESPONSE_BYTES {
        return (body, false);
    }
    // MR-C3: keep a UTF-8-safe prefix of the original body so the
    // agent can read the start of the actual error / payload instead
    // of losing it completely behind a generic envelope.
    let snippet_end = floor_to_char_boundary(&body, CAP_SNIPPET_BYTES.min(body.len()));
    let snippet = &body[..snippet_end];
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
        "snippet": snippet,
    });
    (
        serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{\"truncated\":true}".into()),
        true,
    )
}

/// Find the largest char-boundary index `<= idx`. R3-N5: renamed
/// from `floor_char_boundary` to avoid colliding with the eventually-
/// stable `str::floor_char_boundary` API. Once stdlib's stabilises,
/// this helper can be deleted in favour of the method.
fn floor_to_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// A single MCP tool callable via `tools/call`.
///
/// `name()` doubles as the registry key and the on-the-wire identifier; it
/// must therefore be stable across releases.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Stable identifier the client passes to `tools/call`.
    ///
    /// The lifetime is tied to `&self` so dynamic tools can return a
    /// borrowed slice of their owned `String`; built-ins return
    /// `&'static str` constants without ceremony.
    fn name(&self) -> &str;

    /// Human-readable description shown in `tools/list`.
    fn description(&self) -> &str;

    /// JSON Schema for the `arguments` object accepted by this tool.
    fn input_schema(&self) -> Value;

    /// Descriptor name as a `Cow<'static, str>`. MR-N3: built-ins
    /// override this with `Cow::Borrowed("name")` so the descriptor
    /// round-trips without an allocation; the default impl falls
    /// back to an owned clone, which dynamic tools rely on.
    fn descriptor_name(&self) -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Owned(self.name().to_owned())
    }

    /// Same shape as [`Self::descriptor_name`] for the description.
    fn descriptor_description(&self) -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Owned(self.description().to_owned())
    }

    /// Convenience for assembling the on-the-wire descriptor.
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: self.descriptor_name(),
            description: self.descriptor_description(),
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
///
/// T2-T5-C: in addition to the built-ins, the registry now accepts
/// *dynamic* tools registered at startup via [`Self::register_dynamic`].
/// These are the host-side hook that plugins use to expose their own
/// MCP tools — the v2.0 surface is generic over any closure-shaped
/// handler; the v2.1 follow-up wires the WASM-side `mcp` WIT interface
/// onto this same registration path. Name-collision policy:
/// **built-ins always win**, and on a dynamic-vs-dynamic clash the
/// first registration wins (`RegistrationOutcome::CollisionDynamic`).
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
    /// Dynamic tools sourced from plugins. Stored separately so the
    /// built-in slice keeps its compile-time order and the dynamic
    /// slice keeps its registration order.
    dynamic: Vec<Arc<DynamicTool>>,
}

/// One dynamically-registered tool. Owns its name, description, and
/// input-schema strings so the registry doesn't have to hold borrowed
/// data with `'static` lifetimes; the cost is one allocation per tool
/// at registration time.
pub struct DynamicTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    /// Identifier of the plugin that registered the tool, surfaced
    /// only in collision diagnostics.
    pub source: String,
    /// Async handler executed by `tools/call`. Returning
    /// `ToolOutput::err(…)` produces a tool-level error;
    /// returning a real [`McpError`] surfaces as a JSON-RPC error.
    pub handler: DynamicHandler,
}

/// Boxed handler executed when a dynamic tool is dispatched. The
/// arguments arrive already parsed; the host has not validated them
/// against `input_schema` (validation lives one level up in v2.0—
/// dispatch will pass through verbatim, the v2.1 wiring task plugs in
/// the JSON-schema check).
pub type DynamicHandler = Arc<
    dyn for<'a> Fn(
            &'a ServerContext,
            Value,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<ToolOutput, McpError>> + Send + 'a>,
        > + Send
        + Sync,
>;

/// Outcome of [`ToolRegistry::register_dynamic`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationOutcome {
    /// Tool was registered and is now reachable via `tools/list` /
    /// `tools/call`.
    Registered,
    /// A built-in tool of the same name already exists; the dynamic
    /// registration was rejected.
    CollisionBuiltin,
    /// Another dynamic tool of the same name was already registered
    /// (by `existing_source`). The new registration was rejected.
    CollisionDynamic { existing_source: String },
}

#[async_trait]
impl Tool for DynamicTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        self.input_schema.clone()
    }

    async fn call(&self, ctx: &ServerContext, arguments: Value) -> Result<ToolOutput, McpError> {
        (self.handler)(ctx, arguments).await
    }
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
            dynamic: Vec::new(),
        }
    }

    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        let mut out: Vec<ToolDescriptor> = self.tools.iter().map(|t| t.descriptor()).collect();
        out.extend(self.dynamic.iter().map(|d| d.descriptor()));
        out
    }

    pub fn find(&self, name: &str) -> Option<&dyn Tool> {
        if let Some(builtin) = self
            .tools
            .iter()
            .find(|t| t.name() == name)
            .map(std::convert::AsRef::as_ref)
        {
            return Some(builtin);
        }
        self.dynamic
            .iter()
            .find(|d| d.name == name)
            .map(|arc| arc.as_ref() as &dyn Tool)
    }

    /// T2-T5-C: register a dynamic tool sourced from a plugin.
    ///
    /// Returns a [`RegistrationOutcome`] so the caller can surface a
    /// collision warning (typical pattern: `tracing::warn!`).
    pub fn register_dynamic(&mut self, tool: DynamicTool) -> RegistrationOutcome {
        // Built-in collision: rejected unconditionally.
        if self.tools.iter().any(|t| t.name() == tool.name) {
            return RegistrationOutcome::CollisionBuiltin;
        }
        // Dynamic-vs-dynamic: first registration wins.
        if let Some(existing) = self.dynamic.iter().find(|d| d.name == tool.name) {
            return RegistrationOutcome::CollisionDynamic {
                existing_source: existing.source.clone(),
            };
        }
        self.dynamic.push(Arc::new(tool));
        RegistrationOutcome::Registered
    }

    /// Read-only access to the dynamic-tool list. Useful for
    /// diagnostics and tests.
    pub fn dynamic_tools(&self) -> &[Arc<DynamicTool>] {
        &self.dynamic
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn make_tool(name: &str, source: &str, counter: Arc<AtomicUsize>) -> DynamicTool {
        DynamicTool {
            name: name.to_owned(),
            description: format!("dynamic tool {name}"),
            input_schema: serde_json::json!({ "type": "object" }),
            source: source.to_owned(),
            handler: Arc::new(move |_ctx, args| {
                let counter = counter.clone();
                Box::pin(async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(ToolOutput::ok(args.to_string()))
                })
            }),
        }
    }

    #[test]
    fn dynamic_tool_registers_and_appears_in_descriptors() {
        let mut reg = ToolRegistry::with_defaults();
        let counter = Arc::new(AtomicUsize::new(0));
        let outcome = reg.register_dynamic(make_tool("my_tool", "plugin-a", counter));
        assert_eq!(outcome, RegistrationOutcome::Registered);
        let names: Vec<String> = reg
            .descriptors()
            .into_iter()
            .map(|d| d.name.into_owned())
            .collect();
        assert!(names.iter().any(|n| n == "my_tool"));
    }

    #[test]
    fn collision_with_builtin_is_rejected() {
        let mut reg = ToolRegistry::with_defaults();
        let counter = Arc::new(AtomicUsize::new(0));
        let outcome = reg.register_dynamic(make_tool("run_query", "evil-plugin", counter));
        assert_eq!(outcome, RegistrationOutcome::CollisionBuiltin);
        assert!(reg.dynamic_tools().is_empty());
    }

    #[test]
    fn collision_between_dynamic_tools_first_wins() {
        let mut reg = ToolRegistry::with_defaults();
        let counter = Arc::new(AtomicUsize::new(0));
        let first = reg.register_dynamic(make_tool("shared", "plugin-a", counter.clone()));
        let second = reg.register_dynamic(make_tool("shared", "plugin-b", counter));
        assert_eq!(first, RegistrationOutcome::Registered);
        assert_eq!(
            second,
            RegistrationOutcome::CollisionDynamic {
                existing_source: "plugin-a".to_owned()
            }
        );
        assert_eq!(reg.dynamic_tools().len(), 1);
    }

    #[test]
    fn find_returns_dynamic_when_no_builtin_matches() {
        let mut reg = ToolRegistry::with_defaults();
        let counter = Arc::new(AtomicUsize::new(0));
        reg.register_dynamic(make_tool("new_tool", "plugin-a", counter));
        assert!(reg.find("new_tool").is_some());
    }

    #[test]
    fn find_prefers_builtin_on_name_match() {
        // Even if we managed to slip a dynamic in (we can't through
        // register_dynamic), find should walk built-ins first. Verify
        // by checking that `run_query` resolves to the built-in.
        let reg = ToolRegistry::with_defaults();
        let tool = reg.find("run_query").expect("builtin");
        assert_eq!(tool.name(), "run_query");
    }

    /// Build a minimal [`ServerContext`] for unit tests that only need
    /// a value to pass through. Dynamic tools that touch the database
    /// are integration-tested elsewhere; this helper exists so the
    /// dispatch-path test below can call the registered handler
    /// without a live driver.
    fn test_context() -> ServerContext {
        use crate::registry::DriverRegistry;
        use narwhal_config::{ConnectionsFile, CredentialError, CredentialStore};
        use secrecy::SecretString;
        use uuid::Uuid;
        struct NoopStore;
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
        ServerContext::new(
            Arc::new(DriverRegistry::with_defaults()),
            Arc::new(ConnectionsFile::default()),
            Arc::new(NoopStore),
        )
    }

    #[tokio::test]
    async fn dynamic_tool_handler_executes_via_call() {
        let mut reg = ToolRegistry::with_defaults();
        let counter = Arc::new(AtomicUsize::new(0));
        reg.register_dynamic(make_tool("echo", "plugin-a", counter.clone()));
        let ctx = test_context();
        let tool = reg.find("echo").expect("registered");
        let out = tool
            .call(&ctx, serde_json::json!({"hello": "world"}))
            .await
            .expect("call");
        assert!(!out.is_error);
        assert!(out.text.contains("hello"));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
