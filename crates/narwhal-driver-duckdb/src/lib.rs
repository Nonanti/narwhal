//! DuckDB driver backed by the `duckdb` crate (an embedded OLAP engine
//! whose Rust API is a near-fork of `rusqlite`).
//!
//! Like SQLite, every call is synchronous so we dispatch the work onto
//! [`tokio::task::spawn_blocking`] and serialise concurrent use behind a
//! [`tokio::sync::Mutex`]. Unlike SQLite, DuckDB:
//!
//! * supports multiple logical schemas, surfaced via `information_schema`;
//! * has a richer type lattice (huge ints, decimals, intervals, lists,
//!   structs, maps, unions). [`crate::types`] keeps the lossy mapping in
//!   one place so the rest of the code stays simple;
//! * supports query cancellation through [`duckdb::InterruptHandle`].
//!
//! The intent is parity with the SQLite driver's surface area, so the
//! result set, schema discovery and transaction surface are uniform. DDL
//! and PRAGMA invocations are different (DuckDB uses `information_schema`
//! and a couple of `pragma_*` table-valued functions), so the underlying
//! SQL is bespoke even when the wire shape matches.

#![forbid(unsafe_code)]

mod types;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use duckdb::params_from_iter;
use duckdb::types::Value as DuckValue;
use narwhal_core::{
    CancelHandle, Capabilities, Column, ColumnHeader, Connection, ConnectionConfig, DatabaseDriver,
    Error, ForeignKey, Index, IsolationLevel, QueryResult, ReferentialAction, Result,
    Row as CoreRow, RowStream, Schema, Table, TableKind, TableSchema, UniqueConstraint, Value,
};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task;
use tracing::{debug, info};

use crate::types::{value_from_ref, value_to_sql};

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[derive(Debug, Default)]
pub struct DuckdbDriver;

impl DuckdbDriver {
    pub const NAME: &'static str = "duckdb";

    pub fn new() -> Self {
        Self
    }

    fn capabilities() -> Capabilities {
        Capabilities::default()
            .with_transactions(true)
            // DuckDB has InterruptHandle; we wire it up below.
            .with_cancellation(true)
            .with_multiple_schemas(true)
            .with_prepared_statements(true)
            .with_savepoints(true)
            .with_rows_affected(true)
    }
}

#[async_trait]
impl DatabaseDriver for DuckdbDriver {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn display_name(&self) -> &'static str {
        "DuckDB"
    }

    fn validate(&self, config: &ConnectionConfig) -> Vec<String> {
        if config.params.path.is_none() {
            vec!["path is required (use ':memory:' for an in-memory database)".into()]
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

        debug!(target: "narwhal::duckdb", path = %path, "opening database");
        let conn = task::spawn_blocking(move || {
            if path == ":memory:" {
                duckdb::Connection::open_in_memory()
            } else {
                duckdb::Connection::open(PathBuf::from(path))
            }
        })
        .await
        .map_err(|e| Error::Other(e.to_string()))?
        .map_err(|e| Error::Connection(e.to_string()))?;

        info!(target: "narwhal::duckdb", "database opened");
        let interrupt = conn.interrupt_handle();
        Ok(Box::new(DuckdbConnection {
            inner: Arc::new(Mutex::new(conn)),
            interrupt,
        }))
    }
}

pub struct DuckdbConnection {
    inner: Arc<Mutex<duckdb::Connection>>,
    interrupt: Arc<duckdb::InterruptHandle>,
}

