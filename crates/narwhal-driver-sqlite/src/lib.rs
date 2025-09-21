//! SQLite driver backed by `rusqlite`.
//!
//! `rusqlite` is synchronous, so blocking calls are dispatched to
//! `tokio::task::spawn_blocking`. The connection is wrapped in an
//! [`tokio::sync::Mutex`] so concurrent driver method calls serialise
//! safely.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use narwhal_core::{
    CancelHandle, Capabilities, Connection, ConnectionConfig, DatabaseDriver, Error,
    IsolationLevel, QueryResult, Result, Schema, Table, TableSchema, Value,
};
use tokio::sync::Mutex;
use tracing::{debug, info};

#[derive(Debug, Default)]
pub struct SqliteDriver;

impl SqliteDriver {
    pub const NAME: &'static str = "sqlite";

    pub fn new() -> Self {
        Self
    }

    fn capabilities() -> Capabilities {
        Capabilities::default()
            .with_transactions(true)
            .with_cancellation(false)
            .with_multiple_schemas(false)
            .with_prepared_statements(true)
            .with_savepoints(true)
            .with_rows_affected(true)
    }
}

#[async_trait]
impl DatabaseDriver for SqliteDriver {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn display_name(&self) -> &'static str {
        "SQLite"
    }

    fn validate(&self, config: &ConnectionConfig) -> Vec<String> {
        if config.params.path.is_none() {
            vec!["path is required".into()]
        } else {
            Vec::new()
        }
    }

    async fn connect(
        &self,
        config: &ConnectionConfig,
        _password: Option<&str>,
    ) -> Result<Box<dyn Connection>> {
        let path = config
            .params
            .path
            .as_deref()
            .ok_or_else(|| Error::Config("path missing".into()))?
            .to_owned();
        let path_buf = PathBuf::from(&path);

        debug!(target: "narwhal::sqlite", path = %path, "opening database");
        let conn = tokio::task::spawn_blocking(move || rusqlite::Connection::open(path_buf))
            .await
            .map_err(|e| Error::Other(e.to_string()))?
            .map_err(|e| Error::Connection(e.to_string()))?;

        info!(target: "narwhal::sqlite", path = %path, "database opened");
        Ok(Box::new(SqliteConnection {
            inner: Arc::new(Mutex::new(conn)),
        }))
    }
}

pub struct SqliteConnection {
    inner: Arc<Mutex<rusqlite::Connection>>,
}

impl SqliteConnection {
    async fn execute_batch(&self, sql: &str) -> Result<()> {
        let inner = self.inner.clone();
        let sql = sql.to_owned();
        tokio::task::spawn_blocking(move || {
            let guard = inner.blocking_lock();
            guard
                .execute_batch(&sql)
                .map_err(|e| Error::Query(e.to_string()))
        })
        .await
        .map_err(|e| Error::Other(e.to_string()))?
    }
}

#[async_trait]
impl Connection for SqliteConnection {
    async fn execute(&mut self, _sql: &str, _params: &[Value]) -> Result<QueryResult> {
        Err(Error::unsupported(
            "sqlite execute: result mapping pending implementation",
        ))
    }

    async fn begin(&mut self) -> Result<()> {
        self.execute_batch("BEGIN").await
    }

    async fn begin_with(&mut self, isolation: IsolationLevel) -> Result<()> {
        // SQLite supports DEFERRED, IMMEDIATE and EXCLUSIVE transactions but
        // does not honour ANSI isolation levels in the conventional sense.
        // Map sensibly and fall back to the default for finer-grained levels.
        let stmt = match isolation {
            IsolationLevel::Serializable => "BEGIN EXCLUSIVE",
            IsolationLevel::RepeatableRead | IsolationLevel::ReadCommitted => "BEGIN IMMEDIATE",
            IsolationLevel::ReadUncommitted => "BEGIN DEFERRED",
        };
        self.execute_batch(stmt).await
    }

    async fn commit(&mut self) -> Result<()> {
        self.execute_batch("COMMIT").await
    }

    async fn rollback(&mut self) -> Result<()> {
        self.execute_batch("ROLLBACK").await
    }

    async fn list_schemas(&mut self) -> Result<Vec<Schema>> {
        Ok(vec![Schema {
            name: "main".into(),
        }])
    }

    async fn list_tables(&mut self, _schema: &str) -> Result<Vec<Table>> {
        Err(Error::unsupported(
            "sqlite list_tables pending implementation",
        ))
    }

    async fn describe_table(&mut self, _schema: &str, _name: &str) -> Result<TableSchema> {
        Err(Error::unsupported(
            "sqlite describe_table pending implementation",
        ))
    }

    async fn ping(&mut self) -> Result<()> {
        self.execute_batch("SELECT 1").await
    }

    fn cancel_handle(&self) -> Option<Box<dyn CancelHandle>> {
        None
    }

    fn capabilities(&self) -> Capabilities {
        SqliteDriver::capabilities()
    }

    async fn close(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}
