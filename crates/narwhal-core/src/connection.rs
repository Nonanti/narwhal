use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::schema::{QueryResult, Schema, Table, TableSchema};

/// Static connection metadata stored in the user's config file.
///
/// The password itself is **not** stored here — it lives in the OS keychain
/// (or, as a fallback, in an encrypted local file). `ConnectionConfig` only
/// references the credential by id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionConfig {
    /// Stable id, generated when the connection is created.
    pub id: uuid::Uuid,
    /// User-facing name, e.g. "prod-readonly".
    pub name: String,
    /// Driver identifier, e.g. "postgres", "sqlite".
    pub driver: String,
    pub params: ConnectionParams,
}

/// Driver-agnostic connection parameters.
///
/// Unused fields stay `None`; each driver decides what it needs.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConnectionParams {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub username: Option<String>,
    /// Path on disk (SQLite, etc.).
    pub path: Option<String>,
    /// Free-form options like `sslmode=require`.
    #[serde(default)]
    pub options: std::collections::BTreeMap<String, String>,
}

/// An open connection to a database.
///
/// Drivers implement this trait; the rest of narwhal interacts with
/// `Box<dyn Connection>` and does not know which database is behind it.
#[async_trait]
pub trait Connection: Send + Sync {
    /// Execute arbitrary SQL and return the result.
    async fn execute(&mut self, sql: &str) -> Result<QueryResult>;

    /// List logical schemas/namespaces.
    async fn list_schemas(&mut self) -> Result<Vec<Schema>>;

    /// List tables/views inside a schema.
    async fn list_tables(&mut self, schema: &str) -> Result<Vec<Table>>;

    /// Describe a single table (columns, types, PKs).
    async fn describe_table(&mut self, schema: &str, name: &str) -> Result<TableSchema>;

    /// Quick "is this still alive?" check.
    async fn ping(&mut self) -> Result<()>;

    /// Close the underlying connection. Consumes the trait object.
    async fn close(self: Box<Self>) -> Result<()>;
}
