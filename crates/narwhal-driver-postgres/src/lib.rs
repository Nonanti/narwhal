//! PostgreSQL driver backed by `tokio-postgres`.

#![forbid(unsafe_code)]

use std::sync::Arc;

use async_trait::async_trait;
use narwhal_core::{
    CancelHandle, Capabilities, Connection, ConnectionConfig, DatabaseDriver, Error,
    IsolationLevel, QueryResult, Result, Schema, Table, TableSchema, Value,
};
use tokio_postgres::NoTls;
use tracing::{debug, error, info};

/// PostgreSQL driver. Currently uses `NoTls`; TLS support will be added once
/// the configuration surface for certificates is finalised.
#[derive(Debug, Default)]
pub struct PostgresDriver;

impl PostgresDriver {
    pub const NAME: &'static str = "postgres";

    pub fn new() -> Self {
        Self
    }

    fn capabilities() -> Capabilities {
        Capabilities::default()
            .with_transactions(true)
            .with_cancellation(true)
            .with_multiple_schemas(true)
            .with_prepared_statements(true)
            .with_savepoints(true)
            .with_rows_affected(true)
    }
}

#[async_trait]
impl DatabaseDriver for PostgresDriver {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn display_name(&self) -> &'static str {
        "PostgreSQL"
    }

    fn validate(&self, config: &ConnectionConfig) -> Vec<String> {
        let mut errors = Vec::new();
        if config.params.host.is_none() {
            errors.push("host is required".into());
        }
        if config.params.database.is_none() {
            errors.push("database is required".into());
        }
        if config.params.username.is_none() {
            errors.push("username is required".into());
        }
        errors
    }

    async fn connect(
        &self,
        config: &ConnectionConfig,
        password: Option<&str>,
    ) -> Result<Box<dyn Connection>> {
        let connection_string = build_connection_string(config, password)?;
        debug!(target: "narwhal::postgres", "establishing connection");

        let (client, connection) = tokio_postgres::connect(&connection_string, NoTls)
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;

        tokio::spawn(async move {
            if let Err(e) = connection.await {
                error!(target: "narwhal::postgres", error = %e, "connection task terminated");
            }
        });

        info!(target: "narwhal::postgres", "connection established");
        Ok(Box::new(PostgresConnection {
            client: Arc::new(client),
        }))
    }
}

fn build_connection_string(config: &ConnectionConfig, password: Option<&str>) -> Result<String> {
    let host = config
        .params
        .host
        .as_deref()
        .ok_or_else(|| Error::Config("host missing".into()))?;
    let port = config.params.port.unwrap_or(5432);
    let database = config
        .params
        .database
        .as_deref()
        .ok_or_else(|| Error::Config("database missing".into()))?;
    let user = config
        .params
        .username
        .as_deref()
        .ok_or_else(|| Error::Config("username missing".into()))?;

    let mut out = format!("host={host} port={port} dbname={database} user={user}");
    if let Some(pw) = password {
        out.push_str(&format!(" password={pw}"));
    }
    for (k, v) in &config.params.options {
        out.push_str(&format!(" {k}={v}"));
    }
    Ok(out)
}

pub struct PostgresConnection {
    client: Arc<tokio_postgres::Client>,
}

#[async_trait]
impl Connection for PostgresConnection {
    async fn execute(&mut self, _sql: &str, _params: &[Value]) -> Result<QueryResult> {
        Err(Error::unsupported(
            "postgres execute: result mapping pending implementation",
        ))
    }

    async fn begin(&mut self) -> Result<()> {
        self.client
            .batch_execute("BEGIN")
            .await
            .map_err(|e| Error::Query(e.to_string()))
    }

    async fn begin_with(&mut self, isolation: IsolationLevel) -> Result<()> {
        let level = match isolation {
            IsolationLevel::ReadUncommitted => "READ UNCOMMITTED",
            IsolationLevel::ReadCommitted => "READ COMMITTED",
            IsolationLevel::RepeatableRead => "REPEATABLE READ",
            IsolationLevel::Serializable => "SERIALIZABLE",
        };
        let stmt = format!("BEGIN ISOLATION LEVEL {level}");
        self.client
            .batch_execute(&stmt)
            .await
            .map_err(|e| Error::Query(e.to_string()))
    }

    async fn commit(&mut self) -> Result<()> {
        self.client
            .batch_execute("COMMIT")
            .await
            .map_err(|e| Error::Query(e.to_string()))
    }

    async fn rollback(&mut self) -> Result<()> {
        self.client
            .batch_execute("ROLLBACK")
            .await
            .map_err(|e| Error::Query(e.to_string()))
    }

    async fn list_schemas(&mut self) -> Result<Vec<Schema>> {
        Err(Error::unsupported(
            "postgres list_schemas pending implementation",
        ))
    }

    async fn list_tables(&mut self, _schema: &str) -> Result<Vec<Table>> {
        Err(Error::unsupported(
            "postgres list_tables pending implementation",
        ))
    }

    async fn describe_table(&mut self, _schema: &str, _name: &str) -> Result<TableSchema> {
        Err(Error::unsupported(
            "postgres describe_table pending implementation",
        ))
    }

    async fn ping(&mut self) -> Result<()> {
        self.client
            .simple_query("SELECT 1")
            .await
            .map(|_| ())
            .map_err(|e| Error::Connection(e.to_string()))
    }

    fn cancel_handle(&self) -> Option<Box<dyn CancelHandle>> {
        Some(Box::new(PostgresCancelHandle {
            token: self.client.cancel_token(),
        }))
    }

    fn capabilities(&self) -> Capabilities {
        PostgresDriver::capabilities()
    }

    async fn close(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}

struct PostgresCancelHandle {
    token: tokio_postgres::CancelToken,
}

#[async_trait]
impl CancelHandle for PostgresCancelHandle {
    async fn cancel(&self) -> Result<()> {
        self.token
            .cancel_query::<NoTls>(NoTls)
            .await
            .map_err(|e| Error::Connection(e.to_string()))
    }
}

