use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::cancel::CancelHandle;
use crate::capabilities::Capabilities;
use crate::error::Result;
use crate::schema::{QueryResult, Schema, Table, TableSchema};
use crate::stream::RowStream;
use crate::value::Value;

/// Static metadata describing how to reach a database.
///
/// The credential itself is not stored here; it is retrieved separately from
/// the configured credential store and passed to
/// [`crate::DatabaseDriver::connect`] at runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionConfig {
    pub id: uuid::Uuid,
    pub name: String,
    pub driver: String,
    pub params: ConnectionParams,
}

/// Driver-agnostic connection parameters.
///
/// Each driver decides which fields are required; unused fields remain
/// `None`. Engine-specific tuning is expressed through [`Self::options`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectionParams {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub username: Option<String>,
    pub path: Option<String>,
    #[serde(default)]
    pub options: std::collections::BTreeMap<String, String>,
}

/// Standard ANSI transaction isolation levels.
///
/// Drivers map this to the engine's native syntax; unsupported levels yield
/// [`crate::Error::Unsupported`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IsolationLevel {
    ReadUncommitted,
    ReadCommitted,
    RepeatableRead,
    Serializable,
}

/// Open session against a database.
///
/// All methods that mutate session state take `&mut self` to make ownership
/// explicit and to surface accidental concurrent use at compile time.
#[async_trait]
pub trait Connection: Send + Sync {
    /// Execute a single statement and return the materialised result set.
    ///
    /// Parameters are bound positionally. Drivers that do not implement
    /// server-side prepared statements emulate binding by escaping.
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult>;

    /// Execute a single statement and return a row stream.
    ///
    /// Streams release server-side resources only when the returned
    /// [`RowStream::close`] is called or the stream is dropped.
    async fn stream(&mut self, sql: &str, params: &[Value]) -> Result<Box<dyn RowStream>>;

    /// Begin a transaction with the engine's default isolation level.
    async fn begin(&mut self) -> Result<()>;

    /// Begin a transaction with the requested isolation level.
    async fn begin_with(&mut self, isolation: IsolationLevel) -> Result<()>;

    /// Commit the current transaction.
    async fn commit(&mut self) -> Result<()>;

    /// Roll back the current transaction.
    async fn rollback(&mut self) -> Result<()>;

    /// List logical schemas/namespaces visible to the session.
    async fn list_schemas(&mut self) -> Result<Vec<Schema>>;

    /// List tables and views inside `schema`.
    async fn list_tables(&mut self, schema: &str) -> Result<Vec<Table>>;

    /// Describe the columns, defaults and constraints of `schema.name`.
    async fn describe_table(&mut self, schema: &str, name: &str) -> Result<TableSchema>;

    /// Liveness probe.
    async fn ping(&mut self) -> Result<()>;

    /// Return a cancellation handle that may be used to abort the next query
    /// dispatched on this connection. `None` means the driver does not
    /// support out-of-band cancellation.
    fn cancel_handle(&self) -> Option<Box<dyn CancelHandle>>;

    /// Static capability descriptor for this driver.
    fn capabilities(&self) -> Capabilities;

    /// Tear down the underlying connection.
    async fn close(self: Box<Self>) -> Result<()>;
}
