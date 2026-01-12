//! Error types for the MCP server.
//!
//! Two layers:
//! - [`McpError`] is the internal error type tools and the dispatch loop
//!   produce. It maps to a JSON-RPC `error` envelope at the protocol edge.
//! - Driver / connection errors are kept as their original `narwhal_core::Error`
//!   wrapped in [`McpError::Connection`] so we never lose the underlying
//!   cause when surfacing to the agent.

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum McpError {
    /// Malformed `params` block — surfaces as JSON-RPC `-32602`.
    #[error("invalid params: {0}")]
    InvalidParams(String),

    /// The named connection is not present in `connections.toml`.
    /// Tools convert this into a `tools/call` result with `isError: true`
    /// rather than a protocol-level error.
    #[error("unknown connection: {0}")]
    UnknownConnection(String),

    /// Wrapped driver / wire-level failure.
    #[error("connection error: {0}")]
    Connection(#[from] narwhal_core::Error),

    /// Wrapped credential-store failure.
    #[error("credential error: {0}")]
    Credential(#[from] narwhal_config::CredentialError),

    /// stdio / serialization / unexpected runtime errors.
    #[error("internal error: {0}")]
    Internal(String),
}
