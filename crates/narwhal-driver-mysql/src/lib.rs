//! MySQL / MariaDB driver backed by `mysql_async`.
//!
//! The driver opens a dedicated connection per [`Connection`] instance
//! rather than sharing a pool internally; multi-connection workloads are
//! served by the `narwhal-pool` crate which is agnostic to the underlying
//! engine.

#![forbid(unsafe_code)]

mod types;

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use mysql_async::prelude::*;
use mysql_async::{Conn, Opts, OptsBuilder, Params};
use narwhal_core::{
    CancelHandle, Capabilities, Column, ColumnHeader, Connection, ConnectionConfig, DatabaseDriver,
    Error, IsolationLevel, QueryResult, Result, Row as CoreRow, RowStream, Schema, Table,
    TableKind, TableSchema, Value,
};
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::types::{column_header, value_from_my, value_to_my};

#[derive(Debug, Default)]
pub struct MysqlDriver;

impl MysqlDriver {
    pub const NAME: &'static str = "mysql";

    pub fn new() -> Self {
        Self
    }

    fn capabilities() -> Capabilities {
        Capabilities::default()
            .with_transactions(true)
            .with_cancellation(false)
            .with_multiple_schemas(true)
            .with_prepared_statements(true)
            .with_savepoints(true)
            .with_rows_affected(true)
    }
}

#[async_trait]
impl DatabaseDriver for MysqlDriver {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn display_name(&self) -> &'static str {
        "MySQL"
    }

    fn validate(&self, config: &ConnectionConfig) -> Vec<String> {
        let mut errors = Vec::new();
        if config.params.host.is_none() {
            errors.push("host is required".into());
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
        let opts = build_opts(config, password)?;
        debug!(target: "narwhal::mysql", "establishing connection");
        let conn = Conn::new(opts)
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        info!(target: "narwhal::mysql", "connection established");
        Ok(Box::new(MysqlConnection {
            inner: Arc::new(Mutex::new(Some(conn))),
        }))
    }
}

fn build_opts(config: &ConnectionConfig, password: Option<&str>) -> Result<Opts> {
    let host = config
        .params
        .host
        .as_deref()
        .ok_or_else(|| Error::Config("host missing".into()))?;
    let user = config
        .params
        .username
        .as_deref()
        .ok_or_else(|| Error::Config("username missing".into()))?;

    let mut builder = OptsBuilder::default()
        .ip_or_hostname(host)
        .user(Some(user))
        .pass(password.map(str::to_owned));
    if let Some(port) = config.params.port {
        builder = builder.tcp_port(port);
    }
    if let Some(db) = config.params.database.as_deref() {
        builder = builder.db_name(Some(db));
    }
    Ok(Opts::from(builder))
}

pub struct MysqlConnection {
    inner: Arc<Mutex<Option<Conn>>>,
}

impl MysqlConnection {
    async fn with_conn<R, F>(&self, f: F) -> Result<R>
    where
        F: for<'a> FnOnce(
            &'a mut Conn,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<R>> + Send + 'a>,
        >,
    {
        let mut guard = self.inner.lock().await;
        let conn = guard
            .as_mut()
            .ok_or_else(|| Error::Connection("connection closed".into()))?;
        f(conn).await
    }
}

#[async_trait]
impl Connection for MysqlConnection {
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        let bound: Vec<mysql_async::Value> = params.iter().map(value_to_my).collect();
        let sql = sql.to_owned();
        let started = Instant::now();

