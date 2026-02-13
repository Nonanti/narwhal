//! `DuckDB` driver backed by the `duckdb` crate (an embedded OLAP engine
//! whose Rust API is a near-fork of `rusqlite`).
//!
//! Like `SQLite`, every call is synchronous so we dispatch the work onto
//! [`tokio::task::spawn_blocking`] and serialise concurrent use behind a
//! [`tokio::sync::Mutex`]. Unlike `SQLite`, `DuckDB`:
//!
//! * supports multiple logical schemas, surfaced via `information_schema`;
//! * has a richer type lattice (huge ints, decimals, intervals, lists,
//!   structs, maps, unions). The internal `types` module keeps the lossy
//!   mapping in one place so the rest of the code stays simple;
//! * supports query cancellation through [`duckdb::InterruptHandle`].
//!
//! The intent is parity with the `SQLite` driver's surface area, so the
//! result set, schema discovery and transaction surface are uniform. DDL
//! and PRAGMA invocations are different (`DuckDB` uses `information_schema`
//! and a couple of `pragma_*` table-valued functions), so the underlying
//! SQL is bespoke even when the wire shape matches.

#![forbid(unsafe_code)]

mod types;

#[doc(hidden)]
pub mod __test_only {
    //! Private helpers exposed for integration tests only. Not part of the
    //! public API; do not depend on this module outside the crate's own
    //! `tests/` directory.
    pub fn has_returning_clause(sql: &str) -> bool {
        super::has_returning_clause(sql)
    }
}

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
use tracing::{debug, info, warn};

use crate::types::{value_from_ref, value_to_sql};

#[derive(Debug, Default)]
pub struct DuckdbDriver;

impl DuckdbDriver {
    pub const NAME: &'static str = "duckdb";

    pub const fn new() -> Self {
        Self
    }

