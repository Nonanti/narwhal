//! SQLite driver backed by `rusqlite`.
//!
//! `rusqlite` is synchronous, so every database call is dispatched onto a
//! blocking thread via [`tokio::task::spawn_blocking`]. The connection is
//! protected by a [`tokio::sync::Mutex`] so concurrent method invocations
//! serialise correctly.

#![forbid(unsafe_code)]

mod types;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use narwhal_core::{
    CancelHandle, Capabilities, Column, ColumnHeader, Connection, ConnectionConfig, DatabaseDriver,
    Error, IsolationLevel, QueryResult, Result, Row as CoreRow, RowStream, Schema, Table,
    TableKind, TableSchema, Value,
};
use rusqlite::params_from_iter;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task;
use tracing::{debug, info};

use crate::types::{value_from_ref, value_to_sql};

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

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
        let conn = task::spawn_blocking(move || rusqlite::Connection::open(path_buf))
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
    async fn run(&self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        let inner = self.inner.clone();
        let sql = sql.to_owned();
        let params: Vec<rusqlite::types::Value> = params.iter().map(value_to_sql).collect();

        task::spawn_blocking(move || run_blocking(&inner, &sql, params))
            .await
            .map_err(|e| Error::Other(e.to_string()))?
    }

    async fn execute_batch(&self, sql: &'static str) -> Result<()> {
        let inner = self.inner.clone();
        task::spawn_blocking(move || {
            inner
                .blocking_lock()
                .execute_batch(sql)
                .map_err(|e| Error::Query(e.to_string()))
        })
        .await
        .map_err(|e| Error::Other(e.to_string()))?
    }
}

