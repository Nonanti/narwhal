//! PostgreSQL driver backed by `tokio-postgres`.
//!
//! Transport security is configured via the `sslmode` option in the
//! connection parameters. Supported values mirror libpq:
//!
//! - `disable` (default): no TLS.
//! - `require`: TLS is required; the server certificate is accepted without
//!   verification (suitable for trusted networks where confidentiality is
//!   the goal but a CA chain is not deployed).
//! - `verify-ca` / `verify-full`: TLS with hostname and chain verification
//!   against the operating system's trust store.

#![forbid(unsafe_code)]

mod tls;
mod types;

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures::stream::StreamExt;
use narwhal_core::{
    CancelHandle, Capabilities, Column, ColumnHeader, Connection, ConnectionConfig, DatabaseDriver,
    Error, IsolationLevel, QueryResult, Result, Row as CoreRow, RowStream, Schema, Table,
    TableKind, TableSchema, Value,
};
use tokio_postgres::error::SqlState;
use tokio_postgres::types::{ToSql, Type};
use tokio_postgres::NoTls;
use tracing::{debug, error, info};

use crate::tls::{make_tls_connector, SslMode};
use crate::types::{column_to_value, Param};

/// PostgreSQL driver. The current implementation uses `NoTls`; configurable
/// TLS support is planned and tracked separately.
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
        let sslmode = SslMode::from_options(&config.params.options)?;
        debug!(target: "narwhal::postgres", sslmode = %sslmode.as_str(), "establishing connection");

        let client = match sslmode {
            SslMode::Disable => {
                let (client, connection) = tokio_postgres::connect(&connection_string, NoTls)
                    .await
                    .map_err(|e| Error::Connection(e.to_string()))?;
                spawn_connection(connection);
                client
            }
            other => {
                let connector = make_tls_connector(other)?;
                let (client, connection) = tokio_postgres::connect(&connection_string, connector)
                    .await
                    .map_err(|e| Error::Connection(e.to_string()))?;
                spawn_connection(connection);
                client
            }
        };

        info!(target: "narwhal::postgres", "connection established");
        Ok(Box::new(PostgresConnection {
            client: Arc::new(client),
        }))
    }
}

fn spawn_connection<S, T>(connection: tokio_postgres::Connection<S, T>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    T: tokio_postgres::tls::TlsStream + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            error!(target: "narwhal::postgres", error = %e, "connection task terminated");
        }
    });
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

fn map_pg_error(error: tokio_postgres::Error) -> Error {
    if let Some(db) = error.as_db_error() {
        if db.code() == &SqlState::QUERY_CANCELED {
            return Error::Cancelled;
        }
    }
    Error::Query(error.to_string())
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

impl PostgresConnection {
    /// Prepare the statement, then route to `query` or `execute` based on
    /// whether the statement returns rows.
    async fn run(&self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        let started = Instant::now();
        let statement = self.client.prepare(sql).await.map_err(map_pg_error)?;

        let bindings: Vec<Param<'_>> = params.iter().map(Param).collect();
        let param_refs: Vec<&(dyn ToSql + Sync)> =
            bindings.iter().map(|p| p as &(dyn ToSql + Sync)).collect();

        if statement.columns().is_empty() {
            let affected = self
                .client
                .execute(&statement, &param_refs[..])
                .await
                .map_err(map_pg_error)?;
            Ok(QueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                rows_affected: Some(affected),
                elapsed_ms: started.elapsed().as_millis() as u64,
            })
        } else {
            let rows = self
                .client
                .query(&statement, &param_refs[..])
                .await
                .map_err(map_pg_error)?;

            let columns: Vec<ColumnHeader> = statement
                .columns()
                .iter()
                .map(|c| ColumnHeader {
                    name: c.name().to_owned(),
                    data_type: c.type_().name().to_owned(),
                })
                .collect();

            let mut mapped = Vec::with_capacity(rows.len());
            for row in &rows {
                let mut values = Vec::with_capacity(row.len());
                for (idx, col) in row.columns().iter().enumerate() {
                    values.push(column_to_value(row, idx, col.type_())?);
                }
                mapped.push(CoreRow(values));
            }

            Ok(QueryResult {
                columns,
                rows: mapped,
                rows_affected: None,
                elapsed_ms: started.elapsed().as_millis() as u64,
            })
        }
    }
}

