//! T2-T3-C: embedded LSP client for narwhal.
//!
//! Speaks JSON-RPC 2.0 over a generic [`Transport`] (typically the
//! stdin/stdout of an `sqls` / `sqlls` child process). The client owns
//! the request/response router and exposes typed helpers for the
//! common LSP methods used by an editor: `initialize`, `initialized`,
//! `textDocument/didOpen`, `textDocument/didChange`,
//! `textDocument/completion`, `textDocument/hover`,
//! `textDocument/definition`, and `shutdown` / `exit`.
//!
//! v2.0 scope: this crate ships the protocol primitives and proves
//! the round-trip with an in-memory transport. Wiring into the
//! editor pane (`AppCore::editor_dispatch`) is intentionally deferred
//! to v2.1 — the brief flags a 2–3 week effort and the
//! editor-integration half (popup widget plumbing, debounce,
//! cancellation routing) is the larger piece. The crate's API is
//! ready for that follow-up: see [`ClientHandle::completion`] and
//! [`ClientHandle::hover`].

#![forbid(unsafe_code)]

pub mod client;
pub mod transport;

pub use client::{Capabilities, Client, ClientHandle, LspError, ServerSpec};
pub use transport::{MemoryTransport, Transport};

/// Re-export of the LSP type aliases consumers will need so a downstream
/// crate doesn't have to pin `lsp-types` themselves.
pub use lsp_types;

/// JSON-RPC 2.0 frame envelope shared by every message.
pub mod jsonrpc {
    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    /// Either an `i64` or a `String` request id, per JSON-RPC 2.0.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(untagged)]
    pub enum Id {
        Number(i64),
        String(String),
    }

    /// Outgoing request (client → server).
    #[derive(Debug, Clone, Serialize)]
    pub struct Request<'a> {
        pub jsonrpc: &'static str,
        pub id: Id,
        pub method: &'a str,
        pub params: Value,
    }

    /// Outgoing notification (client → server, no response expected).
    #[derive(Debug, Clone, Serialize)]
    pub struct Notification<'a> {
        pub jsonrpc: &'static str,
        pub method: &'a str,
        pub params: Value,
    }

    /// Incoming message from the server. The router dispatches on the
    /// presence of `id` and `method` fields to decide between a
    /// response, a request, and a notification.
    #[derive(Debug, Clone, Deserialize)]
    pub struct Incoming {
        #[allow(dead_code)]
        pub jsonrpc: Option<String>,
        pub id: Option<Id>,
        pub method: Option<String>,
        pub params: Option<Value>,
        pub result: Option<Value>,
        pub error: Option<RpcError>,
    }

    /// JSON-RPC error object returned by the server.
    #[derive(Debug, Clone, Deserialize, Serialize)]
    pub struct RpcError {
        pub code: i32,
        pub message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub data: Option<Value>,
    }

    impl std::fmt::Display for RpcError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}: {}", self.code, self.message)
        }
    }

    impl std::error::Error for RpcError {}
}
