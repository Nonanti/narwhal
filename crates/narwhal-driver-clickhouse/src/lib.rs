//! ClickHouse driver using the native HTTP interface.
//!
//! ClickHouse exposes an HTTP API on port 8123 by default. Queries are
//! sent as `POST` requests with the SQL in the body and results come back
//! in the `TabSeparatedWithNamesAndTypes` format which embeds column
//! names and native type strings in the first two rows.
//!
//! # Architecture
//!
//! * **Transport** — [`reqwest`] async HTTP client. One client is shared
//!   across all queries on a connection; the client is cloned (which is
//!   cheap — it internally uses an `Arc` connection pool).
//! * **Streaming** — [`Connection::stream`] currently **buffers the
//!   full HTTP response body** before parsing it line-by-line into rows
//!   that flow over an [`mpsc`] channel. True chunked streaming (via
//!   `Response::bytes_stream`) is a planned improvement; today large
//!   result sets are subject to the buffer's memory footprint.
//! * **Cancellation** — Cancellation is **not wired up** in this MVP.
//!   ClickHouse supports `KILL QUERY WHERE query_id = ?` server-side,
//!   but the driver does not yet track the active query_id per
//!   connection. [`Connection::cancel_handle`] therefore returns
//!   `None` and [`ClickhouseDriver::capabilities`] reports
//!   `with_cancellation(false)` rather than lie about support.
//! * **Parameter binding** — ClickHouse's HTTP API does not support
//!   server-side prepared statements. Parameters are rendered as SQL
//!   literals via [`types::value_to_sql_literal`] and interpolated into
//!   the query string. String escaping uses single-quote doubling to
//!   prevent injection.
//!
//! # Limitations
//!
//! * ClickHouse does not support true ACID transactions, savepoints, or
//!   foreign keys. The corresponding [`Connection`] methods return
//!   [`Error::Unsupported`].
//! * `rows_affected` is not reliably available from the HTTP response
//!   (it lives in the `X-ClickHouse-Summary` header, but the format is
//!   version-dependent). For now, `rows_affected` is always `None` for
//!   DML and `0` for row-returning statements.

#![forbid(unsafe_code)]

mod types;

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use narwhal_core::{
    CancelHandle, Capabilities, Column, ColumnHeader, Connection, ConnectionConfig,
    ConnectionParams, DatabaseDriver, Error, IsolationLevel, QueryResult, Result, Row as CoreRow,
    RowStream, Schema, Table, TableKind, TableSchema, Value,
};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info};
use url::Url;

use crate::types::{parse_tsv_body, parse_tsv_value, value_to_sql_literal};

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// ClickHouse driver factory.
#[derive(Debug, Default)]
pub struct ClickhouseDriver;

impl ClickhouseDriver {
    pub const NAME: &'static str = "clickhouse";

    pub fn new() -> Self {
        Self
    }

    fn capabilities() -> Capabilities {
        Capabilities::default()
            .with_transactions(false)
            // No active-query tracking yet — KILL QUERY needs the
            // running query_id and the connection doesn't carry one
            // through the call chain. Flip to true together with the
            // tracking work.
            .with_cancellation(false)
            .with_multiple_schemas(true)
            .with_prepared_statements(false)
            .with_savepoints(false)
            .with_rows_affected(false)
    }
}

#[async_trait]
impl DatabaseDriver for ClickhouseDriver {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn display_name(&self) -> &'static str {
        "ClickHouse"
    }

    fn validate(&self, config: &ConnectionConfig) -> Vec<String> {
        let mut problems = Vec::new();
        if config.params.host.is_none() {
            problems.push("host is required".into());
        }
        problems
    }

    async fn connect(
        &self,
        config: &ConnectionConfig,
        password: Option<&str>,
    ) -> Result<Box<dyn Connection>> {
        let base_url = build_base_url(&config.params)?;
        let user = config
            .params
            .username
            .as_deref()
            .unwrap_or("default")
            .to_owned();
        let database = config
            .params
            .database
            .as_deref()
            .unwrap_or("default")
            .to_owned();
        let pw = password.map(String::from).unwrap_or_default();

        debug!(target: "narwhal::clickhouse", %base_url, %user, %database, "connecting");

        // Five-minute default request timeout. ClickHouse analytical
        // queries can run for a long time; this is a per-request limit,
        // not a session limit. TODO: surface as a config option once
        // narwhal-config grows a `request_timeout_seconds` field.
        const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| Error::Connection(e.to_string()))?;

        // Ping to verify connectivity.
        let mut url = base_url.clone();
        url.query_pairs_mut().append_pair("query", "SELECT 1");

        let response = client
            .post(url.as_str())
            .basic_auth(&user, if pw.is_empty() { None } else { Some(&pw) })
            .send()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Connection(format!(
                "ClickHouse returned {status}: {body}"
            )));
        }

        info!(target: "narwhal::clickhouse", %base_url, "connected");

        Ok(Box::new(ClickhouseConnection {
            inner: Arc::new(Mutex::new(SharedState {
                client,
                base_url,
                user,
                password: pw,
                database,
            })),
        }))
    }
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