fn run_blocking(
    inner: &Arc<Mutex<rusqlite::Connection>>,
    sql: &str,
    params: Vec<rusqlite::types::Value>,
) -> Result<QueryResult> {
    let started = Instant::now();
    let guard = inner.blocking_lock();
    let mut statement = guard
        .prepare(sql)
        .map_err(|e| Error::Query(e.to_string()))?;

    let column_count = statement.column_count();
    if column_count == 0 {
        let affected = statement
            .execute(params_from_iter(params.iter()))
            .map_err(|e| Error::Query(e.to_string()))?;
        return Ok(QueryResult {
            columns: Vec::new(),
            rows: Vec::new(),
            rows_affected: Some(affected as u64),
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
    }

    let headers: Vec<ColumnHeader> = statement
        .columns()
        .into_iter()
        .map(|c| ColumnHeader {
            name: c.name().to_owned(),
            data_type: c.decl_type().unwrap_or("").to_owned(),
        })
        .collect();

    let mut rows = statement
        .query(params_from_iter(params.iter()))
        .map_err(|e| Error::Query(e.to_string()))?;

    let mut collected = Vec::new();
    while let Some(row) = rows.next().map_err(|e| Error::Query(e.to_string()))? {
        let mut values = Vec::with_capacity(column_count);
        for idx in 0..column_count {
            let v = row.get_ref(idx).map_err(|e| Error::Query(e.to_string()))?;
            values.push(value_from_ref(v));
        }
        collected.push(CoreRow(values));
    }

    Ok(QueryResult {
        columns: headers,
        rows: collected,
        rows_affected: None,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

#[async_trait]
impl Connection for SqliteConnection {
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        self.run(sql, params).await
    }

    async fn stream(&mut self, sql: &str, params: &[Value]) -> Result<Box<dyn RowStream>> {
        let inner = self.inner.clone();
        let sql = sql.to_owned();
        let bound: Vec<rusqlite::types::Value> = params.iter().map(value_to_sql).collect();
        let (header_tx, header_rx) = oneshot::channel::<Result<Vec<ColumnHeader>>>();
        let (row_tx, row_rx) = mpsc::channel::<Result<CoreRow>>(64);

        task::spawn_blocking(move || {
            let guard = inner.blocking_lock();
            let mut statement = match guard.prepare(&sql) {
                Ok(stmt) => stmt,
                Err(error) => {
                    let _ = header_tx.send(Err(Error::Query(error.to_string())));
                    return;
                }
            };

            let headers: Vec<ColumnHeader> = statement
                .columns()
                .into_iter()
                .map(|c| ColumnHeader {
                    name: c.name().to_owned(),
                    data_type: c.decl_type().unwrap_or("").to_owned(),
                })
                .collect();
            let column_count = headers.len();
            if header_tx.send(Ok(headers)).is_err() {
                return;
            }
            if column_count == 0 {
                // Non-result-bearing statements never produce rows from a
                // stream; running them via execute() is the supported path.
                return;
            }

            let mut rows = match statement.query(params_from_iter(bound.iter())) {
                Ok(rows) => rows,
                Err(error) => {
                    let _ = row_tx.blocking_send(Err(Error::Query(error.to_string())));
                    return;
                }
            };

            loop {
                match rows.next() {
                    Ok(Some(row)) => {
                        let mut values = Vec::with_capacity(column_count);
                        let mut failure: Option<Error> = None;
                        for idx in 0..column_count {
                            match row.get_ref(idx) {
                                Ok(v) => values.push(value_from_ref(v)),
                                Err(error) => {
                                    failure = Some(Error::Query(error.to_string()));
                                    break;
                                }
                            }
                        }
                        let payload = match failure {
                            Some(err) => Err(err),
                            None => Ok(CoreRow(values)),
                        };
                        if row_tx.blocking_send(payload).is_err() {
                            // Consumer dropped the stream.
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let _ = row_tx.blocking_send(Err(Error::Query(error.to_string())));
                        break;
                    }
                }
            }
        });

        let columns = header_rx
            .await
            .map_err(|_| Error::Other("sqlite stream cancelled".into()))??;

        Ok(Box::new(SqliteRowStream {
            columns,
            rx: row_rx,
        }))
    }

    async fn begin(&mut self) -> Result<()> {
        self.execute_batch("BEGIN").await
    }

    async fn begin_with(&mut self, isolation: IsolationLevel) -> Result<()> {
        // SQLite does not honour ANSI isolation levels in the conventional
        // sense; map sensibly to its own transaction modes.
        match isolation {
            IsolationLevel::Serializable => self.execute_batch("BEGIN EXCLUSIVE").await,
            IsolationLevel::RepeatableRead | IsolationLevel::ReadCommitted => {
                self.execute_batch("BEGIN IMMEDIATE").await
            }
            IsolationLevel::ReadUncommitted => self.execute_batch("BEGIN DEFERRED").await,
        }
    }

    async fn commit(&mut self) -> Result<()> {
        self.execute_batch("COMMIT").await
    }

    async fn rollback(&mut self) -> Result<()> {
        self.execute_batch("ROLLBACK").await
    }

    async fn savepoint(&mut self, name: &str) -> Result<()> {
        let stmt = format!("SAVEPOINT {}", quote_ident(name));
        self.run(&stmt, &[]).await.map(|_| ())
    }

    async fn release_savepoint(&mut self, name: &str) -> Result<()> {
        let stmt = format!("RELEASE SAVEPOINT {}", quote_ident(name));
        self.run(&stmt, &[]).await.map(|_| ())
    }

    async fn rollback_to_savepoint(&mut self, name: &str) -> Result<()> {
        let stmt = format!("ROLLBACK TO SAVEPOINT {}", quote_ident(name));
        self.run(&stmt, &[]).await.map(|_| ())
    }

    async fn list_schemas(&mut self) -> Result<Vec<Schema>> {
        Ok(vec![Schema {
            name: "main".into(),
        }])
    }

    async fn list_tables(&mut self, _schema: &str) -> Result<Vec<Table>> {
        const SQL: &str = "
            SELECT name, type
            FROM sqlite_master
            WHERE type IN ('table', 'view')
              AND name NOT LIKE 'sqlite_%'
            ORDER BY name";
        let result = self.run(SQL, &[]).await?;

        let mut out = Vec::with_capacity(result.rows.len());
        for row in result.rows {
            let mut iter = row.0.into_iter();
            let name = match iter.next() {
                Some(Value::String(s)) => s,
                _ => continue,
            };
            let kind = match iter.next() {
                Some(Value::String(s)) if s == "view" => TableKind::View,
                _ => TableKind::Table,
            };
            out.push(Table {
                schema: "main".into(),
                name,
                kind,
            });
        }
        Ok(out)
    }

    async fn describe_table(&mut self, _schema: &str, name: &str) -> Result<TableSchema> {
        // `PRAGMA table_info` does not accept bound parameters; quote the
        // identifier so embedded special characters do not break the
        // statement.
        let escaped = name.replace('"', "\"\"");
        let sql = format!("PRAGMA table_info(\"{escaped}\")");
        let result = self.run(&sql, &[]).await?;

        if result.rows.is_empty() {
            return Err(Error::Schema(format!("table {name} not found")));
        }

        let columns = result
            .rows
            .into_iter()
            .filter_map(|row| {
                let mut iter = row.0.into_iter();
                let _cid = iter.next()?;
                let col_name = match iter.next()? {
                    Value::String(s) => s,
                    _ => return None,
                };
                let data_type = match iter.next()? {
                    Value::String(s) => s,
                    _ => String::new(),
                };
                let notnull = matches!(iter.next()?, Value::Int(1));
                let default = match iter.next()? {
                    Value::String(s) => Some(s),
                    Value::Int(i) => Some(i.to_string()),
                    Value::Float(f) => Some(f.to_string()),
                    _ => None,
                };
                let primary_key = matches!(iter.next()?, Value::Int(i) if i > 0);
                Some(Column {
                    name: col_name,
                    data_type,
                    nullable: !notnull,
                    primary_key,
                    default,
                })
            })
            .collect();

        Ok(TableSchema {
            table: Table {
                schema: "main".into(),
                name: name.to_owned(),
                kind: TableKind::Table,
            },
            columns,
        })
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

struct SqliteRowStream {
    columns: Vec<ColumnHeader>,
    rx: mpsc::Receiver<Result<CoreRow>>,
}

#[async_trait]
impl RowStream for SqliteRowStream {
    fn columns(&self) -> &[ColumnHeader] {
        &self.columns
    }

    async fn next_row(&mut self) -> Result<Option<CoreRow>> {
        match self.rx.recv().await {
            Some(Ok(row)) => Ok(Some(row)),
            Some(Err(error)) => Err(error),
            None => Ok(None),
        }
    }

    async fn close(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use narwhal_core::{ConnectionConfig, ConnectionParams};
    use uuid::Uuid;

    fn memory_config() -> ConnectionConfig {
        ConnectionConfig {
            id: Uuid::nil(),
            name: "test".into(),
            driver: SqliteDriver::NAME.into(),
            params: ConnectionParams {
                path: Some(":memory:".into()),
                ..Default::default()
            },
        }
    }

    async fn open() -> Box<dyn Connection> {
        SqliteDriver::new()
            .connect(&memory_config(), None)
            .await
            .expect("open in-memory database")
    }

    #[tokio::test]
    async fn round_trip_select() {
        let mut conn = open().await;
        let result = conn
            .execute("SELECT 1 AS one, 'narwhal' AS name", &[])
            .await
            .unwrap();
        assert_eq!(result.columns.len(), 2);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].get(0).map(Value::render), Some("1".into()));
        assert_eq!(
            result.rows[0].get(1).map(Value::render),
            Some("narwhal".into())
        );
    }

    #[tokio::test]
    async fn parameter_binding_and_dml() {
        let mut conn = open().await;
        conn.execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            &[],
        )
        .await
        .unwrap();
        let dml = conn
            .execute(
                "INSERT INTO users (id, name) VALUES (?1, ?2)",
                &[Value::Int(1), Value::String("berkant".into())],
            )
            .await
            .unwrap();
        assert_eq!(dml.rows_affected, Some(1));

        let select = conn
            .execute("SELECT name FROM users WHERE id = ?1", &[Value::Int(1)])
            .await
            .unwrap();
        assert_eq!(select.rows.len(), 1);
        assert_eq!(
            select.rows[0].get(0).map(Value::render),
            Some("berkant".into())
        );
    }

    #[tokio::test]
    async fn savepoint_partial_rollback() {
        let mut conn = open().await;
        conn.execute("CREATE TABLE t (n INTEGER)", &[])
            .await
            .unwrap();
        conn.begin().await.unwrap();
        conn.execute("INSERT INTO t VALUES (1)", &[]).await.unwrap();
        conn.savepoint("sp1").await.unwrap();
        conn.execute("INSERT INTO t VALUES (2)", &[]).await.unwrap();
        conn.rollback_to_savepoint("sp1").await.unwrap();
        conn.release_savepoint("sp1").await.unwrap();
        conn.commit().await.unwrap();

        let result = conn
            .execute("SELECT n FROM t ORDER BY n", &[])
            .await
            .unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].get(0).map(Value::render), Some("1".into()));
    }

    #[tokio::test]
    async fn transaction_rollback() {
        let mut conn = open().await;
        conn.execute("CREATE TABLE t (n INTEGER)", &[])
            .await
            .unwrap();
        conn.begin().await.unwrap();
        conn.execute("INSERT INTO t VALUES (1)", &[]).await.unwrap();
        conn.rollback().await.unwrap();
        let result = conn.execute("SELECT count(*) FROM t", &[]).await.unwrap();
        assert_eq!(result.rows[0].get(0).map(Value::render), Some("0".into()));
    }

    #[tokio::test]
    async fn stream_yields_rows_in_order() {
        let mut conn = open().await;
        conn.execute("CREATE TABLE nums (n INTEGER)", &[])
            .await
            .unwrap();
        for i in 1..=5 {
            conn.execute("INSERT INTO nums VALUES (?1)", &[Value::Int(i)])
                .await
                .unwrap();
        }
        let mut stream = conn
            .stream("SELECT n FROM nums ORDER BY n", &[])
            .await
            .unwrap();
        assert_eq!(stream.columns().len(), 1);
        let mut collected = Vec::new();
        while let Some(row) = stream.next_row().await.unwrap() {
            collected.push(row.get(0).map(Value::render).unwrap_or_default());
        }
        assert_eq!(collected, vec!["1", "2", "3", "4", "5"]);
    }

    #[tokio::test]
    async fn list_and_describe() {
        let mut conn = open().await;
        conn.execute(
            "CREATE TABLE items (
                id INTEGER PRIMARY KEY,
                title TEXT NOT NULL,
                price REAL DEFAULT 0.0
            )",
            &[],
        )
        .await
        .unwrap();

        let tables = conn.list_tables("main").await.unwrap();
        assert!(tables.iter().any(|t| t.name == "items"));

        let schema = conn.describe_table("main", "items").await.unwrap();
        assert_eq!(schema.columns.len(), 3);
        assert_eq!(schema.columns[0].name, "id");
        assert!(schema.columns[0].primary_key);
        assert!(!schema.columns[1].nullable);
    }
}
