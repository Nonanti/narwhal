//! `PostgreSQL` driver backed by `tokio-postgres`.
//!
//! Transport security is configured via the `ssl_mode` parameter in the
//! connection parameters. Supported values mirror libpq:
//!
//! - `disable`: no TLS.
//! - `prefer` (default): TLS with full chain + hostname verification against
//!   the system trust store or `ssl_root_cert`. Unlike libpq, there is no
//!   fallback to plain-text — this is a hardened interpretation.
//! - `require`: TLS with chain verification but hostname verification
//!   skipped (matches `MySQL` `require` semantics). The server certificate
//!   must chain to a trusted root, but the hostname in the certificate
//!   is not checked.
//! - `verify-ca`: identical to `require` (chain verify, no hostname) —
//!   provided for explicitness.
//! - `verify-full`: TLS with full chain + hostname verification.

#![forbid(unsafe_code)]

mod ddl;
mod tls;
mod types;

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures::stream::StreamExt;
use narwhal_core::{
    CancelHandle, Capabilities, Column, ColumnHeader, Connection, ConnectionConfig,
    ConnectionParams, DatabaseDriver, Error, ForeignKey, Index, IsolationLevel, QueryResult,
    ReferentialAction, Result, Row as CoreRow, RowStream, Schema, Table, TableKind, TableSchema,
    UniqueConstraint, Value,
};
use std::time::Duration;
use tokio_postgres::error::SqlState;
use tokio_postgres::types::{ToSql, Type};
use tokio_postgres::Config as PgConfig;
use tokio_postgres::NoTls;
use tracing::{debug, error, info};

use crate::tls::{make_tls_connector, InternalSslMode, MakeRustlsConnect};
use crate::types::{column_to_value, Param};

/// `PostgreSQL` driver. The current implementation uses `NoTls`; configurable
/// TLS support is planned and tracked separately.
#[derive(Debug, Default)]
pub struct PostgresDriver;

impl PostgresDriver {
    pub const NAME: &'static str = "postgres";

