//! PostgreSQL driver for narwhal.
//!
//! This is a thin shell over `tokio-postgres`. The detailed type mapping
//! (`tokio_postgres::types::Type` → `narwhal_core::Value`) is intentionally
//! left as a TODO so we can flesh it out together with realistic queries.

use async_trait::async_trait;
use narwhal_core::{
    Connection, ConnectionConfig, DatabaseDriver, Error, QueryResult, Result, Schema, Table,
    TableSchema,
};
use tokio_postgres::NoTls;
use tracing::{debug, info};

pub struct PostgresDriver;

impl PostgresDriver {
    pub const NAME: &'static str = "postgres";

    pub fn new() -> Self {
        Self
    }
}

impl Default for PostgresDriver {
    fn default() -> Self {
        Self::new()
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
        let mut errs = Vec::new();
        if config.params.host.is_none() {
            errs.push("host is required".into());
        }
        if config.params.database.is_none() {
            errs.push("database is required".into());
        }
        if config.params.username.is_none() {
            errs.push("username is required".into());
        }
        errs
    }

    async fn connect(
        &self,
        config: &ConnectionConfig,
        password: Option<&str>,
    ) -> Result<Box<dyn Connection>> {
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

        let mut conn_str = format!(
            "host={host} port={port} dbname={database} user={user}",
            host = host,
            port = port,
            database = database,
            user = user,
        );
        if let Some(pw) = password {
            conn_str.push_str(&format!(" password={}", pw));
        }
        for (k, v) in &config.params.options {
            conn_str.push_str(&format!(" {}={}", k, v));
        }

        debug!(target: "narwhal::postgres", host, port, database, user, "connecting");
        let (client, connection) = tokio_postgres::connect(&conn_str, NoTls)
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;

        // Drive the connection task in the background.
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::error!(target: "narwhal::postgres", error = %e, "connection task failed");
            }
        });

        info!(target: "narwhal::postgres", "connected");
        Ok(Box::new(PostgresConnection { client }))
    }
}

pub struct PostgresConnection {
    client: tokio_postgres::Client,
}

#[async_trait]
impl Connection for PostgresConnection {
    async fn execute(&mut self, sql: &str) -> Result<QueryResult> {
        // TODO: proper Row → Value mapping; this is a placeholder so the
        // skeleton compiles and pings work.
        let _ = sql;
        let _ = &self.client;
        Err(Error::Query("postgres execute not implemented yet".into()))
    }

    async fn list_schemas(&mut self) -> Result<Vec<Schema>> {
        Err(Error::Query("list_schemas not implemented yet".into()))
    }

    async fn list_tables(&mut self, _schema: &str) -> Result<Vec<Table>> {
        Err(Error::Query("list_tables not implemented yet".into()))
    }

    async fn describe_table(&mut self, _schema: &str, _name: &str) -> Result<TableSchema> {
        Err(Error::Query("describe_table not implemented yet".into()))
    }

    async fn ping(&mut self) -> Result<()> {
        self.client
            .simple_query("SELECT 1")
            .await
            .map(|_| ())
            .map_err(|e| Error::Connection(e.to_string()))
    }

    async fn close(self: Box<Self>) -> Result<()> {
        // tokio-postgres closes when the client is dropped.
        Ok(())
    }
}