    fn capabilities() -> Capabilities {
        Capabilities::default()
            .with_transactions(true)
            // DuckDB has InterruptHandle; we wire it up below.
            .with_cancellation(true)
            .with_multiple_schemas(true)
            .with_prepared_statements(true)
            .with_savepoints(false)
            .with_rows_affected(true)
            // DuckDB returns Arrow chunks lazily through PreparedStatement::query.
            .with_streaming(true)
            .with_row_level_dml(true)
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

        // L11: log canonical path so a mistyped relative path is obvious.
        let canonical = if path == ":memory:" {
            None
        } else {
            std::fs::canonicalize(&path)
                .ok()
                .map(|p| p.display().to_string())
        };
        debug!(
            target: "narwhal::duckdb",
            path = %path,
            canonical = canonical.as_deref().unwrap_or("<unresolved>"),
            "opening database"
        );
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

        info!(
            target: "narwhal::duckdb",
            canonical = canonical.as_deref().unwrap_or("<unresolved>"),
            "database opened"
        );
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
    /// Look up the table kind (Table/View) from `duckdb_views` and `duckdb_tables`.
    async fn lookup_table_kind(&self, schema: &str, name: &str) -> Result<TableKind> {
        const SQL: &str = "
            SELECT 'view' AS kind FROM duckdb_views() WHERE schema_name = ? AND view_name = ?
            UNION ALL
            SELECT 'table' AS kind FROM duckdb_tables() WHERE schema_name = ? AND table_name = ?
            LIMIT 1";
        let s = Value::String(schema.to_owned());
        let n = Value::String(name.to_owned());
        let result = self.run(SQL, &[s.clone(), n.clone(), s, n]).await?;
        match result.rows.into_iter().next() {
            Some(row) => match row.0.first() {
                Some(Value::String(k)) if k.eq_ignore_ascii_case("view") => Ok(TableKind::View),
                _ => Ok(TableKind::Table),
            },
            None => Ok(TableKind::Table),
        }
    }

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
}

/// Best-effort: does `sql` likely return a result set?
///
/// `DuckDB`'s prepared-statement API requires us to commit to either
/// [`Statement::execute`] or [`Statement::query`] *before* it knows the
/// statement shape, so we infer from:
///
/// 1. the leading keyword — SELECT/WITH/SHOW/DESCRIBE/EXPLAIN/VALUES/
///    FROM/PRAGMA/TABLE/SUMMARIZE all return rows;
/// 2. the presence of a `RETURNING` clause on DML statements —
///    `INSERT … RETURNING`, `UPDATE … RETURNING`, `DELETE … RETURNING`,
///    `MERGE … RETURNING` all stream rows back. Before this we were
///    silently swallowing those rows and only reporting rows-affected.
///
/// Statements that match neither fall through to `execute`.
fn statement_returns_rows(sql: &str) -> bool {
    let lead = sql
        .trim_start()
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    let lead_returns = matches!(
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
    );
    if lead_returns {
        return true;
    }
    matches!(
        lead.as_str(),
        "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "REPLACE"
    ) && has_returning_clause(sql)
}

/// Case-insensitive search for a `RETURNING` keyword outside of any
/// single- or double-quoted string literal. Word-boundary aware so an
/// identifier like `customer_returning` doesn't trigger a false positive.
pub(crate) fn has_returning_clause(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if c == q {
                // Doubled quote = escaped literal quote.
                if i + 1 < bytes.len() && bytes[i + 1] == q {
                    i += 2;
                    continue;
                }
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == b'\'' || c == b'"' {
            quote = Some(c);
            i += 1;
            continue;
        }
        if (c == b'R' || c == b'r')
            && bytes.len() - i >= 9
            && bytes[i..i + 9].eq_ignore_ascii_case(b"RETURNING")
            && is_word_boundary(bytes, i, i + 9)
        {
            return true;
        }
        i += 1;
    }
    false
}

fn is_word_boundary(bytes: &[u8], start: usize, end: usize) -> bool {
    let before =
        start == 0 || !bytes[start - 1].is_ascii_alphanumeric() && bytes[start - 1] != b'_';
    let after = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric() && bytes[end] != b'_';
    before && after
}

/// Render a [`duckdb::types::Type`] as the human-readable SQL type name
/// matching `DuckDB`'s documentation. The default `Debug` formatting
/// produces variant names like `Int` rather than the engine's own
/// `INTEGER`; composite types are rendered recursively. Keeps the
/// result-pane legend on parity with the other drivers.
fn format_column_type(ty: &duckdb::types::Type) -> String {
    use duckdb::types::Type;
    match ty {
        Type::Null => "NULL".into(),
        Type::Boolean => "BOOLEAN".into(),
        Type::TinyInt => "TINYINT".into(),
        Type::SmallInt => "SMALLINT".into(),
        Type::Int => "INTEGER".into(),
        Type::BigInt => "BIGINT".into(),
        Type::HugeInt => "HUGEINT".into(),
        Type::UTinyInt => "UTINYINT".into(),
        Type::USmallInt => "USMALLINT".into(),
        Type::UInt => "UINTEGER".into(),
        Type::UBigInt => "UBIGINT".into(),
        Type::Float => "FLOAT".into(),
        Type::Double => "DOUBLE".into(),
        Type::Decimal => "DECIMAL".into(),
        Type::Timestamp => "TIMESTAMP".into(),
        Type::Text => "VARCHAR".into(),
        Type::Blob => "BLOB".into(),
        Type::Date32 => "DATE".into(),
        Type::Time64 => "TIME".into(),
        Type::Interval => "INTERVAL".into(),
        Type::Enum => "ENUM".into(),
        Type::Union => "UNION".into(),
        Type::Any => "ANY".into(),
        Type::List(inner) => format!("LIST({})", format_column_type(inner)),
        Type::Array(inner, size) => format!("{}[{size}]", format_column_type(inner)),
        Type::Map(k, v) => format!("MAP({}, {})", format_column_type(k), format_column_type(v)),
        Type::Struct(fields) => {
            let parts: Vec<String> = fields
                .iter()
                .map(|(n, t)| format!("{n} {}", format_column_type(t)))
                .collect();
            format!("STRUCT({})", parts.join(", "))
        }
    }
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
                        .map_or("", std::string::String::as_str)
                        .to_owned(),
                    data_type: format_column_type(&duckdb::types::Type::from(
                        &stmt.column_type(idx),
                    )),
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
                                .map_or("", std::string::String::as_str)
                                .to_owned(),
                            data_type: format_column_type(&duckdb::types::Type::from(
                                &stmt.column_type(idx),
                            )),
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

    async fn list_all_tables(&mut self) -> Result<Vec<(Schema, Vec<Table>)>> {
        const SQL: &str = "
            SELECT table_schema, table_name, table_type
              FROM information_schema.tables
             WHERE table_schema NOT IN ('information_schema', 'pg_catalog')
             ORDER BY table_schema, table_name";
        let result = self.run(SQL, &[]).await?;

        let mut map: std::collections::BTreeMap<String, Vec<Table>> =
            std::collections::BTreeMap::new();
        for row in result.rows {
            let mut iter = row.0.into_iter();
            let schema = match iter.next() {
                Some(Value::String(s)) => s,
                _ => continue,
            };
            let name = match iter.next() {
                Some(Value::String(s)) => s,
                _ => continue,
            };
            let kind = match iter.next() {
                Some(Value::String(s)) if s.eq_ignore_ascii_case("VIEW") => TableKind::View,
                _ => TableKind::Table,
            };
            map.entry(schema.clone()).or_default().push(Table {
                schema: schema.clone(),
                name,
                kind,
            });
        }

        // Preserve the order of schemas from list_schemas.
        let schemas = self.list_schemas().await?;
        let mut out = Vec::with_capacity(schemas.len());
        for schema in schemas {
            let tables = map.remove(&schema.name).unwrap_or_default();
            out.push((schema, tables));
        }
        for (name, tables) in map {
            out.push((Schema { name }, tables));
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
        // information_schema may be unavailable on some DuckDB builds;
        // log the miss instead of returning a silently empty PK set,
        // which makes the UI's missing PK indicator unexplainable.
        let pk = match self
            .run(
                PK_SQL,
                &[
                    Value::String(schema.to_owned()),
                    Value::String(name.to_owned()),
                ],
            )
            .await
        {
            Ok(r) => Some(r),
            Err(error) => {
                tracing::warn!(
                    target: "narwhal::duckdb",
                    schema, table = name, error = %error,
                    "primary-key lookup failed; continuing without"
                );
                None
            }
        };
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

        // Look up the table kind from duckdb_views + duckdb_tables (M11).
        let kind = self.lookup_table_kind(schema, name).await?;

        let indexes = match describe_indexes(self, schema, name).await {
            Ok(v) => v,
            Err(error) => {
                warn!(
                    target: "narwhal::duckdb",
                    %schema, %name, %error,
                    "failed to read index metadata; continuing with an empty list"
                );
                Vec::new()
            }
        };
        let foreign_keys = match describe_foreign_keys(self, schema, name).await {
            Ok(v) => v,
            Err(error) => {
                warn!(
                    target: "narwhal::duckdb",
                    %schema, %name, %error,
                    "failed to read foreign-key metadata; continuing with an empty list"
                );
                Vec::new()
            }
        };
        // Sprint 6 (M16): align with the MySQL/SQLite policy — every
        // non-PRIMARY UNIQUE index is surfaced, including single-column
        // ones. The previous `> 1` filter produced inconsistent
        // table-schema shapes across drivers.
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
                schema: schema.to_owned(),
                name: name.to_owned(),
                kind,
            },
            columns,
            indexes,
            foreign_keys,
            unique_constraints,
        })
    }

    async fn fetch_ddl(&mut self, schema: &str, name: &str) -> Result<String> {
        // UNION tables + views; duckdb_views() uses `view_name`, not `table_name`.
        const SQL: &str = "
            SELECT sql FROM duckdb_tables() WHERE schema_name = ? AND table_name = ?
            UNION ALL
            SELECT sql FROM duckdb_views()  WHERE schema_name = ? AND view_name  = ?";
        let s = Value::String(schema.to_owned());
        let n = Value::String(name.to_owned());
        let result = self.run(SQL, &[s.clone(), n.clone(), s, n]).await?;
        match result
            .rows
            .into_iter()
            .next()
            .and_then(|r| r.0.into_iter().next())
        {
            Some(Value::String(ddl)) => Ok(ddl),
            _ => Err(Error::Schema(format!("DDL not found for {schema}.{name}"))),
        }
    }

    async fn ping(&mut self) -> Result<()> {
        self.execute_batch("SELECT 1").await
    }

    /// Issue C (sprint 5): `DuckDB` enforces read-only at open time via
    /// the `access_mode='READ_ONLY'` connection-string option; there is
    /// no session-level toggle equivalent to PG's
    /// `default_transaction_read_only` or `SQLite`'s `PRAGMA query_only`.
    /// Returning a *typed* [`Error::Unsupported`] with a precise hint
    /// lets the MCP context surface the gap loudly (warn-level log) and
    /// gives operators concrete guidance.
    async fn set_read_only(&mut self, read_only: bool) -> Result<()> {
        if read_only {
            Err(Error::unsupported(
                "DuckDB does not expose a session-level read-only flag; \
                 reopen the connection with `access_mode = 'READ_ONLY'` to \
                 enforce write rejection at the engine level",
            ))
        } else {
            // Turning enforcement OFF when it was never ON is a no-op.
            Ok(())
        }
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

/// Look up indexes for `schema.name` via `DuckDB`'s `duckdb_indexes()`
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

/// Pull the comma-separated identifier list out of
/// `CREATE INDEX … ON t (a, b) [WHERE …]`.
///
/// We scan left-to-right and pick the *first* top-level parenthesised
/// group — the column list. The old version used `rfind('(')` which
/// broke on partial indexes like `… ON t (a, b) WHERE (status IS NOT
/// NULL)` (it returned `status IS NOT NULL` as a column).
fn parse_index_columns(sql: &str) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    // Walk to the first unquoted '('.
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if c == q {
                if i + 1 < bytes.len() && bytes[i + 1] == q {
                    i += 2;
                    continue;
                }
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == b'\'' || c == b'"' {
            quote = Some(c);
            i += 1;
            continue;
        }
        if c == b'(' {
            break;
        }
        i += 1;
    }
    if i >= bytes.len() {
        return Vec::new();
    }
    let start = i + 1;
    let mut depth = 1usize;
    i += 1;
    quote = None;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if c == q {
                if i + 1 < bytes.len() && bytes[i + 1] == q {
                    i += 2;
                    continue;
                }
                quote = None;
            }
            i += 1;
            continue;
        }
        match c {
            b'\'' | b'"' => quote = Some(c),
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if i >= bytes.len() || depth != 0 {
        return Vec::new();
    }
    sql[start..i]
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

    #[test]
    fn statement_returns_rows_handles_returning_clauses() {
        // The old version missed all four forms below — user got
        // 'rows affected' instead of the rows.
        assert!(statement_returns_rows(
            "INSERT INTO t (n) VALUES (1) RETURNING n"
        ));
        assert!(statement_returns_rows("  update t set n = 2 returning n  "));
        assert!(statement_returns_rows(
            "DELETE FROM t WHERE n = 1 RETURNING n"
        ));
        // RETURNING-looking text inside a string literal must *not*
        // fool the heuristic.
        assert!(!statement_returns_rows(
            "INSERT INTO t (n) VALUES ('we are returning home')"
        ));
        // Identifier with 'returning' in it is not the keyword.
        assert!(!statement_returns_rows(
            "INSERT INTO customer_returning (n) VALUES (1)"
        ));
        // Plain DML still goes to execute().
        assert!(!statement_returns_rows("INSERT INTO t VALUES (1)"));
        assert!(!statement_returns_rows("UPDATE t SET n = 0"));
        // Sanity: SELECT branch still works.
        assert!(statement_returns_rows("SELECT 1"));
        assert!(statement_returns_rows(
            "  with cte as (select 1) select * from cte"
        ));
    }

    #[test]
    fn format_column_type_renders_engine_names() {
        use duckdb::types::Type;
        assert_eq!(format_column_type(&Type::Int), "INTEGER");
        assert_eq!(format_column_type(&Type::Text), "VARCHAR");
        assert_eq!(format_column_type(&Type::Date32), "DATE");
        assert_eq!(
            format_column_type(&Type::List(Box::new(Type::BigInt))),
            "LIST(BIGINT)"
        );
        assert_eq!(
            format_column_type(&Type::Map(Box::new(Type::Text), Box::new(Type::Int))),
            "MAP(VARCHAR, INTEGER)"
        );
    }

    #[test]
    fn parse_index_columns_handles_partial_indexes() {
        // The old version returned ['status IS NOT NULL'] here.
        assert_eq!(
            parse_index_columns("CREATE INDEX idx ON t (a, b) WHERE (status IS NOT NULL)"),
            vec!["a".to_string(), "b".to_string()]
        );
        // Quoted identifiers stay quoted-aware.
        assert_eq!(
            parse_index_columns("CREATE INDEX idx ON t (\"a b\", c)"),
            vec!["a b".to_string(), "c".to_string()]
        );
        // No parens at all — empty list, no panic.
        assert!(parse_index_columns("").is_empty());
        assert!(parse_index_columns("CREATE INDEX idx ON t a").is_empty());
    }

    #[tokio::test]
    async fn returning_clause_actually_streams_rows() {
        // End-to-end version of the unit test above — prove that a
        // INSERT … RETURNING really does come back with columns and
        // rows, not just rows_affected.
        let mut conn = open().await;
        conn.execute("CREATE TABLE t (id INTEGER, label TEXT)", &[])
            .await
            .unwrap();
        let result = conn
            .execute(
                "INSERT INTO t (id, label) VALUES (1, 'a'), (2, 'b') RETURNING id",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(
            result.columns.len(),
            1,
            "expected one column, got {:?}",
            result.columns
        );
        assert_eq!(result.columns[0].name, "id");
        assert_eq!(result.columns[0].data_type, "INTEGER");
        assert_eq!(result.rows.len(), 2);
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
