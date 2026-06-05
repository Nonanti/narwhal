//! `SQLite` driver backed by `rusqlite`.
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

use narwhal_core::{
    Capabilities, Column, ColumnHeader, Connection, ConnectionConfig, DatabaseDriver, Error,
    ForeignKey, Index, IsolationLevel, QueryResult, ReferentialAction, Result, Row as CoreRow,
    RowStream, Schema, Table, TableKind, TableSchema, UniqueConstraint, Value,
};
use rusqlite::params_from_iter;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task;
use tracing::{debug, info};

use self::types::{value_from_ref, value_to_sql};

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[derive(Debug, Default)]
pub struct SqliteDriver;

impl SqliteDriver {
    pub const NAME: &'static str = "sqlite";

    pub const fn new() -> Self {
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
            // rusqlite::Statement::query yields rows lazily from the
            // SQLite VM step-by-step, so the stream is genuine.
            .with_streaming(true)
            .with_row_level_dml(true)
    }
}

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
    ) -> Result<Box<dyn narwhal_core::DynConnection>> {
        let path = config
            .params
            .path
            .as_deref()
            .ok_or_else(|| Error::Config("path missing".into()))?
            .to_owned();
        let path_buf = PathBuf::from(&path);

        // L11: log the absolute, canonicalised path when possible so a
        // mistyped relative path or symlink mismatch is obvious from the
        // log. We still pass the original `path_buf` to rusqlite so the
        // user's exact spelling is what hits sqlite3_open (canonicalize
        // would fail for in-memory `:memory:` and for not-yet-existing
        // files we intend to create).
        let canonical = std::fs::canonicalize(&path_buf)
            .ok()
            .map(|p| p.display().to_string());
        debug!(
            target: "narwhal::sqlite",
            path = %path,
            canonical = canonical.as_deref().unwrap_or("<unresolved>"),
            "opening database"
        );
        let conn = task::spawn_blocking(move || rusqlite::Connection::open(path_buf))
            .await
            .map_err(|e| Error::connection_with("sqlite spawn_blocking join", e))?
            .map_err(|e| Error::connection_with("sqlite open", e))?;

        info!(
            target: "narwhal::sqlite",
            path = %path,
            canonical = canonical.as_deref().unwrap_or("<unresolved>"),
            "database opened"
        );
        Ok(Box::new(SqliteConnection {
            inner: Arc::new(Mutex::new(Some(conn))),
        }))
    }
}

pub struct SqliteConnection {
    /// M2.2: wrapped in `Option` so `close()` can `guard.take()` the
    /// `rusqlite::Connection`, which drops it and releases any file
    /// locks. All access sites check for `None` and return
    /// `Error::Connection("connection closed")`.
    inner: Arc<Mutex<Option<rusqlite::Connection>>>,
}

impl SqliteConnection {
    /// Look up the table kind from `sqlite_master.type` (M11).
    async fn lookup_table_kind(&self, name: &str) -> Result<TableKind> {
        let sql = "SELECT type FROM sqlite_master WHERE name = ? AND type IN ('table', 'view')";
        let result = self.run(sql, &[Value::String(name.to_owned())]).await?;
        match result.rows.into_iter().next() {
            Some(row) => match row.0.first() {
                Some(Value::String(t)) if t == "view" => Ok(TableKind::View),
                _ => Ok(TableKind::Table),
            },
            None => Ok(TableKind::Table),
        }
    }

    /// Enumerate every index on `escaped_table_name` (which must already have
    /// embedded quote characters doubled).
    async fn list_indexes(&self, escaped_table_name: &str) -> Result<Vec<Index>> {
        let list_sql = format!("PRAGMA index_list(\"{escaped_table_name}\")");
        let list = self.run(&list_sql, &[]).await?;
        let mut indexes = Vec::with_capacity(list.rows.len());
        for row in list.rows {
            let mut iter = row.0.into_iter();
            let _seq = iter.next();
            let name = match iter.next() {
                Some(Value::String(s)) => s,
                _ => continue,
            };
            let unique = matches!(iter.next(), Some(Value::Int(1)));
            let origin = match iter.next() {
                Some(Value::String(s)) => s,
                _ => String::new(),
            };
            let escaped_idx = name.replace('"', "\"\"");
            let info_sql = format!("PRAGMA index_info(\"{escaped_idx}\")");
            let info = self.run(&info_sql, &[]).await?;
            let columns: Vec<String> = info
                .rows
                .into_iter()
                .filter_map(|r| match r.0.get(2) {
                    Some(Value::String(s)) => Some(s.clone()),
                    _ => None,
                })
                .collect();
            indexes.push(Index {
                name,
                columns,
                unique,
                primary: origin == "pk",
            });
        }
        Ok(indexes)
    }