    pub const fn new() -> Self {
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
            // tokio_postgres::Client::query_raw exposes a server-side
            // cursor; rows arrive incrementally without buffering.
            .with_streaming(true)
            .with_row_level_dml(true)
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
        let pg_config = build_pg_config(config, password)?;
        let sslmode = InternalSslMode::from_params(&config.params)?;
        debug!(target: "narwhal::postgres", sslmode = %sslmode.as_str(), "establishing connection");

        let client = match sslmode {
            InternalSslMode::Disable => {
                let (client, connection) = pg_config
                    .connect(NoTls)
                    .await
                    .map_err(|e| Error::Connection(e.to_string()))?;
                spawn_connection(connection);
                client
            }
            other => {
                let connector = make_tls_connector(other, &config.params)?;
                let (client, connection) = pg_config
                    .connect(connector)
                    .await
                    .map_err(|e| Error::Connection(e.to_string()))?;
                spawn_connection(connection);
                client
            }
        };

        info!(target: "narwhal::postgres", "connection established");
        Ok(Box::new(PostgresConnection {
            client: Arc::new(client),
            tls_mode: sslmode,
            params: Arc::new(config.params.clone()),
            prepared_cache: std::sync::Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(64).expect("64 is nonzero"),
            )),
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

/// Options whitelist for the PG connection string. Only these keys from
/// `params.options` are forwarded to the server; unknown keys are
/// rejected with a config error to prevent injection.
///
/// `sslmode` is included for backward compatibility: the driver's
/// [`InternalSslMode::from_params`] reads it from the options map when
/// the explicit `ssl_mode` field is at the default. Without the whitelist
/// entry, a config that sets `options.sslmode` would be rejected at load
/// time, breaking existing user configurations.
const OPTIONS_WHITELIST: &[&str] = &[
    "application_name",
    "connect_timeout",
    "options",
    "keepalives",
    "keepalives_idle",
    "sslmode",
];

fn build_pg_config(config: &ConnectionConfig, password: Option<&str>) -> Result<PgConfig> {
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

    let mut cfg = PgConfig::new();
    cfg.host(host).port(port).dbname(database).user(user);

    if let Some(pw) = password {
        cfg.password(pw);
    }

    for (k, v) in &config.params.options {
        if !OPTIONS_WHITELIST.contains(&k.as_str()) {
            return Err(Error::Config(format!("unsupported connection option: {k}")));
        }
        match k.as_str() {
            "application_name" => {
                cfg.application_name(v);
            }
            "connect_timeout" => {
                let secs: u64 = v
                    .parse()
                    .map_err(|_| Error::Config(format!("invalid connect_timeout value: {v}")))?;
                cfg.connect_timeout(Duration::from_secs(secs));
            }
            "options" => {
                cfg.options(v);
            }
            "keepalives" => {
                let enabled: bool = v
                    .parse()
                    .map_err(|_| Error::Config(format!("invalid keepalives value: {v}")))?;
                cfg.keepalives(enabled);
            }
            "keepalives_idle" => {
                let secs: u64 = v
                    .parse()
                    .map_err(|_| Error::Config(format!("invalid keepalives_idle value: {v}")))?;
                cfg.keepalives_idle(Duration::from_secs(secs));
            }
            // `sslmode` is consumed by `InternalSslMode::from_params`
            // before reaching `build_pg_config`. It is whitelisted so
            // the config parser does not reject it, but it is not
            // forwarded to the server — libpq's sslmode is handled at
            // the TLS layer, not as a connection parameter.
            "sslmode" => {}
            _ => unreachable!("whitelist check above guarantees this"),
        }
    }

    Ok(cfg)
}

pub struct PostgresConnection {
    client: Arc<tokio_postgres::Client>,
    /// TLS mode used for the original connection, needed by cancel handle
    /// so it can use the same TLS configuration.
    tls_mode: InternalSslMode,
    params: Arc<ConnectionParams>,
    /// Prepared statement cache (M9). Avoids a prepare round-trip
    /// for repeated admin/schema queries.
    prepared_cache: std::sync::Mutex<lru::LruCache<String, tokio_postgres::Statement>>,
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

fn parse_action(token: &str) -> Option<ReferentialAction> {
    // Postgres returns single-character codes for ON UPDATE / ON DELETE:
    // a=NO ACTION, r=RESTRICT, c=CASCADE, n=SET NULL, d=SET DEFAULT.
    match token {
        "a" => Some(ReferentialAction::NoAction),
        "r" => Some(ReferentialAction::Restrict),
        "c" => Some(ReferentialAction::Cascade),
        "n" => Some(ReferentialAction::SetNull),
        "d" => Some(ReferentialAction::SetDefault),
        _ => None,
    }
}

impl PostgresConnection {
    async fn list_indexes(&self, schema: &str, table: &str) -> Result<Vec<Index>> {
        const SQL: &str = "
            SELECT i.relname,
                   ix.indisunique,
                   ix.indisprimary,
                   pg_catalog.pg_get_indexdef(ix.indexrelid, k + 1, true)
            FROM pg_catalog.pg_class t
            JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace
            JOIN pg_catalog.pg_index ix ON ix.indrelid = t.oid
            JOIN pg_catalog.pg_class i ON i.oid = ix.indexrelid
            CROSS JOIN LATERAL generate_series(0, array_length(ix.indkey, 1) - 1) AS k
            WHERE n.nspname = $1 AND t.relname = $2
            ORDER BY i.relname, k";
        let rows = self
            .run(
                SQL,
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
            let unique = matches!(row.0.get(1), Some(Value::Bool(true)));
            let primary = matches!(row.0.get(2), Some(Value::Bool(true)));
            let column_expr = match row.0.get(3) {
                Some(Value::String(s)) => s.clone(),
                _ => continue,
            };
            let entry = by_name.entry(name.clone()).or_insert(Index {
                name,
                columns: Vec::new(),
                unique,
                primary,
            });
            entry.columns.push(column_expr);
        }
        Ok(by_name.into_values().collect())
    }

    async fn list_foreign_keys(&self, schema: &str, table: &str) -> Result<Vec<ForeignKey>> {
        const SQL: &str = "
            SELECT con.conname,
                   con.conkey,
                   nf.nspname,
                   cf.relname,
                   con.confkey,
                   con.confupdtype::text,
                   con.confdeltype::text,
                   (SELECT string_agg(a.attname, E'\x1F' ORDER BY k.ord)
                    FROM unnest(con.conkey) WITH ORDINALITY AS k(num, ord)
                    JOIN pg_catalog.pg_attribute a
                      ON a.attrelid = con.conrelid AND a.attnum = k.num) AS cols,
                   (SELECT string_agg(a.attname, E'\x1F' ORDER BY k.ord)
                    FROM unnest(con.confkey) WITH ORDINALITY AS k(num, ord)
                    JOIN pg_catalog.pg_attribute a
                      ON a.attrelid = con.confrelid AND a.attnum = k.num) AS refcols
            FROM pg_catalog.pg_constraint con
            JOIN pg_catalog.pg_class c ON c.oid = con.conrelid
            JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
            JOIN pg_catalog.pg_class cf ON cf.oid = con.confrelid
            JOIN pg_catalog.pg_namespace nf ON nf.oid = cf.relnamespace
            WHERE con.contype = 'f' AND n.nspname = $1 AND c.relname = $2
            ORDER BY con.conname";
        let rows = self
            .run(
                SQL,
                &[
                    Value::String(schema.to_owned()),
                    Value::String(table.to_owned()),
                ],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.rows.len());
        for row in rows.rows {
            let name = match row.0.first() {
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
            let on_update = row.0.get(5).and_then(|v| match v {
                Value::String(s) => parse_action(s),
                _ => None,
            });
            let on_delete = row.0.get(6).and_then(|v| match v {
                Value::String(s) => parse_action(s),
                _ => None,
            });
            let columns = extract_csv(row.0.get(7));
            let referenced_columns = extract_csv(row.0.get(8));
            out.push(ForeignKey {
                name,
                columns,
                referenced_schema: ref_schema,
                referenced_table: ref_table,
                referenced_columns,
                on_update,
                on_delete,
            });
        }
        Ok(out)
    }

    async fn list_unique_constraints(
        &self,
        schema: &str,
        table: &str,
    ) -> Result<Vec<UniqueConstraint>> {
        const SQL: &str = "
            SELECT con.conname,
                   (SELECT string_agg(a.attname, E'\x1F' ORDER BY k.ord)
                    FROM unnest(con.conkey) WITH ORDINALITY AS k(num, ord)
                    JOIN pg_catalog.pg_attribute a
                      ON a.attrelid = con.conrelid AND a.attnum = k.num)
            FROM pg_catalog.pg_constraint con
            JOIN pg_catalog.pg_class c ON c.oid = con.conrelid
            JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
            WHERE con.contype = 'u' AND n.nspname = $1 AND c.relname = $2
            ORDER BY con.conname";
        let rows = self
            .run(
                SQL,
                &[
                    Value::String(schema.to_owned()),
                    Value::String(table.to_owned()),
                ],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.rows.len());
        for row in rows.rows {
            let name = match row.0.first() {
                Some(Value::String(s)) => s.clone(),
                _ => continue,
            };
            let columns = extract_csv(row.0.get(1));
            if !columns.is_empty() {
                out.push(UniqueConstraint { name, columns });
            }
        }
        Ok(out)
    }
}

fn extract_csv(value: Option<&Value>) -> Vec<String> {
    // The schema queries above use `string_agg(..., E'\x1F')` to flatten
    // multi-column constraints into a text value separated by the unit
    // separator character (U+001F). This avoids breakage when identifiers
    // themselves contain commas (bug M8). We fall back to comma-splitting
    // for values produced by older queries that haven't been migrated yet.
    let raw = match value {
        Some(Value::String(s) | Value::Unknown(s)) => s,
        _ => return Vec::new(),
    };
    if raw.is_empty() {
        return Vec::new();
    }
    if raw.contains('\x1F') {
        raw.split('\x1F')
            .map(std::borrow::ToOwned::to_owned)
            .collect()
    } else {
        raw.split(',').map(std::borrow::ToOwned::to_owned).collect()
    }
}

impl PostgresConnection {
    /// Prepare the statement, using the LRU cache when possible to
    /// avoid repeated prepare round-trips (M9).
    async fn prepare_cached(&self, sql: &str) -> Result<tokio_postgres::Statement> {
        // Check cache first (sync lock, very brief).
        {
            let mut cache = self
                .prepared_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(stmt) = cache.get(sql) {
                return Ok(stmt.clone());
            }
        }
        // Not cached — prepare on the server.
        let statement = self.client.prepare(sql).await.map_err(map_pg_error)?;
        {
            let mut cache = self
                .prepared_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            cache.put(sql.to_owned(), statement.clone());
        }
        Ok(statement)
    }

    /// Prepare the statement, then route to `query` or `execute` based on
    /// whether the statement returns rows.
    async fn run(&self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        let started = Instant::now();
        let statement = self.prepare_cached(sql).await?;

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
        let statement = self.prepare_cached(sql).await?;

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
            // Future isolation levels: default to SERIALIZABLE (strictest).
            _ => "SERIALIZABLE",
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

    async fn list_all_tables(&mut self) -> Result<Vec<(Schema, Vec<Table>)>> {
        const SQL: &str = "
            SELECT n.nspname, c.relname, c.relkind::text
              FROM pg_catalog.pg_class c
              JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
               AND n.nspname NOT LIKE 'pg_temp_%'
               AND n.nspname NOT LIKE 'pg_toast_temp_%'
               AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
             ORDER BY n.nspname, c.relname";
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
                Some(Value::String(s)) => match s.as_str() {
                    "r" | "p" => TableKind::Table,
                    "v" => TableKind::View,
                    "m" => TableKind::MaterializedView,
                    "f" => TableKind::Table,
                    _ => TableKind::Table,
                },
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
        // Append any schemas that appeared in the query but not in
        // list_schemas (defensive).
        for (name, tables) in map {
            out.push((Schema { name }, tables));
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

        // Index/FK/unique queries are best-effort — permission errors
        // on a single catalogue should not fail `describe_table`, but
        // they must surface in logs so opaque empty index lists in the
        // UI are explainable.
        let indexes = match self.list_indexes(schema, name).await {
            Ok(v) => v,
            Err(error) => {
                tracing::warn!(
                    target: "narwhal::postgres",
                    schema, table = name, error = %error,
                    "list_indexes failed; continuing without"
                );
                Vec::new()
            }
        };
        let foreign_keys = match self.list_foreign_keys(schema, name).await {
            Ok(v) => v,
            Err(error) => {
                tracing::warn!(
                    target: "narwhal::postgres",
                    schema, table = name, error = %error,
                    "list_foreign_keys failed; continuing without"
                );
                Vec::new()
            }
        };
        let unique_constraints = match self.list_unique_constraints(schema, name).await {
            Ok(v) => v,
            Err(error) => {
                tracing::warn!(
                    target: "narwhal::postgres",
                    schema, table = name, error = %error,
                    "list_unique_constraints failed; continuing without"
                );
                Vec::new()
            }
        };

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
        ddl::build_create_table(self, schema, name).await
    }

    async fn ping(&mut self) -> Result<()> {
        self.client
            .simple_query("SELECT 1")
            .await
            .map(|_| ())
            .map_err(|e| Error::Connection(e.to_string()))
    }

    fn cancel_handle(&self) -> Option<Box<dyn CancelHandle>> {
        let tls_factory: Arc<dyn Fn() -> Result<MakeRustlsConnect> + Send + Sync> =
            if self.tls_mode == InternalSslMode::Disable {
                Arc::new(|| {
                    Err(Error::Config(
                        "cancel not available in disable TLS mode".into(),
                    ))
                })
            } else {
                let mode = self.tls_mode;
                let params = Arc::clone(&self.params);
                Arc::new(move || make_tls_connector(mode, &params))
            };
        Some(Box::new(PostgresCancelHandle {
            token: self.client.cancel_token(),
            tls_mode: self.tls_mode,
            tls_factory,
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
    tls_mode: InternalSslMode,
    tls_factory: Arc<dyn Fn() -> Result<MakeRustlsConnect> + Send + Sync>,
}

#[async_trait]
impl CancelHandle for PostgresCancelHandle {
    async fn cancel(&self) -> Result<()> {
        if self.tls_mode == InternalSslMode::Disable {
            self.token
                .cancel_query::<NoTls>(NoTls)
                .await
                .map_err(|e| Error::Connection(e.to_string()))
        } else {
            let connector = (self.tls_factory)().map_err(|e| {
                Error::Connection(format!("failed to create TLS connector for cancel: {e}"))
            })?;
            self.token
                .cancel_query(connector)
                .await
                .map_err(|e| Error::Connection(e.to_string()))
        }
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
    fn pg_config_includes_password_and_options() {
        let mut options = std::collections::BTreeMap::new();
        options.insert("application_name".into(), "narwhal".into());
        options.insert("connect_timeout".into(), "30".into());
        let params = ConnectionParams {
            host: Some("db.local".into()),
            port: Some(6543),
            database: Some("analytics".into()),
            username: Some("reader".into()),
            options,
            ..Default::default()
        };
        let cfg = config(params);
        let pg_cfg = build_pg_config(&cfg, Some("pass word")).unwrap();
        // The Config builder handles special characters safely —
        // no string concatenation injection risk.
        assert_eq!(pg_cfg.get_user(), Some("reader"));
        assert_eq!(pg_cfg.get_dbname(), Some("analytics"));
        assert!(pg_cfg.get_password().is_some());
        assert_eq!(pg_cfg.get_application_name(), Some("narwhal"));
    }

    #[test]
    fn capabilities_match_engine() {
        let caps = PostgresDriver::capabilities();
        assert!(caps.transactions);
        assert!(caps.cancellation);
        assert!(caps.multiple_schemas);
        assert!(caps.prepared_statements);
    }

    #[test]
    fn unknown_option_rejected() {
        let mut options = std::collections::BTreeMap::new();
        options.insert("evil_inject".into(), "value".into());
        let params = ConnectionParams {
            host: Some("db.local".into()),
            database: Some("analytics".into()),
            username: Some("reader".into()),
            options,
            ..Default::default()
        };
        let cfg = config(params);
        let err = build_pg_config(&cfg, None).unwrap_err();
        assert!(err
            .to_string()
            .contains("unsupported connection option: evil_inject"));
    }
}