#[async_trait]
impl Connection for PostgresConnection {
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        self.run(sql, params).await
    }

    async fn stream(&mut self, sql: &str, params: &[Value]) -> Result<Box<dyn RowStream>> {
        let statement = self.client.prepare(sql).await.map_err(map_pg_error)?;

        let columns: Vec<ColumnHeader> = statement
            .columns()
            .iter()
            .map(|c| ColumnHeader {
                name: c.name().to_owned(),
                data_type: c.type_().name().to_owned(),
            })
            .collect();
        let column_types: Vec<Type> = statement
            .columns()
            .iter()
            .map(|c| c.type_().clone())
            .collect();

        let owned_params: Vec<Value> = params.to_vec();
        let bindings: Vec<Param<'_>> = owned_params.iter().map(Param).collect();
        let inner = self
            .client
            .query_raw(&statement, bindings.iter())
            .await
            .map_err(map_pg_error)?;

        Ok(Box::new(PostgresRowStream {
            columns,
            column_types,
            inner: Box::pin(inner),
            _params: owned_params,
        }))
    }

    async fn begin(&mut self) -> Result<()> {
        self.client
            .batch_execute("BEGIN")
            .await
            .map_err(map_pg_error)
    }

    async fn begin_with(&mut self, isolation: IsolationLevel) -> Result<()> {
        let level = match isolation {
            IsolationLevel::ReadUncommitted => "READ UNCOMMITTED",
            IsolationLevel::ReadCommitted => "READ COMMITTED",
            IsolationLevel::RepeatableRead => "REPEATABLE READ",
            IsolationLevel::Serializable => "SERIALIZABLE",
        };
        let stmt = format!("BEGIN ISOLATION LEVEL {level}");
        self.client.batch_execute(&stmt).await.map_err(map_pg_error)
    }

    async fn commit(&mut self) -> Result<()> {
        self.client
            .batch_execute("COMMIT")
            .await
            .map_err(map_pg_error)
    }

    async fn rollback(&mut self) -> Result<()> {
        self.client
            .batch_execute("ROLLBACK")
            .await
            .map_err(map_pg_error)
    }

    async fn savepoint(&mut self, name: &str) -> Result<()> {
        let stmt = format!("SAVEPOINT {}", quote_ident(name));
        self.client.batch_execute(&stmt).await.map_err(map_pg_error)
    }

    async fn release_savepoint(&mut self, name: &str) -> Result<()> {
        let stmt = format!("RELEASE SAVEPOINT {}", quote_ident(name));
        self.client.batch_execute(&stmt).await.map_err(map_pg_error)
    }

    async fn rollback_to_savepoint(&mut self, name: &str) -> Result<()> {
        let stmt = format!("ROLLBACK TO SAVEPOINT {}", quote_ident(name));
        self.client.batch_execute(&stmt).await.map_err(map_pg_error)
    }

    async fn list_schemas(&mut self) -> Result<Vec<Schema>> {
        const SQL: &str = "
            SELECT nspname
            FROM pg_catalog.pg_namespace
            WHERE nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
              AND nspname NOT LIKE 'pg_temp_%'
              AND nspname NOT LIKE 'pg_toast_temp_%'
            ORDER BY nspname";
        let result = self.run(SQL, &[]).await?;
        Ok(result
            .rows
            .into_iter()
            .filter_map(|row| match row.0.into_iter().next() {
                Some(Value::String(name)) => Some(Schema { name }),
                _ => None,
            })
            .collect())
    }

    async fn list_tables(&mut self, schema: &str) -> Result<Vec<Table>> {
        const SQL: &str = "
            SELECT c.relname, c.relkind::text
            FROM pg_catalog.pg_class c
            JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
            WHERE n.nspname = $1
              AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
            ORDER BY c.relname";
        let result = self.run(SQL, &[Value::String(schema.to_owned())]).await?;

        let mut out = Vec::with_capacity(result.rows.len());
        for row in result.rows {
            let mut iter = row.0.into_iter();
            let name = match iter.next() {
                Some(Value::String(s)) => s,
                _ => continue,
            };
            let kind = match iter.next() {
                Some(Value::String(s)) => match s.as_str() {
                    "r" | "p" => TableKind::Table,
                    "v" => TableKind::View,
                    "m" => TableKind::MaterializedView,
                    "f" => TableKind::Table,
                    _ => TableKind::Table,
                },
                _ => TableKind::Table,
            };
            out.push(Table {
                schema: schema.to_owned(),
                name,
                kind,
            });
        }
        Ok(out)
    }

    async fn describe_table(&mut self, schema: &str, name: &str) -> Result<TableSchema> {
        const SQL: &str = "
            SELECT
                a.attname,
                pg_catalog.format_type(a.atttypid, a.atttypmod),
                NOT a.attnotnull,
                COALESCE(i.indisprimary, false),
                pg_catalog.pg_get_expr(d.adbin, d.adrelid),
                c.relkind::text
            FROM pg_catalog.pg_attribute a
            JOIN pg_catalog.pg_class c ON c.oid = a.attrelid
            JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
            LEFT JOIN pg_catalog.pg_attrdef d
                ON d.adrelid = a.attrelid AND d.adnum = a.attnum
            LEFT JOIN pg_catalog.pg_index i
                ON i.indrelid = c.oid AND a.attnum = ANY(i.indkey) AND i.indisprimary
            WHERE n.nspname = $1 AND c.relname = $2 AND a.attnum > 0 AND NOT a.attisdropped
            ORDER BY a.attnum";

        let result = self
            .run(
                SQL,
                &[
                    Value::String(schema.to_owned()),
                    Value::String(name.to_owned()),
                ],
            )
            .await?;

        if result.rows.is_empty() {
            return Err(Error::Schema(format!("table {schema}.{name} not found")));
        }

        let mut columns = Vec::with_capacity(result.rows.len());
        let mut kind = TableKind::Table;
        for row in result.rows {
            let mut iter = row.0.into_iter();
            let col_name = match iter.next() {
                Some(Value::String(s)) => s,
                _ => continue,
            };
            let data_type = match iter.next() {
                Some(Value::String(s)) => s,
                Some(Value::Unknown(s)) => s,
                _ => "unknown".into(),
            };
            let nullable = matches!(iter.next(), Some(Value::Bool(true)));
            let primary_key = matches!(iter.next(), Some(Value::Bool(true)));
            let default = match iter.next() {
                Some(Value::String(s)) => Some(s),
                Some(Value::Unknown(s)) => Some(s),
                _ => None,
            };
            if let Some(Value::String(relkind)) = iter.next() {
                kind = match relkind.as_str() {
                    "v" => TableKind::View,
                    "m" => TableKind::MaterializedView,
                    _ => TableKind::Table,
                };
            }

            columns.push(Column {
                name: col_name,
                data_type,
                nullable,
                primary_key,
                default,
            });
        }

        Ok(TableSchema {
            table: Table {
                schema: schema.to_owned(),
                name: name.to_owned(),
                kind,
            },
            columns,
        })
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

struct PostgresRowStream {
    columns: Vec<ColumnHeader>,
    column_types: Vec<Type>,
    inner: std::pin::Pin<Box<tokio_postgres::RowStream>>,
    _params: Vec<Value>,
}

#[async_trait]
impl RowStream for PostgresRowStream {
    fn columns(&self) -> &[ColumnHeader] {
        &self.columns
    }

    async fn next_row(&mut self) -> Result<Option<CoreRow>> {
        match self.inner.next().await {
            Some(Ok(row)) => {
                let mut values = Vec::with_capacity(self.column_types.len());
                for (idx, ty) in self.column_types.iter().enumerate() {
                    values.push(column_to_value(&row, idx, ty)?);
                }
                Ok(Some(CoreRow(values)))
            }
            Some(Err(error)) => Err(map_pg_error(error)),
            None => Ok(None),
        }
    }

    async fn close(self: Box<Self>) -> Result<()> {
        // The portal is released when the underlying `RowStream` is dropped.
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

#[cfg(test)]
mod tests {
    use super::*;
    use narwhal_core::ConnectionParams;
    use uuid::Uuid;

    fn config(params: ConnectionParams) -> ConnectionConfig {
        ConnectionConfig {
            id: Uuid::nil(),
            name: "test".into(),
            driver: PostgresDriver::NAME.into(),
            params,
        }
    }

    #[test]
    fn validate_reports_missing_fields() {
        let driver = PostgresDriver::new();
        let errors = driver.validate(&config(ConnectionParams::default()));
        assert_eq!(errors.len(), 3);
    }

    #[test]
    fn connection_string_includes_password_and_options() {
        let mut options = std::collections::BTreeMap::new();
        options.insert("sslmode".into(), "require".into());
        let params = ConnectionParams {
            host: Some("db.local".into()),
            port: Some(6543),
            database: Some("analytics".into()),
            username: Some("reader".into()),
            options,
            ..Default::default()
        };
        let cfg = config(params);
        let cs = build_connection_string(&cfg, Some("hunter2")).unwrap();
        assert!(cs.contains("host=db.local"));
        assert!(cs.contains("port=6543"));
        assert!(cs.contains("dbname=analytics"));
        assert!(cs.contains("user=reader"));
        assert!(cs.contains("password=hunter2"));
        assert!(cs.contains("sslmode=require"));
    }

    #[test]
    fn capabilities_match_engine() {
        let caps = PostgresDriver::capabilities();
        assert!(caps.transactions);
        assert!(caps.cancellation);
        assert!(caps.multiple_schemas);
        assert!(caps.prepared_statements);
    }
}