    async fn list_foreign_keys(&self, escaped_table_name: &str) -> Result<Vec<ForeignKey>> {
        let sql = format!("PRAGMA foreign_key_list(\"{escaped_table_name}\")");
        let result = self.run(&sql, &[]).await?;
        // Rows are: id, seq, table, from, to, on_update, on_delete, match.
        // Composite foreign keys share an `id`; we group them.
        let mut by_id: std::collections::BTreeMap<i64, ForeignKey> =
            std::collections::BTreeMap::new();
        for row in result.rows {
            let v = row.0;
            let id = match v.first() {
                Some(Value::Int(i)) => *i,
                _ => continue,
            };
            let ref_table = match v.get(2) {
                Some(Value::String(s)) => s.clone(),
                _ => continue,
            };
            let from = match v.get(3) {
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
            let entry = by_id.entry(id).or_insert_with(|| ForeignKey {
                name: format!("fk_{id}"),
                columns: Vec::new(),
                referenced_schema: None,
                referenced_table: ref_table.clone(),
                referenced_columns: Vec::new(),
                on_update,
                on_delete,
            });
            entry.columns.push(from);
            entry.referenced_columns.push(to);
        }
        Ok(by_id.into_values().collect())
    }

    async fn run(&self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        let inner = self.inner.clone();
        let sql = sql.to_owned();
        let params: Vec<rusqlite::types::Value> = params.iter().map(value_to_sql).collect();

        task::spawn_blocking(move || run_blocking(&inner, &sql, params))
            .await
            .map_err(|e| Error::connection_with("sqlite spawn_blocking join", e))?
    }

    async fn execute_batch(&self, sql: &'static str) -> Result<()> {
        let inner = self.inner.clone();
        task::spawn_blocking(move || {
            let mut guard = inner.blocking_lock();
            let conn = guard
                .as_mut()
                .ok_or_else(|| Error::Connection("connection closed".into()))?;
            conn.execute_batch(sql)
                .map_err(|e| Error::query_with("sqlite execute_batch", e))
        })
        .await
        .map_err(|e| Error::connection_with("sqlite spawn_blocking join", e))?
    }
}

fn run_blocking(
    inner: &Arc<Mutex<Option<rusqlite::Connection>>>,
    sql: &str,
    params: Vec<rusqlite::types::Value>,
) -> Result<QueryResult> {
    let started = Instant::now();
    let guard = inner.blocking_lock();
    let conn = guard
        .as_ref()
        .ok_or_else(|| Error::Connection("connection closed".into()))?;
    let mut statement = conn
        .prepare(sql)
        .map_err(|e| Error::query_with("sqlite prepare", e))?;

    let column_count = statement.column_count();
    if column_count == 0 {
        let affected = statement
            .execute(params_from_iter(params.iter()))
            .map_err(|e| Error::query_with("sqlite execute", e))?;
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
        .map_err(|e| Error::query_with("sqlite query", e))?;

    let mut collected = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|e| Error::query_with("sqlite fetch", e))?
    {
        let mut values = Vec::with_capacity(column_count);
        for idx in 0..column_count {
            let v = row
                .get_ref(idx)
                .map_err(|e| Error::query_with("sqlite get_ref", e))?;
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

impl Connection for SqliteConnection {
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        self.run(sql, params).await
    }

    async fn stream(
        &mut self,
        sql: &str,
        params: &[Value],
    ) -> Result<Box<dyn narwhal_core::DynRowStream>> {
        let inner = self.inner.clone();
        let sql = sql.to_owned();
        let bound: Vec<rusqlite::types::Value> = params.iter().map(value_to_sql).collect();
        let (header_tx, header_rx) = oneshot::channel::<Result<Vec<ColumnHeader>>>();
        let (row_tx, row_rx) = mpsc::channel::<Result<CoreRow>>(64);

        task::spawn_blocking(move || {
            let guard = inner.blocking_lock();
            let conn = if let Some(c) = guard.as_ref() {
                c
            } else {
                let _ = header_tx.send(Err(Error::Connection("connection closed".into())));
                return;
            };
            let mut statement = match conn.prepare(&sql) {
                Ok(stmt) => stmt,
                Err(error) => {
                    let _ = header_tx.send(Err(Error::query_with("sqlite stream prepare", error)));
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
                    let _ =
                        row_tx.blocking_send(Err(Error::query_with("sqlite stream query", error)));
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
                                    failure =
                                        Some(Error::query_with("sqlite stream get_ref", error));
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
                        let _ = row_tx
                            .blocking_send(Err(Error::query_with("sqlite stream fetch", error)));
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
            // Unknown future isolation levels: fall back to the strictest mode
            // that SQLite can actually honour.
            _ => self.execute_batch("BEGIN EXCLUSIVE").await,
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

    async fn list_all_tables(&mut self) -> Result<Vec<(Schema, Vec<Table>)>> {
        const SQL: &str = "
            SELECT name, type
            FROM sqlite_master
            WHERE type IN ('table', 'view')
              AND name NOT LIKE 'sqlite_%'
            ORDER BY name";
        let result = self.run(SQL, &[]).await?;

        let mut tables = Vec::with_capacity(result.rows.len());
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
            tables.push(Table {
                schema: "main".into(),
                name,
                kind,
            });
        }
        Ok(vec![(
            Schema {
                name: "main".into(),
            },
            tables,
        )])
    }

    async fn describe_table(&mut self, _schema: &str, name: &str) -> Result<TableSchema> {
        // `PRAGMA table_info` does not accept bound parameters; quote the
        // identifier so embedded special characters do not break the
        // statement.
        let escaped = name.replace('"', "\"\"");
        let info_sql = format!("PRAGMA table_info(\"{escaped}\")");
        let info = self.run(&info_sql, &[]).await?;

        if info.rows.is_empty() {
            return Err(Error::Schema(format!("table {name} not found")));
        }

        let columns: Vec<Column> = info
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

        // Look up the table kind from sqlite_master (M11).
        let kind = self.lookup_table_kind(name).await?;

        let indexes = self.list_indexes(&escaped).await.unwrap_or_default();
        let foreign_keys = self.list_foreign_keys(&escaped).await.unwrap_or_default();
        // Sprint 6 (M16): align with the MySQL driver's policy —
        // every UNIQUE index that isn't the implicit PRIMARY is a
        // surfaced constraint, including single-column ones. The old
        // `columns.len() > 1` filter dropped single-column UNIQUEs and
        // produced a different table-schema shape across drivers,
        // which surprised cross-driver tooling.
        let unique_constraints = indexes
            .iter()
            .filter(|i| i.unique && !i.primary)
            .map(|i| UniqueConstraint {
                name: i.name.clone(),
                columns: i.columns.clone(),
            })
            .collect();

        Ok(TableSchema {
            table: Table {
                schema: "main".into(),
                name: name.to_owned(),
                kind,
            },
            columns,
            indexes,
            foreign_keys,
            unique_constraints,
        })
    }

    async fn fetch_ddl(&mut self, _schema: &str, name: &str) -> Result<String> {
        let sql = "SELECT sql FROM sqlite_master WHERE type IN ('table', 'view') AND name = ?";
        let result = self.run(sql, &[Value::String(name.to_owned())]).await?;
        match result
            .rows
            .into_iter()
            .next()
            .and_then(|r| r.0.into_iter().next())
        {
            Some(Value::String(ddl)) => Ok(ddl),
            _ => Err(Error::Schema(format!("DDL not found for {name}"))),
        }
    }

    async fn ping(&mut self) -> Result<()> {
        self.execute_batch("SELECT 1").await
    }

    /// `PrAGMA query_only` is per-connection in `SQLite` and survives
    /// until explicitly cleared. Toggling to `false` re-enables writes;
    /// callers driving the global `--read-only` flag should never
    /// re-enable.
    async fn set_read_only(&mut self, read_only: bool) -> Result<()> {
        let pragma = if read_only {
            "PRAGMA query_only = ON"
        } else {
            "PRAGMA query_only = OFF"
        };
        self.execute_batch(pragma).await
    }

    fn cancel_handle(&self) -> Option<Box<dyn narwhal_core::DynCancelHandle>> {
        None
    }

    fn capabilities(&self) -> Capabilities {
        SqliteDriver::capabilities()
    }

    // M2.2: explicitly drop the connection handle via guard.take() so
    // the SQLite file lock is released immediately. After this call
    // all subsequent operations return Error::Connection("connection closed").
    async fn close(self: Box<Self>) -> Result<()> {
        let mut guard = self.inner.lock().await;
        guard.take();
        Ok(())
    }
}

struct SqliteRowStream {
    columns: Vec<ColumnHeader>,
    rx: mpsc::Receiver<Result<CoreRow>>,
}

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
            params: ConnectionParams::with(|p| {
                p.path = Some(":memory:".into());
            }),
        }
    }

    async fn open() -> Box<dyn narwhal_core::DynConnection> {
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

    #[tokio::test]
    async fn describe_table_reports_indexes_and_foreign_keys() {
        let mut conn = open().await;
        conn.execute(
            "CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE)",
            &[],
        )
        .await
        .unwrap();
        conn.execute(
            "CREATE TABLE orders (
                id INTEGER PRIMARY KEY,
                customer_id INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
                placed_at TEXT NOT NULL
            )",
            &[],
        )
        .await
        .unwrap();
        conn.execute(
            "CREATE INDEX idx_orders_placed_at ON orders(placed_at)",
            &[],
        )
        .await
        .unwrap();
        conn.execute(
            "CREATE UNIQUE INDEX idx_orders_unique ON orders(customer_id, placed_at)",
            &[],
        )
        .await
        .unwrap();

        let schema = conn.describe_table("main", "orders").await.unwrap();
        assert!(
            schema
                .indexes
                .iter()
                .any(|i| i.name == "idx_orders_placed_at" && !i.unique)
        );
        assert!(
            schema
                .indexes
                .iter()
                .any(|i| i.name == "idx_orders_unique" && i.unique)
        );

        assert_eq!(schema.foreign_keys.len(), 1);
        let fk = &schema.foreign_keys[0];
        assert_eq!(fk.columns, vec!["customer_id"]);
        assert_eq!(fk.referenced_table, "customers");
        assert_eq!(fk.referenced_columns, vec!["id"]);
        assert_eq!(fk.on_delete, Some(narwhal_core::ReferentialAction::Cascade));

        // composite unique index appears as a multi-column unique constraint
        assert!(
            schema
                .unique_constraints
                .iter()
                .any(|u| u.columns == vec!["customer_id", "placed_at"])
        );
    }

    /// M2.2: `close()` must drop the `rusqlite::Connection` via `guard.take()`,
    /// releasing any file lock held by `SQLite`. A connection that has been
    /// closed must refuse further queries with "connection closed".
    #[tokio::test]
    async fn close_drops_connection_and_releases_file_lock() {
        let tmp = tempfile::Builder::new()
            .suffix(".db")
            .tempdir()
            .expect("tempdir");
        let db_path = tmp.path().join("close_test.db");

        let config = ConnectionConfig {
            id: Uuid::nil(),
            name: "close-test".into(),
            driver: SqliteDriver::NAME.into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(db_path.display().to_string());
            }),
        };

        // Open and close — after close the file lock must be released so
        // a second connection can open the same file.
        let conn = SqliteDriver::new()
            .connect(&config, None)
            .await
            .expect("first open");
        conn.close().await.expect("close");

        // If close() didn't drop the Connection, this second open will
        // fail with a lock error on some platforms.
        let conn2 = SqliteDriver::new()
            .connect(&config, None)
            .await
            .expect("reopen after close");
        conn2.close().await.expect("close2");
    }

    /// M2.2: after `close()`, any query must return
    /// `Error::Connection("connection closed")` rather than panicking.
    ///
    /// Because `close(self: Box<Self>)` consumes the connection, we
    /// simulate the post-close state by constructing a
    /// `SqliteConnection` whose inner has already been taken (set to
    /// `None`), matching the `DuckDB` driver's test pattern.
    #[tokio::test]
    async fn close_then_query_returns_connection_closed_error() {
        let conn = SqliteConnection {
            inner: Arc::new(Mutex::new(None::<rusqlite::Connection>)),
        };

        // run() and execute_batch() must both surface "connection closed".
        let err = conn.run("SELECT 1", &[]).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("connection closed"),
            "expected 'connection closed' in error, got: {msg}"
        );

        let err = conn.execute_batch("SELECT 1").await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("connection closed"),
            "expected 'connection closed' in error, got: {msg}"
        );
    }
}