/// Shared state behind an `Arc<Mutex<>>` so the spawned streaming task
/// can clone the `Arc` and issue HTTP requests independently.
struct SharedState {
    client: reqwest::Client,
    base_url: Url,
    user: String,
    password: String,
    database: String,
}

pub struct ClickhouseConnection {
    inner: Arc<Mutex<SharedState>>,
}

/// Best-effort heuristic: does `sql` likely return a result set?
///
/// ClickHouse's HTTP API always returns a response body (even for DDL),
/// but we need to decide whether to parse it as rows or treat it as a
/// simple acknowledgement. The heuristic matches the same pattern used
/// by the DuckDB driver.
fn statement_returns_rows(sql: &str) -> bool {
    let lead = sql
        .trim_start()
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(
        lead.as_str(),
        "SELECT" | "WITH" | "SHOW" | "DESCRIBE" | "EXPLAIN" | "EXISTS"
    )
}

/// Build the base URL from connection parameters.
///
/// Default: `http://localhost:8123/`.
fn build_base_url(params: &ConnectionParams) -> Result<Url> {
    let host = params
        .host
        .as_deref()
        .ok_or_else(|| Error::Config("host is required".into()))?;
    let port = params.port.unwrap_or(8123);
    Url::parse(&format!("http://{host}:{port}/"))
        .map_err(|e| Error::Config(format!("invalid URL: {e}")))
}

/// Double-quote an identifier for ClickHouse (e.g. `"my table"`).
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

impl ClickhouseConnection {
    /// Send a query to ClickHouse via HTTP and return the full response
    /// body as a string.
    async fn http_query(&self, sql: &str) -> Result<String> {
        let state = self.inner.lock().await;
        let mut url = state.base_url.clone();
        url.query_pairs_mut()
            .append_pair("database", &state.database);

        debug!(target: "narwhal::clickhouse", %sql, "sending HTTP query");

        // SQL goes in the request body, not the URL query string. URLs
        // are capped around 8 KiB on most front-end proxies and even on
        // bare ClickHouse, long analytical queries blow that limit.
        let response = state
            .client
            .post(url.as_str())
            .basic_auth(
                &state.user,
                if state.password.is_empty() {
                    None
                } else {
                    Some(state.password.as_str())
                },
            )
            .body(sql.to_owned())
            .send()
            .await
            .map_err(|e| Error::Query(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Query(format!(
                "ClickHouse returned {status}: {body}"
            )));
        }

        response
            .text()
            .await
            .map_err(|e| Error::Query(e.to_string()))
    }