impl DuckdbConnection {
    async fn run(&self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        let inner = self.inner.clone();
        let sql = sql.to_owned();
        let bound: Vec<DuckValue> = params.iter().map(value_to_sql).collect();

        task::spawn_blocking(move || run_blocking(&inner, &sql, bound))
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

    async fn execute_owned(&self, sql: String) -> Result<()> {
        let inner = self.inner.clone();
        task::spawn_blocking(move || {
            inner
                .blocking_lock()
                .execute_batch(&sql)
                .map_err(|e| Error::Query(e.to_string()))
        })
        .await
        .map_err(|e| Error::Other(e.to_string()))?
    }
}

/// Best-effort: does `sql` likely return a result set?
///
/// DuckDB's prepared-statement API requires us to commit to either
/// [`Statement::execute`] or [`Statement::query`] *before* it knows the
/// statement shape, so we infer from the leading keyword. The list
/// covers DuckDB's row-returning forms; everything else falls through
/// to `execute` and reports rows-affected.
fn statement_returns_rows(sql: &str) -> bool {
    let lead = sql
        .trim_start()
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(
        lead.as_str(),
        "SELECT"
            | "WITH"
            | "SHOW"
            | "DESCRIBE"
            | "EXPLAIN"
            | "VALUES"
            | "FROM"
            | "PRAGMA"
            | "TABLE"
            | "SUMMARIZE"
    )
}

fn run_blocking(
    inner: &Arc<Mutex<duckdb::Connection>>,
    sql: &str,
    params: Vec<DuckValue>,
) -> Result<QueryResult> {
    let started = Instant::now();
    let guard = inner.blocking_lock();
    let mut statement = guard
        .prepare(sql)
        .map_err(|e| Error::Query(e.to_string()))?;

    if !statement_returns_rows(sql) {
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

    let mut rows = statement
        .query(params_from_iter(params.iter()))
        .map_err(|e| Error::Query(e.to_string()))?;

    // After query() the statement has been executed, so column metadata is
    // available via the borrow handed back by [`Rows::as_ref`].
    let (column_count, headers) = match rows.as_ref() {
        Some(stmt) => {
            let count = stmt.column_count();
            let headers: Vec<ColumnHeader> = (0..count)
                .map(|idx| ColumnHeader {
                    name: stmt
                        .column_name(idx)
                        .map(|s| s.as_str())
                        .unwrap_or("")
                        .to_owned(),
                    data_type: format!("{:?}", stmt.column_type(idx)),
                })
                .collect();
            (count, headers)
        }
        None => (0, Vec::new()),
    };

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
impl Connection for DuckdbConnection {
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        self.run(sql, params).await
    }

    async fn stream(&mut self, sql: &str, params: &[Value]) -> Result<Box<dyn RowStream>> {
        let inner = self.inner.clone();
        let sql = sql.to_owned();
        let bound: Vec<DuckValue> = params.iter().map(value_to_sql).collect();
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

            if !statement_returns_rows(&sql) {
                // Non-result-bearing statement: report an empty header set
                // so the stream consumer terminates cleanly. The execute
                // path (run/run_blocking) is the canonical home for DML.
                let _ = header_tx.send(Ok(Vec::new()));
                return;
            }

            let mut rows = match statement.query(params_from_iter(bound.iter())) {
                Ok(rows) => rows,
                Err(error) => {
                    let _ = header_tx.send(Err(Error::Query(error.to_string())));
                    return;
                }
            };

            let (column_count, headers) = match rows.as_ref() {
                Some(stmt) => {
                    let count = stmt.column_count();
                    let headers: Vec<ColumnHeader> = (0..count)
                        .map(|idx| ColumnHeader {
                            name: stmt
                                .column_name(idx)
                                .map(|s| s.as_str())
                                .unwrap_or("")
                                .to_owned(),
                            data_type: format!("{:?}", stmt.column_type(idx)),
                        })
                        .collect();
                    (count, headers)
                }
                None => (0, Vec::new()),
            };
            if header_tx.send(Ok(headers)).is_err() {
                return;
            }
            if column_count == 0 {
                return;
            }

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
            .map_err(|_| Error::Other("duckdb stream cancelled".into()))??;

        Ok(Box::new(DuckdbRowStream {
            columns,
            rx: row_rx,
        }))
    }

    async fn begin(&mut self) -> Result<()> {
        self.execute_batch("BEGIN").await
    }

    async fn begin_with(&mut self, isolation: IsolationLevel) -> Result<()> {
        // DuckDB only supports snapshot isolation; the SQL accepts the
        // ANSI keywords but they're effectively a no-op apart from
        // surfacing parse errors for typos. Map every level to the same
        // BEGIN statement so the contract is honoured without surprising
        // the user with "unsupported".
        let _ = isolation;
        self.execute_batch("BEGIN TRANSACTION").await
    }

    async fn commit(&mut self) -> Result<()> {
        self.execute_batch("COMMIT").await
    }

    async fn rollback(&mut self) -> Result<()> {
        self.execute_batch("ROLLBACK").await
    }

    async fn savepoint(&mut self, name: &str) -> Result<()> {
        // DuckDB does not yet implement SAVEPOINT (as of 1.1); surface
        // that rather than silently failing on the BEGIN substitute.
        let _ = name;
        Err(Error::unsupported("savepoints (DuckDB)"))
    }

    async fn release_savepoint(&mut self, name: &str) -> Result<()> {
        let _ = name;
        Err(Error::unsupported("savepoints (DuckDB)"))
    }

    async fn rollback_to_savepoint(&mut self, name: &str) -> Result<()> {
        let _ = name;
        Err(Error::unsupported("savepoints (DuckDB)"))
    }

    async fn list_schemas(&mut self) -> Result<Vec<Schema>> {
        // Filter out the system schemas; DuckDB exposes pg_catalog and a
        // handful of others that aren't relevant for browsing user data.
        const SQL: &str = "
            SELECT schema_name
              FROM information_schema.schemata
             WHERE schema_name NOT IN ('information_schema', 'pg_catalog')
             ORDER BY schema_name";
        let result = self.run(SQL, &[]).await?;
        let mut out = Vec::with_capacity(result.rows.len());
        for row in result.rows {
            if let Some(Value::String(name)) = row.0.into_iter().next() {
                out.push(Schema { name });
            }
        }
        Ok(out)
    }

    async fn list_tables(&mut self, schema: &str) -> Result<Vec<Table>> {
        const SQL: &str = "
            SELECT table_name, table_type
              FROM information_schema.tables
             WHERE table_schema = ?
             ORDER BY table_name";
        let result = self.run(SQL, &[Value::String(schema.to_owned())]).await?;
        let mut out = Vec::with_capacity(result.rows.len());
        for row in result.rows {
            let mut iter = row.0.into_iter();
            let name = match iter.next() {
                Some(Value::String(s)) => s,
                _ => continue,
            };
            let kind = match iter.next() {
                Some(Value::String(s)) if s.eq_ignore_ascii_case("VIEW") => TableKind::View,
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
        // Pull column metadata from information_schema.
        const COL_SQL: &str = "
            SELECT column_name, data_type, is_nullable, column_default
              FROM information_schema.columns
             WHERE table_schema = ? AND table_name = ?
             ORDER BY ordinal_position";
        let cols = self
            .run(
                COL_SQL,
                &[
                    Value::String(schema.to_owned()),
                    Value::String(name.to_owned()),
                ],
            )
            .await?;
        if cols.rows.is_empty() {
            return Err(Error::Schema(format!("table {schema}.{name} not found")));
        }

        // Primary keys: information_schema.key_column_usage joined to
        // table_constraints.
        const PK_SQL: &str = "
            SELECT kcu.column_name
              FROM information_schema.table_constraints tc
              JOIN information_schema.key_column_usage kcu
                ON tc.constraint_name = kcu.constraint_name
               AND tc.table_schema    = kcu.table_schema
               AND tc.table_name      = kcu.table_name
             WHERE tc.constraint_type = 'PRIMARY KEY'
               AND tc.table_schema = ? AND tc.table_name = ?";
        let pk = self
            .run(
                PK_SQL,
                &[
                    Value::String(schema.to_owned()),
                    Value::String(name.to_owned()),
                ],
            )
            .await
            .ok();
        let pk_set: std::collections::HashSet<String> = pk
            .map(|r| {
                r.rows
                    .into_iter()
                    .filter_map(|row| match row.0.into_iter().next() {
                        Some(Value::String(s)) => Some(s),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let columns: Vec<Column> = cols
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
                    _ => String::new(),
                };
                let nullable = match iter.next()? {
                    Value::String(s) => s.eq_ignore_ascii_case("YES"),
                    Value::Bool(b) => b,
                    _ => true,
                };
                let default = match iter.next()? {
                    Value::String(s) => Some(s),
                    Value::Null => None,
                    other => Some(other.render()),
                };
                let primary_key = pk_set.contains(&col_name);
                Some(Column {
                    name: col_name,
                    data_type,
                    nullable,
                    primary_key,
                    default,
                })
            })
            .collect();

        let indexes = describe_indexes(self, schema, name)
            .await
            .unwrap_or_default();
        let foreign_keys = describe_foreign_keys(self, schema, name)
            .await
            .unwrap_or_default();
        let unique_constraints = indexes
            .iter()
            .filter(|i| i.unique && !i.primary && i.columns.len() > 1)
            .map(|i| UniqueConstraint {
                name: i.name.clone(),
                columns: i.columns.clone(),
            })
            .collect();

        Ok(TableSchema {
            table: Table {
                schema: schema.to_owned(),
                name: name.to_owned(),
                kind: TableKind::Table,
            },
            columns,
            indexes,
            foreign_keys,
            unique_constraints,
        })
    }

    async fn ping(&mut self) -> Result<()> {
        self.execute_batch("SELECT 1").await
    }

    fn cancel_handle(&self) -> Option<Box<dyn CancelHandle>> {
        Some(Box::new(DuckdbCancel {
            handle: self.interrupt.clone(),
        }))
    }

    fn capabilities(&self) -> Capabilities {
        DuckdbDriver::capabilities()
    }

    async fn close(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}

// ---- discovery helpers ----

/// Look up indexes for `schema.name` via DuckDB's `duckdb_indexes()`
/// table function. The function exposes the SQL that built the index but
/// not its column list directly, so we parse the trailing `(col, col)`
/// out of the rendered statement — good enough for the common cases.
async fn describe_indexes(conn: &DuckdbConnection, schema: &str, name: &str) -> Result<Vec<Index>> {
    const SQL: &str = "
        SELECT index_name, is_unique, is_primary, sql
          FROM duckdb_indexes()
         WHERE schema_name = ? AND table_name = ?";
    let result = conn
        .run(
            SQL,
            &[
                Value::String(schema.to_owned()),
                Value::String(name.to_owned()),
            ],
        )
        .await?;
    let mut out = Vec::with_capacity(result.rows.len());
    for row in result.rows {
        let mut iter = row.0.into_iter();
        let index_name = match iter.next() {
            Some(Value::String(s)) => s,
            _ => continue,
        };
        let unique = matches!(iter.next(), Some(Value::Bool(true) | Value::Int(1)));
        let primary = matches!(iter.next(), Some(Value::Bool(true) | Value::Int(1)));
        let columns = match iter.next() {
            Some(Value::String(sql)) => parse_index_columns(&sql),
            _ => Vec::new(),
        };
        out.push(Index {
            name: index_name,
            columns,
            unique,
            primary,
        });
    }
    Ok(out)
}

/// Best-effort: pull the comma-separated identifier list inside the *last*
/// parenthesised group of a `CREATE INDEX … ON t (a, b)` statement.
fn parse_index_columns(sql: &str) -> Vec<String> {
    let Some(open) = sql.rfind('(') else {
        return Vec::new();
    };
    let Some(close) = sql.rfind(')') else {
        return Vec::new();
    };
    if close <= open + 1 {
        return Vec::new();
    }
    sql[open + 1..close]
        .split(',')
        .map(|part| part.trim().trim_matches('"').trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

async fn describe_foreign_keys(
    conn: &DuckdbConnection,
    schema: &str,
    name: &str,
) -> Result<Vec<ForeignKey>> {
    const SQL: &str = "
        SELECT
            rc.constraint_name      AS name,
            kcu.column_name         AS from_column,
            kcu.referenced_table_schema AS ref_schema,
            kcu.referenced_table_name   AS ref_table,
            kcu.referenced_column_name  AS to_column,
            rc.update_rule              AS on_update,
            rc.delete_rule              AS on_delete
          FROM information_schema.referential_constraints rc
          JOIN information_schema.key_column_usage kcu
            ON rc.constraint_name = kcu.constraint_name
         WHERE kcu.table_schema = ? AND kcu.table_name = ?
         ORDER BY rc.constraint_name, kcu.ordinal_position";
    let result = conn
        .run(
            SQL,
            &[
                Value::String(schema.to_owned()),
                Value::String(name.to_owned()),
            ],
        )
        .await?;
    let mut by_name: std::collections::BTreeMap<String, ForeignKey> =
        std::collections::BTreeMap::new();
    for row in result.rows {
        let v = row.0;
        let fk_name = match v.first() {
            Some(Value::String(s)) => s.clone(),
            _ => continue,
        };
        let from = match v.get(1) {
            Some(Value::String(s)) => s.clone(),
            _ => continue,
        };
        let ref_schema = match v.get(2) {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        };
        let ref_table = match v.get(3) {
            Some(Value::String(s)) => s.clone(),
            _ => continue,
        };
        let to = match v.get(4) {
            Some(Value::String(s)) => s.clone(),
            _ => continue,
        };
        let on_update = v.get(5).and_then(|x| match x {
            Value::String(s) => ReferentialAction::from_engine_token(s),
            _ => None,
        });
        let on_delete = v.get(6).and_then(|x| match x {
            Value::String(s) => ReferentialAction::from_engine_token(s),
            _ => None,
        });
        let entry = by_name
            .entry(fk_name.clone())
            .or_insert_with(|| ForeignKey {
                name: fk_name,
                columns: Vec::new(),
                referenced_schema: ref_schema,
                referenced_table: ref_table,
                referenced_columns: Vec::new(),
                on_update,
                on_delete,
            });
        entry.columns.push(from);
        entry.referenced_columns.push(to);
    }
    Ok(by_name.into_values().collect())
}

// Keep the helper available so dead-code lints don't fire when the
// describe path skips it.
#[allow(dead_code)]
async fn quote_and_run(conn: &DuckdbConnection, schema: &str, name: &str) -> Result<()> {
    let q = format!(
        "SELECT 1 FROM {}.{}",
        quote_ident(schema),
        quote_ident(name)
    );
    conn.execute_owned(q).await
}

// ---- streaming + cancellation ----

struct DuckdbRowStream {
    columns: Vec<ColumnHeader>,
    rx: mpsc::Receiver<Result<CoreRow>>,
}

#[async_trait]
impl RowStream for DuckdbRowStream {
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

struct DuckdbCancel {
    handle: Arc<duckdb::InterruptHandle>,
}

#[async_trait]
impl CancelHandle for DuckdbCancel {
    async fn cancel(&self) -> Result<()> {
        self.handle.interrupt();
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
            driver: DuckdbDriver::NAME.into(),
            params: ConnectionParams {
                path: Some(":memory:".into()),
                ..Default::default()
            },
        }
    }

    async fn open() -> Box<dyn Connection> {
        DuckdbDriver::new()
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
        conn.execute(
            "INSERT INTO users (id, name) VALUES (?, ?)",
            &[Value::Int(1), Value::String("berkant".into())],
        )
        .await
        .unwrap();
        let select = conn
            .execute("SELECT name FROM users WHERE id = ?", &[Value::Int(1)])
            .await
            .unwrap();
        assert_eq!(select.rows.len(), 1);
        assert_eq!(
            select.rows[0].get(0).map(Value::render),
            Some("berkant".into())
        );
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
            conn.execute("INSERT INTO nums VALUES (?)", &[Value::Int(i)])
                .await
                .unwrap();
        }
        let mut stream = conn
            .stream("SELECT n FROM nums ORDER BY n", &[])
            .await
            .unwrap();
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
            "CREATE TABLE items (id INTEGER PRIMARY KEY, title TEXT NOT NULL, price DOUBLE)",
            &[],
        )
        .await
        .unwrap();
        let schemas = conn.list_schemas().await.unwrap();
        // DuckDB always reports the `main` schema for the default catalog.
        assert!(schemas.iter().any(|s| s.name == "main"));

        let tables = conn.list_tables("main").await.unwrap();
        assert!(tables.iter().any(|t| t.name == "items"));

        let schema = conn.describe_table("main", "items").await.unwrap();
        assert_eq!(schema.columns.len(), 3);
        assert_eq!(schema.columns[0].name, "id");
        assert!(schema.columns[0].primary_key);
        assert!(!schema.columns[1].nullable);
    }

    #[test]
    fn parse_index_columns_handles_quoted_and_plain() {
        assert_eq!(
            super::parse_index_columns("CREATE INDEX i ON t (\"a\", \"b\")"),
            vec!["a", "b"]
        );
        assert_eq!(
            super::parse_index_columns("CREATE INDEX i ON t(a)"),
            vec!["a"]
        );
        assert!(super::parse_index_columns("not really sql").is_empty());
    }
}
