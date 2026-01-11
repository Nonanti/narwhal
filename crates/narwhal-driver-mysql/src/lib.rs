//! MySQL / MariaDB driver backed by `mysql_async`.
//!
//! The driver opens a dedicated connection per [`Connection`] instance
//! rather than sharing a pool internally; multi-connection workloads are
//! served by the `narwhal-pool` crate which is agnostic to the underlying
//! engine.

#![forbid(unsafe_code)]

mod types;

#[doc(hidden)]
pub mod __test_only {
    //! Private helpers exposed for integration tests only. Not part of the
    //! public API; do not depend on this module outside the crate's own
    //! `tests/` directory.
    use mysql_async::consts::ColumnType;
    use mysql_async::Value as MyValue;
    use narwhal_core::{Error, Value};

    pub fn try_value_to_my(value: &Value) -> Result<MyValue, Error> {
        super::types::try_value_to_my(value)
    }

    pub fn value_from_my(value: &MyValue, ty: ColumnType) -> Value {
        super::types::value_from_my(value, ty)
    }

    pub fn unique_constraints_from_indexes(
        indexes: &[narwhal_core::Index],
    ) -> Vec<narwhal_core::UniqueConstraint> {
        super::unique_constraints_from_indexes(indexes)
    }

    pub fn map_table_kind(table_type: Option<&str>) -> narwhal_core::TableKind {
        super::map_table_kind(table_type)
    }