    /// Send a query with `TabSeparatedWithNamesAndTypes` format and
    /// return a parsed [`QueryResult`].
    async fn query_tsv(&self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        let started = Instant::now();

        let formatted_sql = if params.is_empty() {
            sql.to_owned()
        } else {
            substitute_params(sql, params)
        };

        // Append the format directive.
        let full_sql = format!("{formatted_sql}\nFORMAT TabSeparatedWithNamesAndTypes");

        let body = self.http_query(&full_sql).await?;
        let (headers, type_strings, rows) = parse_tsv_body(&body);

        let column_headers: Vec<ColumnHeader> = headers
            .into_iter()
            .zip(type_strings)
            .map(|(name, data_type)| ColumnHeader { name, data_type })
            .collect();

        let core_rows: Vec<CoreRow> = rows.into_iter().map(CoreRow).collect();

        Ok(QueryResult {
            columns: column_headers,
            rows: core_rows,
            rows_affected: None,
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// Execute a non-row-returning statement (DDL/DML).
    async fn execute_raw(&self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        let started = Instant::now();

        let formatted_sql = if params.is_empty() {
            sql.to_owned()
        } else {
            substitute_params(sql, params)
        };

        self.http_query(&formatted_sql).await?;

        Ok(QueryResult {
            columns: Vec::new(),
            rows: Vec::new(),
            rows_affected: None,
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// Generate a new query ID for use with cancellation.
    fn new_query_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }
}

/// Substitute `?` placeholders with rendered SQL literals.
///
/// This is a simple left-to-right replacement. Each `?` consumes the next
/// parameter value. Dollar-number placeholders (`$1`, `$2`) are also
/// supported for compatibility with other drivers.
fn substitute_params(sql: &str, params: &[Value]) -> String {
    if sql.contains('$') {
        // Try $1, $2, ... style first. If any are present, substitute
        // them by index; otherwise fall through to `?` substitution.
        let mut result = sql.to_owned();
        let mut any_dollar = false;
        for (i, param) in params.iter().enumerate() {
            let placeholder = format!("${}", i + 1);
            if result.contains(&placeholder) {
                any_dollar = true;
                let literal = value_to_sql_literal(param);
                result = result.replace(&placeholder, &literal);
            }
        }
        if any_dollar {
            // Still handle any remaining `?` placeholders with the
            // leftover params.
            return replace_question_marks(&result, params);
        }
    }

    replace_question_marks(sql, params)
}

/// Escape a string for use inside single-quoted SQL literals. Used for
/// internal queries against `system.tables` etc. where we splice schema
/// or table names into the SQL by hand instead of going through the
/// regular parameter binding path.
fn escape_sql_string(s: &str) -> String {
    s.replace('\'', "''")
}

/// Replace `?` placeholders left-to-right with parameter literals.
fn replace_question_marks(sql: &str, params: &[Value]) -> String {
    let mut result = String::with_capacity(sql.len());
    let mut param_iter = params.iter();
    let mut in_string = false;
    let mut string_quote = b'\0';
    let bytes = sql.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            result.push(c as char);
            if c == string_quote {
                // Check for escaped quote (doubled).
                if i + 1 < bytes.len() && bytes[i + 1] == c {
                    result.push(c as char);
                    i += 2;
                    continue;
                }
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == b'\'' || c == b'"' {
            in_string = true;
            string_quote = c;
            result.push(c as char);
            i += 1;
            continue;
        }
        if c == b'?' {
            if let Some(param) = param_iter.next() {
                result.push_str(&value_to_sql_literal(param));
            }
            i += 1;
            continue;
        }
        result.push(c as char);
        i += 1;
    }

    result
}

#[async_trait]
impl Connection for ClickhouseConnection {
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        if statement_returns_rows(sql) {
            self.query_tsv(sql, params).await
        } else {
            self.execute_raw(sql, params).await
        }
    }

    async fn stream(&mut self, sql: &str, params: &[Value]) -> Result<Box<dyn RowStream>> {
        let state = self.inner.lock().await;
        let formatted_sql = if params.is_empty() {
            sql.to_owned()
        } else {
            substitute_params(sql, params)
        };

        let query_id = Self::new_query_id();

        if !statement_returns_rows(&formatted_sql) {
            // Non-row-returning: execute and return an empty stream.
            let mut url = state.base_url.clone();
            {
                let mut pairs = url.query_pairs_mut();
                pairs.append_pair("database", &state.database);
                pairs.append_pair("query", &formatted_sql);
                pairs.append_pair("query_id", &query_id);
            }

            let response = state
                .client
                .post(url.as_str())
                .basic_auth(
                    &state.user,
                    if state.password.is_empty() {
                        None
                    } else {
                        Some(state.password.as_str())
                    },
                )
                .send()
                .await
                .map_err(|e| Error::Query(e.to_string()))?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(Error::Query(format!(
                    "ClickHouse returned {status}: {body}"
                )));
            }

            // Drop the sender immediately so the receiver yields
            // `Ok(None)` on first poll — a clean empty stream.
            let (_tx, rx) = mpsc::channel::<Result<CoreRow>>(1);
            return Ok(Box::new(ClickhouseRowStream {
                columns: Vec::new(),
                rx,
            }));
        }

        // Row-returning: use TSV format and stream the body.
        let full_sql = format!("{formatted_sql}\nFORMAT TabSeparatedWithNamesAndTypes");

        let mut url = state.base_url.clone();
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("database", &state.database);
            pairs.append_pair("query", &full_sql);
            pairs.append_pair("query_id", &query_id);
        }

        let response = state
            .client
            .post(url.as_str())
            .basic_auth(
                &state.user,
                if state.password.is_empty() {
                    None
                } else {
                    Some(state.password.as_str())
                },
            )
            .send()
            .await
            .map_err(|e| Error::Query(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Query(format!(
                "ClickHouse returned {status}: {body}"
            )));
        }

        // We need the full body for header parsing (first two lines), then
        // stream the remaining lines. Since reqwest's chunked streaming is
        // byte-oriented and we need line boundaries, we buffer the body
        // and stream rows via a channel task.
        let (header_tx, header_rx) = tokio::sync::oneshot::channel::<Result<Vec<ColumnHeader>>>();
        let (row_tx, row_rx) = mpsc::channel::<Result<CoreRow>>(64);

        let body_bytes = response
            .bytes()
            .await
            .map_err(|e| Error::Query(e.to_string()))?;

        tokio::spawn(async move {
            let body = String::from_utf8_lossy(&body_bytes);
            let mut lines = body.lines();

            // First line: column names.
            let header_line = if let Some(l) = lines.next() {
                l
            } else {
                let _ = header_tx.send(Ok(Vec::new()));
                return;
            };
            let headers: Vec<String> = header_line.split('\t').map(String::from).collect();

            // Second line: type strings.
            let type_line = if let Some(l) = lines.next() {
                l
            } else {
                let _ = header_tx.send(Ok(Vec::new()));
                return;
            };
            let type_strings: Vec<String> = type_line.split('\t').map(String::from).collect();

            let column_headers: Vec<ColumnHeader> = headers
                .iter()
                .zip(type_strings.iter())
                .map(|(name, data_type)| ColumnHeader {
                    name: name.clone(),
                    data_type: data_type.clone(),
                })
                .collect();

            if header_tx.send(Ok(column_headers)).is_err() {
                return;
            }

            // Remaining lines: data rows.
            for line in lines {
                let line = line.trim_end_matches('\r');
                if line.is_empty() {
                    continue;
                }
                let fields: Vec<&str> = line.split('\t').collect();
                let mut row = Vec::with_capacity(headers.len());
                for (i, field) in fields.iter().enumerate() {
                    let ch_type = type_strings.get(i).map(String::as_str).unwrap_or("String");
                    row.push(parse_tsv_value(field, ch_type));
                }
                while row.len() < headers.len() {
                    row.push(Value::Null);
                }
                if row_tx.send(Ok(CoreRow(row))).await.is_err() {
                    // Consumer dropped the stream.
                    break;
                }
            }
        });

        let columns = header_rx
            .await
            .map_err(|_| Error::Other("clickhouse stream cancelled".into()))??;

        Ok(Box::new(ClickhouseRowStream {
            columns,
            rx: row_rx,
        }))
    }

    async fn begin(&mut self) -> Result<()> {
        Err(Error::unsupported("transactions (ClickHouse)"))
    }

    async fn begin_with(&mut self, _isolation: IsolationLevel) -> Result<()> {
        Err(Error::unsupported("transactions (ClickHouse)"))
    }

    async fn commit(&mut self) -> Result<()> {
        Err(Error::unsupported("transactions (ClickHouse)"))
    }

    async fn rollback(&mut self) -> Result<()> {
        Err(Error::unsupported("transactions (ClickHouse)"))
    }

    async fn list_schemas(&mut self) -> Result<Vec<Schema>> {
        const SQL: &str = "SHOW DATABASES";
        let result = self.query_tsv(SQL, &[]).await?;

        // Filter system databases that are not interesting for browsing.
        // ClickHouse exposes `INFORMATION_SCHEMA` and `information_schema`
        // as two case variants of the same schema.
        let hidden = ["system", "INFORMATION_SCHEMA", "information_schema"];
        let mut out = Vec::with_capacity(result.rows.len());
        for row in result.rows {
            if let Some(Value::String(name)) = row.0.into_iter().next() {
                if !hidden.contains(&name.as_str()) {
                    out.push(Schema { name });
                }
            }
        }
        Ok(out)
    }

    async fn list_tables(&mut self, schema: &str) -> Result<Vec<Table>> {
        // schema is interpolated into a SQL literal; escape any `'`s
        // even though sidebar-driven calls won't contain them.
        let sql = format!(
            "SELECT name, engine FROM system.tables WHERE database = '{}' ORDER BY name",
            escape_sql_string(schema)
        );
        let result = self.query_tsv(&sql, &[]).await?;
        let mut out = Vec::with_capacity(result.rows.len());
        for row in result.rows {
            let mut iter = row.0.into_iter();
            let name = match iter.next() {
                Some(Value::String(s)) => s,
                _ => continue,
            };
            let engine = match iter.next() {
                Some(Value::String(s)) => s.to_ascii_lowercase(),
                _ => String::new(),
            };
            let kind = if engine == "view" {
                TableKind::View
            } else if engine == "materializedview" {
                TableKind::MaterializedView
            } else {
                TableKind::Table
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
        let escaped_schema = quote_ident(schema);
        let escaped_name = quote_ident(name);
        let sql = format!("DESCRIBE TABLE {escaped_schema}.{escaped_name}");
        let result = self.query_tsv(&sql, &[]).await?;

        if result.rows.is_empty() {
            return Err(Error::Schema(format!("table {schema}.{name} not found")));
        }

        // ClickHouse DESCRIBE TABLE returns:
        // name, type, default_type, default_expression, comment, codec_expression, ttl_expression
        let columns: Vec<Column> = result
            .rows
            .into_iter()
            .filter_map(|row| {
                let mut iter = row.0.into_iter();
                let col_name = match iter.next() {
                    Some(Value::String(s)) => s,
                    _ => return None,
                };
                let data_type = match iter.next() {
                    Some(Value::String(s)) => s,
                    _ => String::new(),
                };
                let _default_kind = match iter.next() {
                    Some(Value::String(s)) => s,
                    _ => String::new(),
                };
                let default_expr = match iter.next() {
                    Some(Value::String(s)) if !s.is_empty() => Some(s),
                    _ => None,
                };
                let default = default_expr;

                // ClickHouse doesn't have a traditional NOT NULL / PRIMARY KEY
                // in DESCRIBE TABLE. Nullable types are expressed in the type
                // string itself. Primary key info is available from system.tables.
                let nullable = data_type.trim().starts_with("Nullable(");

                Some(Column {
                    name: col_name,
                    data_type,
                    nullable,
                    primary_key: false,
                    default,
                })
            })
            .collect();

        // Try to look up primary key from system.tables.
        let primary_key_columns = self
            .lookup_primary_key(schema, name)
            .await
            .unwrap_or_default();
        let pk_set: std::collections::HashSet<String> = primary_key_columns.into_iter().collect();

        let columns: Vec<Column> = columns
            .into_iter()
            .map(|mut c| {
                c.primary_key = pk_set.contains(&c.name);
                c
            })
            .collect();

        // ClickHouse has no foreign keys. Skip indexes in MVP.
        Ok(TableSchema {
            table: Table {
                schema: schema.to_owned(),
                name: name.to_owned(),
                kind: TableKind::Table,
            },
            columns,
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
            unique_constraints: Vec::new(),
        })
    }

    async fn ping(&mut self) -> Result<()> {
        self.http_query("SELECT 1").await.map(|_| ())
    }

    fn cancel_handle(&self) -> Option<Box<dyn CancelHandle>> {
        // See `capabilities()` — cancellation is not yet wired up.
        None
    }

    fn capabilities(&self) -> Capabilities {
        ClickhouseDriver::capabilities()
    }

    async fn close(self: Box<Self>) -> Result<()> {
        // Nothing to close for HTTP — the reqwest client drops cleanly.
        Ok(())
    }
}

impl ClickhouseConnection {
    /// Look up the primary key columns for a table from `system.tables`.
    async fn lookup_primary_key(&mut self, schema: &str, name: &str) -> Result<Vec<String>> {
        // Both identifiers reach SQL as quoted literals; escape `'` to
        // close the injection vector even though normal callers pass
        // sanitised metadata names.
        let sql = format!(
            "SELECT primary_key FROM system.tables WHERE database = '{}' AND name = '{}'",
            escape_sql_string(schema),
            escape_sql_string(name)
        );
        let result = self.query_tsv(&sql, &[]).await?;
        match result.rows.into_iter().next() {
            Some(row) => match row.0.into_iter().next() {
                Some(Value::String(pk)) if !pk.is_empty() => {
                    Ok(pk.split(',').map(|s| s.trim().to_owned()).collect())
                }
                _ => Ok(Vec::new()),
            },
            None => Ok(Vec::new()),
        }
    }
}

// ---------------------------------------------------------------------------
// Row stream
// ---------------------------------------------------------------------------

struct ClickhouseRowStream {
    columns: Vec<ColumnHeader>,
    rx: mpsc::Receiver<Result<CoreRow>>,
}

#[async_trait]
impl RowStream for ClickhouseRowStream {
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
        // Dropping the receiver is sufficient — the sender side will
        // detect the closed channel and stop producing rows.
        Ok(())
    }
}

// Cancellation: deliberately omitted in this MVP. See the module-level
// doc comment for the why and the plan.
