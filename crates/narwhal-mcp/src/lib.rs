//! Model Context Protocol (MCP) server for narwhal.
//!
//! Exposes narwhal's configured database connections to AI agents (Claude
//! Desktop, Cursor, Continue, Aider, …) over the JSON-RPC 2.0 stdio
//! transport defined by the MCP spec.
//!
//! # Quick start
//!
//! ```no_run
//! use std::sync::Arc;
//! use narwhal_config::{ConfigPaths, ConnectionsFile, KeyringStore, CredentialStore};
//! use narwhal_mcp::{DriverRegistry, McpServer, ServerContext};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let paths = ConfigPaths::discover()?;
//! let connections = ConnectionsFile::load(&paths.connections_file())?;
//! let drivers = Arc::new(DriverRegistry::with_defaults());
//! let credentials: Arc<dyn CredentialStore> = Arc::new(KeyringStore::new());
//! let ctx = ServerContext::new(drivers, Arc::new(connections), credentials);
//! McpServer::new(ctx).serve_stdio().await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Wire-up for Claude Desktop
//!
//! ```jsonc
//! // ~/.config/Claude/claude_desktop_config.json
//! {
//!   "mcpServers": {
//!     "narwhal": {
//!       "command": "narwhal",
//!       "args": ["mcp"]
//!     }
//!   }
//! }
//! ```
//!
//! # Tool surface (v0)
//!
//! - [`tools::ListConnectionsTool`] — metadata only, no IO.
//! - [`tools::DescribeSchemaTool`] — opens a short-lived connection.
//!
//! `run_query` / `explain_query` land in v0.2 once we've validated the
//! read-only enforcement story.

#![forbid(unsafe_code)]

pub mod context;
pub mod error;
pub mod json_value;
pub mod protocol;
pub mod registry;
pub mod server;
pub mod tools;

pub use context::ServerContext;
pub use error::McpError;
pub use registry::DriverRegistry;
pub use server::McpServer;