    pub fn uses_text_protocol(sql: &str) -> bool {
        super::uses_text_protocol(sql)
    }
}

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use mysql_async::consts::ColumnType;
use mysql_async::prelude::*;
use mysql_async::{ClientIdentity, Conn, Opts, OptsBuilder, Params, SslOpts};
use narwhal_core::{
    CancelHandle, Capabilities, Column, ColumnHeader, Connection, ConnectionConfig, DatabaseDriver,
    Error, ForeignKey, Index, IsolationLevel, QueryResult, ReferentialAction, Result,
    Row as CoreRow, RowStream, Schema, SslMode, Table, TableKind, TableSchema, UniqueConstraint,
    Value,
};
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::types::{column_header, try_value_to_my, value_from_my};

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
            // MySQL's `stream` currently materialises the full result
            // into `BufferedRowStream`; advertise that until a real
            // `stream_and_drop` implementation lands (bug H5).
            .with_streaming(false)
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
        let mut conn = Conn::new(opts.clone())
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;

        // L31: capture CONNECTION_ID() so the cancel handle can target
        // the right thread via KILL QUERY on a second connection.
        let connection_id: u64 = conn
            .query_first("SELECT CONNECTION_ID()")
            .await
            .map_err(|e| Error::Connection(e.to_string()))?
            .unwrap_or(0);

        info!(
            target: "narwhal::mysql",
            connection_id,
            "connection established"
        );
        Ok(Box::new(MysqlConnection {
            inner: Arc::new(Mutex::new(Some(conn))),
            connection_id,
            opts,
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

    // Wire TLS options from the connection params.
    if config.params.ssl_mode != SslMode::Disable {
        let mut ssl_opts = SslOpts::default();

        if let Some(path) = &config.params.ssl_root_cert {
            ssl_opts = ssl_opts.with_root_certs(vec![path.clone().into()]);
        }

        if config.params.ssl_cert.is_some() && config.params.ssl_key.is_some() {
            let cert_path = config.params.ssl_cert.clone();
            let key_path = config.params.ssl_key.clone();
            let identity = ClientIdentity::new(cert_path.unwrap().into(), key_path.unwrap().into());
            ssl_opts = ssl_opts.with_client_identity(Some(identity));
        }

        // M2: For verify-ca / verify-full, enforce server certificate
        // verification. For prefer/require, chain-verify but skip hostname.
        // Neither prefer nor require accepts invalid (self-signed) certs.
        let skip_domain = !matches!(config.params.ssl_mode, SslMode::VerifyFull);
        let accept_invalid_certs = false;

        ssl_opts = ssl_opts.with_danger_skip_domain_validation(skip_domain);
        ssl_opts = ssl_opts.with_danger_accept_invalid_certs(accept_invalid_certs);

        builder = builder.ssl_opts(ssl_opts);
    }

    Ok(Opts::from(builder))
}

pub struct MysqlConnection {
    inner: Arc<Mutex<Option<Conn>>>,
    /// MySQL server-assigned thread id, captured at connect time. Used
    /// by [`MysqlCancelHandle`] to target the right session with
    /// `KILL QUERY` (L31).
    connection_id: u64,
    /// Cached connection options so the cancel handle can open a second
    /// connection to issue `KILL QUERY` against `connection_id`.
    opts: Opts,
}

/// L31: cancel handle that fires `KILL QUERY <thread_id>` on a fresh
/// connection. Best-effort: if opening the secondary connection fails
/// (e.g. server is at max_connections) we surface the error rather than
/// pretending the cancel succeeded.
struct MysqlCancelHandle {
    connection_id: u64,
    opts: Opts,
}

#[async_trait]
impl CancelHandle for MysqlCancelHandle {
    async fn cancel(&self) -> Result<()> {
        // CONNECTION_ID() never returns 0 on a healthy session, so a
        // stored zero means the connect path's lookup fell back. Refuse
        // to fire `KILL QUERY 0` (which would either error out or, on
        // some forks, hit an unrelated session).
        if self.connection_id == 0 {
            return Err(Error::unsupported(
                "cancel: connection id not captured at connect time",
            ));
        }
        let mut killer = Conn::new(self.opts.clone())
            .await
            .map_err(|e| Error::Connection(format!("cancel: open killer conn: {e}")))?;
        let sql = format!("KILL QUERY {}", self.connection_id);
        debug!(
            target: "narwhal::mysql",
            connection_id = self.connection_id,
            "sending KILL QUERY"
        );
        killer
            .query_drop(&sql)
            .await
            .map_err(|e| Error::Query(format!("KILL QUERY {}: {e}", self.connection_id)))?;
        // Best-effort disconnect; ignore errors because the kill
        // already landed if we got here.
        let _ = killer.disconnect().await;
        Ok(())
    }
}

impl MysqlConnection {
    async fn fetch_table_kind(&mut self, schema: &str, name: &str) -> Result<TableKind> {
        let result = self
            .execute(
                "SELECT table_type FROM information_schema.tables \
                 WHERE table_schema = ? AND table_name = ? LIMIT 1",
                &[
                    Value::String(schema.to_owned()),
                    Value::String(name.to_owned()),
                ],
            )
            .await?;
        let table_type =
            result
                .rows
                .into_iter()
                .next()
                .and_then(|r| match r.0.into_iter().next() {
                    Some(Value::String(s)) => Some(s),
                    _ => None,
                });
        Ok(map_table_kind(table_type.as_deref()))
    }

    async fn list_indexes(&mut self, schema: &str, table: &str) -> Result<Vec<Index>> {
        let rows = self
            .execute(
                "SELECT index_name, non_unique, column_name \
                 FROM information_schema.statistics \
                 WHERE table_schema = ? AND table_name = ? \
                 ORDER BY index_name, seq_in_index",
                &[
                    Value::String(schema.to_owned()),
                    Value::String(table.to_owned()),
                ],
            )
            .await?;
        let mut by_name: std::collections::BTreeMap<String, Index> =
            std::collections::BTreeMap::new();
        for row in rows.rows {
            let name = match row.0.first() {
                Some(Value::String(s)) => s.clone(),
                _ => continue,
            };
            let non_unique = match row.0.get(1) {
                Some(Value::Int(i)) => *i != 0,
                Some(Value::String(s)) => s != "0",
                _ => true,
            };
            let column = match row.0.get(2) {
                Some(Value::String(s)) => s.clone(),
                _ => continue,
            };
            let primary = name == "PRIMARY";
            let entry = by_name.entry(name.clone()).or_insert(Index {
                name,
                columns: Vec::new(),
                unique: !non_unique,
                primary,
            });
            entry.columns.push(column);
        }
        Ok(by_name.into_values().collect())
    }

    async fn list_foreign_keys(&mut self, schema: &str, table: &str) -> Result<Vec<ForeignKey>> {
        let rows = self
            .execute(
                "SELECT k.constraint_name, k.column_name, k.referenced_table_schema, \
                        k.referenced_table_name, k.referenced_column_name, \
                        r.update_rule, r.delete_rule \
                 FROM information_schema.key_column_usage k \
                 LEFT JOIN information_schema.referential_constraints r \
                     ON r.constraint_schema = k.constraint_schema \
                    AND r.constraint_name = k.constraint_name \
                 WHERE k.table_schema = ? AND k.table_name = ? \
                    AND k.referenced_table_name IS NOT NULL \
                 ORDER BY k.constraint_name, k.ordinal_position",
                &[
                    Value::String(schema.to_owned()),
                    Value::String(table.to_owned()),
                ],
            )
            .await?;
        let mut by_name: std::collections::BTreeMap<String, ForeignKey> =
            std::collections::BTreeMap::new();
        for row in rows.rows {
            let name = match row.0.first() {
                Some(Value::String(s)) => s.clone(),
                _ => continue,
            };
            let column = match row.0.get(1) {
                Some(Value::String(s)) => s.clone(),
                _ => continue,
            };
            let ref_schema = match row.0.get(2) {
                Some(Value::String(s)) => Some(s.clone()),
                _ => None,
            };
            let ref_table = match row.0.get(3) {
                Some(Value::String(s)) => s.clone(),
                _ => continue,
            };
            let ref_column = match row.0.get(4) {
                Some(Value::String(s)) => s.clone(),
                _ => continue,
            };
            let on_update = row.0.get(5).and_then(|v| match v {
                Value::String(s) => ReferentialAction::from_engine_token(s),
                _ => None,
            });
            let on_delete = row.0.get(6).and_then(|v| match v {
                Value::String(s) => ReferentialAction::from_engine_token(s),
                _ => None,
            });
            let entry = by_name.entry(name.clone()).or_insert(ForeignKey {
                name,
                columns: Vec::new(),
                referenced_schema: ref_schema,
                referenced_table: ref_table,
                referenced_columns: Vec::new(),
                on_update,
                on_delete,
            });
            entry.columns.push(column);
            entry.referenced_columns.push(ref_column);
        }
        Ok(by_name.into_values().collect())
    }

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
        let bound: Vec<mysql_async::Value> =
            params.iter().map(try_value_to_my).collect::<Result<_>>()?;
        let sql = sql.to_owned();
        let started = Instant::now();

        self.with_conn(move |conn| {
            Box::pin(async move {
                // MySQL's prepared-statement protocol rejects several
                // administrative statements (SAVEPOINT, SET TRANSACTION,
                // USE, ...). When no parameters are bound, fall back to the
                // text protocol so those statements still work.
                if bound.is_empty() && uses_text_protocol(sql.as_str()) {
                    // Statements that MySQL refuses to prepare stay on
                    // the text protocol; their result columns are
                    // ignored anyway (transaction control, USE, ...).
                    let result = conn
                        .query_iter(sql.as_str())
                        .await
                        .map_err(|e| Error::Query(e.to_string()))?;
                    collect_text(result, started).await
                } else {
                    // Everything else goes through the binary protocol
                    // so column type information is preserved (bug H4).
                    // Parameterless calls use `Params::Empty` rather
                    // than `Params::Positional(vec![])` because the
                    // server treats them differently for some
                    // statements.
                    let params = if bound.is_empty() {
                        Params::Empty
                    } else {
                        Params::Positional(bound)
                    };
                    let result = conn
                        .exec_iter(sql.as_str(), params)
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
            // Future isolation levels: fall back to SERIALIZABLE (strictest).
            _ => "SERIALIZABLE",
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
                Some(Value::String(s)) => map_table_kind(Some(s.as_str())),
                _ => map_table_kind(None),
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
        let result = self
            .execute(
                "SELECT table_schema, table_name, table_type \
                 FROM information_schema.tables \
                 WHERE table_schema NOT IN ('mysql', 'information_schema', \
                 'performance_schema', 'sys') \
                 ORDER BY table_schema, table_name",
                &[],
            )
            .await?;

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
                Some(Value::String(s)) => map_table_kind(Some(s.as_str())),
                _ => map_table_kind(None),
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

        let indexes = self.list_indexes(schema, name).await.unwrap_or_default();
        let foreign_keys = self
            .list_foreign_keys(schema, name)
            .await
            .unwrap_or_default();
        let unique_constraints = unique_constraints_from_indexes(&indexes);
        let kind = self
            .fetch_table_kind(schema, name)
            .await
            .unwrap_or(TableKind::Table);

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
        let qualified = format!(
            "`{}`.`{}`",
            schema.replace('`', "``"),
            name.replace('`', "``")
        );
        let sql = format!("SHOW CREATE TABLE {qualified}");
        let result = self.execute(&sql, &[]).await?;
        // SHOW CREATE TABLE returns columns: Table, Create Table, ...
        // The DDL is in column index 1.
        match result
            .rows
            .into_iter()
            .next()
            .and_then(|r| r.0.into_iter().nth(1))
        {
            Some(Value::String(ddl)) => Ok(ddl),
            _ => Err(Error::Schema(format!(
                "DDL not found for table {schema}.{name}"
            ))),
        }
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
        // L31: opens a *second* connection to issue KILL QUERY against
        // our thread id. This is the same shape PostgreSQL's cancel
        // request takes: out-of-band signal, independent of the main
        // socket so a hung query can still be interrupted.
        if self.connection_id == 0 {
            return None;
        }
        Some(Box::new(MysqlCancelHandle {
            connection_id: self.connection_id,
            opts: self.opts.clone(),
        }))
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

/// Statements whose leading keyword forces them onto MySQL's text
/// protocol. The server refuses to prepare these — transaction
/// control, session state, catalogue introspection, lock management,
/// bulk load — so `exec_iter` would fail with a protocol error.
const TEXT_PROTOCOL_KEYWORDS: &[&str] = &[
    "SAVEPOINT",
    "RELEASE",
    "ROLLBACK",
    "START",
    "BEGIN",
    "COMMIT",
    "USE",
    "SET",
    "SHOW",
    "DESCRIBE",
    "DESC",
    "EXPLAIN",
    "LOCK",
    "UNLOCK",
    "FLUSH",
    "RESET",
    "KILL",
    "PURGE",
    "LOAD",
    "HANDLER",
];

/// Decides whether an SQL statement must travel over MySQL's *text*
/// protocol rather than the binary prepared-statement protocol.
///
/// The leading keyword (after skipping ASCII whitespace and a single
/// run of `--` / `/* ... */` comments) is matched case-insensitively
/// against [`TEXT_PROTOCOL_KEYWORDS`]. Anything else — including the
/// empty input — routes through the binary protocol so column types
/// survive intact (see bug H4).
fn uses_text_protocol(sql: &str) -> bool {
    let Some(keyword) = leading_keyword(sql) else {
        return false;
    };
    TEXT_PROTOCOL_KEYWORDS
        .iter()
        .any(|kw| keyword.eq_ignore_ascii_case(kw))
}

/// Returns the first SQL keyword in `sql`, skipping ASCII whitespace
/// and any leading run of `--` line comments and `/* ... */` block
/// comments. Returns `None` when the input is empty or comment-only.
fn leading_keyword(sql: &str) -> Option<&str> {
    let bytes = sql.as_bytes();
    let mut i = 0;
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            // Skip the closing `*/` (or stop at EOF).
            i = (i + 2).min(bytes.len());
            continue;
        }
        break;
    }
    let start = i;
    while i < bytes.len() && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
        i += 1;
    }
    if start == i {
        None
    } else {
        Some(&sql[start..i])
    }
}

/// Maps the `information_schema.tables.TABLE_TYPE` string into
/// [`TableKind`]. Returns [`TableKind::Table`] for anything unknown or
/// missing so `describe_table` degrades gracefully on dialects whose
/// catalogue we have not catalogued yet.
fn map_table_kind(table_type: Option<&str>) -> TableKind {
    match table_type {
        Some("VIEW") => TableKind::View,
        Some("SYSTEM VIEW" | "SYSTEM TABLE") => TableKind::SystemTable,
        _ => TableKind::Table,
    }
}

/// Pure helper used by [`MysqlConnection::describe_table`] so the
/// filter logic can be unit-tested without an integration database.
///
/// All UNIQUE indexes are surfaced (single-column UNIQUE included);
/// the implicit PRIMARY KEY index is excluded because it is reported
/// separately via [`Column::primary_key`].
fn unique_constraints_from_indexes(indexes: &[Index]) -> Vec<UniqueConstraint> {
    indexes
        .iter()
        .filter(|i| i.unique && !i.primary)
        .map(|i| UniqueConstraint {
            name: i.name.clone(),
            columns: i.columns.clone(),
        })
        .collect()
}

fn map_rows(rows: Vec<mysql_async::Row>, column_count: usize) -> Vec<CoreRow> {
    rows.into_iter()
        .map(|row| {
            // Capture per-column types before consuming the row so we can
            // honour BLOB/VARBINARY in the decoder (bug L29).
            let types: Vec<ColumnType> =
                row.columns_ref().iter().map(|c| c.column_type()).collect();
            let mut values = Vec::with_capacity(column_count);
            for (idx, value) in row.unwrap_raw().into_iter().enumerate() {
                let ty = types
                    .get(idx)
                    .copied()
                    .unwrap_or(ColumnType::MYSQL_TYPE_NULL);
                values.push(value_from_my(
                    &value.unwrap_or(mysql_async::Value::NULL),
                    ty,
                ));
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
