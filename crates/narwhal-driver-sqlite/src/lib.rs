//! SQLite driver for narwhal.
//!
//! `rusqlite` is sync, so each driver call hops onto a blocking thread via
//! `tokio::task::spawn_blocking`. The connection itself is wrapped in a
//! `Mutex` and lives on a dedicated parking lot.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use narwhal_core::{
    Connection, ConnectionConfig, DatabaseDriver, Error, QueryResult, Result, Schema, Table,
    TableSchema,
};
use tokio::sync::Mutex;
use tracing::{debug, info};

pub struct SqliteDriver;

impl SqliteDriver {
    pub const NAME: &'static str = "sqlite";

    pub fn new() -> Self {
        Self
    }
}

impl Default for SqliteDriver {
    fn default() -> Self {
        Self::new()
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
        let mut errs = Vec::new();
        if config.params.path.is_none() {
            errs.push("path is required".into());
        }
        errs
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

        debug!(target: "narwhal::sqlite", path = %path, "opening");
        let conn = tokio::task::spawn_blocking(move || rusqlite::Connection::open(path_buf))
            .await
            .map_err(|e| Error::Other(e.to_string()))?
            .map_err(|e| Error::Connection(e.to_string()))?;

        info!(target: "narwhal::sqlite", path = %path, "opened");
        Ok(Box::new(SqliteConnection {
            inner: Arc::new(Mutex::new(conn)),
        }))
    }
}

pub struct SqliteConnection {
    inner: Arc<Mutex<rusqlite::Connection>>,
}

#[async_trait]
impl Connection for SqliteConnection {
    async fn execute(&mut self, sql: &str) -> Result<QueryResult> {
        // TODO: real result-set mapping via prepare/query + column types.
        let _ = sql;
        let _ = &self.inner;
        Err(Error::Query("sqlite execute not implemented yet".into()))
    }

    async fn list_schemas(&mut self) -> Result<Vec<Schema>> {
        // SQLite has no real schemas; expose a synthetic "main".
        Ok(vec![Schema {
            name: "main".into(),
        }])
    }

    async fn list_tables(&mut self, _schema: &str) -> Result<Vec<Table>> {
        Err(Error::Query("list_tables not implemented yet".into()))
    }

    async fn describe_table(&mut self, _schema: &str, _name: &str) -> Result<TableSchema> {
        Err(Error::Query("describe_table not implemented yet".into()))
    }

    async fn ping(&mut self) -> Result<()> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let guard = inner.blocking_lock();
            guard
                .execute_batch("SELECT 1")
                .map_err(|e| Error::Connection(e.to_string()))
        })
        .await
        .map_err(|e| Error::Other(e.to_string()))?
    }

    async fn close(self: Box<Self>) -> Result<()> {
        // Dropping the Arc/Mutex closes the connection.
        Ok(())
    }
}