        self.with_conn(move |conn| {
            Box::pin(async move {
                // MySQL's prepared-statement protocol rejects several
                // administrative statements (SAVEPOINT, SET TRANSACTION,
                // USE, ...). When no parameters are bound, fall back to the
                // text protocol so those statements still work.
                if bound.is_empty() {
                    let result = conn
                        .query_iter(sql.as_str())
                        .await
                        .map_err(|e| Error::Query(e.to_string()))?;
                    collect_text(result, started).await
                } else {
                    let result = conn
                        .exec_iter(sql.as_str(), Params::Positional(bound))
                        .await
                        .map_err(|e| Error::Query(e.to_string()))?;
                    collect_binary(result, started).await
                }
            })
        })
        .await
    }

    async fn stream(&mut self, sql: &str, params: &[Value]) -> Result<Box<dyn RowStream>> {
        // mysql_async streams rows back through QueryResult::stream, but the
        // returned stream borrows the connection. To keep the connection
        // protected by a single mutex without leaking the borrow, the entire
        // statement is currently materialised inside `execute` and replayed
        // through a buffered stream. Engines that benefit from server-side
        // cursoring (PostgreSQL) keep their native streaming path.
        let materialised = self.execute(sql, params).await?;
        Ok(Box::new(BufferedRowStream {
            columns: materialised.columns,
            rows: materialised.rows.into_iter(),
        }))
    }

    async fn begin(&mut self) -> Result<()> {
        self.execute("START TRANSACTION", &[]).await.map(|_| ())
    }

    async fn begin_with(&mut self, isolation: IsolationLevel) -> Result<()> {
        let level = match isolation {
            IsolationLevel::ReadUncommitted => "READ UNCOMMITTED",
            IsolationLevel::ReadCommitted => "READ COMMITTED",
            IsolationLevel::RepeatableRead => "REPEATABLE READ",
            IsolationLevel::Serializable => "SERIALIZABLE",
        };
        let stmt = format!("SET TRANSACTION ISOLATION LEVEL {level}");
        self.execute(&stmt, &[]).await?;
        self.execute("START TRANSACTION", &[]).await.map(|_| ())
    }

    async fn commit(&mut self) -> Result<()> {
        self.execute("COMMIT", &[]).await.map(|_| ())
    }

    async fn rollback(&mut self) -> Result<()> {
        self.execute("ROLLBACK", &[]).await.map(|_| ())
    }

    async fn savepoint(&mut self, name: &str) -> Result<()> {
        let stmt = format!("SAVEPOINT {}", quote_ident(name));
        self.execute(&stmt, &[]).await.map(|_| ())
    }

    async fn release_savepoint(&mut self, name: &str) -> Result<()> {
        let stmt = format!("RELEASE SAVEPOINT {}", quote_ident(name));
        self.execute(&stmt, &[]).await.map(|_| ())
    }

    async fn rollback_to_savepoint(&mut self, name: &str) -> Result<()> {
        let stmt = format!("ROLLBACK TO SAVEPOINT {}", quote_ident(name));
        self.execute(&stmt, &[]).await.map(|_| ())
    }

    async fn list_schemas(&mut self) -> Result<Vec<Schema>> {
        let result = self
            .execute(
                "SELECT schema_name FROM information_schema.schemata \
                 WHERE schema_name NOT IN ('mysql', 'information_schema', \
                 'performance_schema', 'sys') \
                 ORDER BY schema_name",
                &[],
            )
            .await?;
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
        let result = self
            .execute(
                "SELECT table_name, table_type FROM information_schema.tables \
                 WHERE table_schema = ? ORDER BY table_name",
                &[Value::String(schema.to_owned())],
            )
            .await?;

        let mut out = Vec::with_capacity(result.rows.len());
        for row in result.rows {
            let mut iter = row.0.into_iter();
            let name = match iter.next() {
                Some(Value::String(s)) => s,
                _ => continue,
            };
            let kind = match iter.next() {
                Some(Value::String(s)) => match s.as_str() {
                    "VIEW" => TableKind::View,
                    "SYSTEM VIEW" | "SYSTEM TABLE" => TableKind::SystemTable,
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
        let result = self
            .execute(
                "SELECT column_name, column_type, is_nullable, column_key, column_default \
                 FROM information_schema.columns \
                 WHERE table_schema = ? AND table_name = ? \
                 ORDER BY ordinal_position",
                &[
                    Value::String(schema.to_owned()),
                    Value::String(name.to_owned()),
                ],
            )
            .await?;

        if result.rows.is_empty() {
            return Err(Error::Schema(format!("table {schema}.{name} not found")));
        }

        let columns = result
            .rows
            .into_iter()
            .filter_map(|row| {
                let mut iter = row.0.into_iter();
                let col_name = match iter.next()? {
                    Value::String(s) => s,
                    _ => return None,
                };
                let data_type = match iter.next()? {
                    Value::String(s) => s,
                    _ => "unknown".into(),
                };
                let nullable = matches!(iter.next(), Some(Value::String(s)) if s == "YES");
                let primary_key = matches!(iter.next(), Some(Value::String(s)) if s == "PRI");
                let default = match iter.next() {
                    Some(Value::String(s)) => Some(s),
                    Some(Value::Int(i)) => Some(i.to_string()),
                    Some(Value::Float(f)) => Some(f.to_string()),
                    _ => None,
                };
                Some(Column {
                    name: col_name,
                    data_type,
                    nullable,
                    primary_key,
                    default,
                })
            })
            .collect();

        Ok(TableSchema {
            table: Table {
                schema: schema.to_owned(),
                name: name.to_owned(),
                kind: TableKind::Table,
            },
            columns,
        })
    }

    async fn ping(&mut self) -> Result<()> {
        self.with_conn(|conn| {
            Box::pin(async move {
                conn.ping()
                    .await
                    .map_err(|e| Error::Connection(e.to_string()))
            })
        })
        .await
    }

    fn cancel_handle(&self) -> Option<Box<dyn CancelHandle>> {
        None
    }

    fn capabilities(&self) -> Capabilities {
        MysqlDriver::capabilities()
    }

    async fn close(self: Box<Self>) -> Result<()> {
        let mut guard = self.inner.lock().await;
        if let Some(conn) = guard.take() {
            conn.disconnect()
                .await
                .map_err(|e| Error::Connection(e.to_string()))?;
        }
        Ok(())
    }
}

async fn collect_text(
    mut result: mysql_async::QueryResult<'_, '_, mysql_async::TextProtocol>,
    started: Instant,
) -> Result<QueryResult> {
    let columns: Vec<ColumnHeader> = result
        .columns()
        .map(|cols| cols.iter().map(column_header).collect())
        .unwrap_or_default();
    if columns.is_empty() {
        let affected = result.affected_rows();
        result
            .drop_result()
            .await
            .map_err(|e| Error::Query(e.to_string()))?;
        return Ok(QueryResult {
            columns: Vec::new(),
            rows: Vec::new(),
            rows_affected: Some(affected),
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
    }
    let raw_rows: Vec<mysql_async::Row> = result
        .collect()
        .await
        .map_err(|e| Error::Query(e.to_string()))?;
    let rows = map_rows(raw_rows, columns.len());
    Ok(QueryResult {
        columns,
        rows,
        rows_affected: None,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

async fn collect_binary(
    mut result: mysql_async::QueryResult<'_, '_, mysql_async::BinaryProtocol>,
    started: Instant,
) -> Result<QueryResult> {
    let columns: Vec<ColumnHeader> = result
        .columns()
        .map(|cols| cols.iter().map(column_header).collect())
        .unwrap_or_default();
    if columns.is_empty() {
        let affected = result.affected_rows();
        result
            .drop_result()
            .await
            .map_err(|e| Error::Query(e.to_string()))?;
        return Ok(QueryResult {
            columns: Vec::new(),
            rows: Vec::new(),
            rows_affected: Some(affected),
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
    }
    let raw_rows: Vec<mysql_async::Row> = result
        .collect()
        .await
        .map_err(|e| Error::Query(e.to_string()))?;
    let rows = map_rows(raw_rows, columns.len());
    Ok(QueryResult {
        columns,
        rows,
        rows_affected: None,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

fn map_rows(rows: Vec<mysql_async::Row>, column_count: usize) -> Vec<CoreRow> {
    rows.into_iter()
        .map(|row| {
            let mut values = Vec::with_capacity(column_count);
            for value in row.unwrap_raw() {
                values.push(value_from_my(&value.unwrap_or(mysql_async::Value::NULL)));
            }
            CoreRow(values)
        })
        .collect()
}

struct BufferedRowStream {
    columns: Vec<ColumnHeader>,
    rows: std::vec::IntoIter<CoreRow>,
}

#[async_trait]
impl RowStream for BufferedRowStream {
    fn columns(&self) -> &[ColumnHeader] {
        &self.columns
    }

    async fn next_row(&mut self) -> Result<Option<CoreRow>> {
        Ok(self.rows.next())
    }

    async fn close(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}

fn quote_ident(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}
