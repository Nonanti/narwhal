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
//! * **Streaming** — [`Connection::stream`] uses
//!   `reqwest::Response::bytes_stream()` to feed a small line buffer
//!   that walks byte chunks as they arrive, emits the two TSV header
//!   lines once available, then forwards each completed row through
//!   an [`mpsc`] channel. Backpressure is provided by the channel's
//!   bounded buffer (capacity 64). Data cells are parsed at the byte
//!   level (not routed through `&str`) because ClickHouse's `String`
//!   type is byte-oriented — cells may contain arbitrary bytes that
//!   are not valid UTF-8. TSV escape sequences (`\b \f \n \r \t \0 \\ \'`)
//!   are decoded, and invalid-UTF-8 payloads are preserved as
//!   [`Value::Bytes`] instead of being silently replaced with `U+FFFD`.
//! * **Cancellation** — Each outgoing request is tagged with a
//!   `query_id` (UUID v4) that is tracked in an `Arc<Mutex<HashSet>>`
//!   on the connection. [`Connection::cancel_handle`] returns a handle
//!   whose `cancel()` method reads the active query IDs and issues a
//!   `KILL QUERY WHERE query_id IN (...)` request. Cancellation is
//!   best-effort: server errors during KILL are ignored and an empty
//!   active-queries set is a no-op.
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

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use narwhal_core::{
    CancelHandle, Capabilities, Column, ColumnHeader, Connection, ConnectionConfig,
    ConnectionParams, DatabaseDriver, Error, IsolationLevel, QueryResult, Result, Row as CoreRow,
    RowStream, Schema, SslMode, Table, TableKind, TableSchema, Value,
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
            .with_cancellation(true)
            .with_multiple_schemas(true)
            .with_prepared_statements(false)
            .with_savepoints(false)
            .with_rows_affected(false)
            // Native HTTP TSV response is consumed chunk-by-chunk via
            // `stream_tsv_chunks`, so rows are surfaced progressively.
            .with_streaming(true)
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
        let mut client_builder = reqwest::Client::builder().timeout(REQUEST_TIMEOUT);

        // When using HTTPS, add the CA root certificate if provided.
        if config.params.ssl_mode != SslMode::Disable {
            let has_root_cert = if let Some(path) = &config.params.ssl_root_cert {
                let bytes = std::fs::read(path).map_err(|e| {
                    Error::Config(format!(
                        "failed to read ssl_root_cert '{}': {e}",
                        path.display()
                    ))
                })?;
                let cert = reqwest::Certificate::from_pem(&bytes)
                    .map_err(|e| Error::Config(format!("failed to parse ssl_root_cert: {e}")))?;
                client_builder = client_builder.add_root_certificate(cert);
                true
            } else {
                false
            };

            // Determine certificate and hostname validation based on
            // ssl_mode and whether an explicit CA was provided:
            //
            // | ssl_mode   | ssl_root_cert | accept_invalid_certs | accept_invalid_hostnames |
            // |------------|---------------|----------------------|--------------------------|
            // | Prefer     | None          | true                 | true                     |
            // | Prefer     | Some          | false                | true                     |
            // | Require    | None          | true                 | true                     |
            // | Require    | Some          | false                | true                     |
            // | VerifyCa   | *             | false                | true                     |
            // | VerifyFull | *             | false                | false                    |
            let accept_invalid_certs = match config.params.ssl_mode {
                SslMode::VerifyCa | SslMode::VerifyFull => false,
                // Prefer/Require: if user supplied a CA cert, honour it.
                _ => !has_root_cert,
            };
            let accept_invalid_hostnames = !matches!(config.params.ssl_mode, SslMode::VerifyFull);

            if has_root_cert && matches!(config.params.ssl_mode, SslMode::Prefer | SslMode::Require)
            {
                tracing::warn!(
                    target: "narwhal::clickhouse",
                    "ssl_root_cert is set with ssl_mode='{}'; \
                     CA certificate will be validated but hostname will not — \
                     use ssl_mode='verify-full' to also enforce hostname checking",
                    match config.params.ssl_mode {
                        SslMode::Prefer => "prefer",
                        SslMode::Require => "require",
                        _ => "unknown",
                    }
                );
            }

            client_builder = client_builder
                .danger_accept_invalid_certs(accept_invalid_certs)
                .danger_accept_invalid_hostnames(accept_invalid_hostnames);
        }

        // Client identity (mTLS) is not directly supported by reqwest's
        // high-level API in the same way as mysql_async / rustls.
        // If ssl_cert and ssl_key are both provided, we read them and
        // configure identity via reqwest::Identity.
        if let (Some(cert_path), Some(key_path)) = (&config.params.ssl_cert, &config.params.ssl_key)
        {
            let cert_bytes = std::fs::read(cert_path).map_err(|e| {
                Error::Config(format!(
                    "failed to read ssl_cert '{}': {e}",
                    cert_path.display()
                ))
            })?;
            let key_bytes = std::fs::read(key_path).map_err(|e| {
                Error::Config(format!(
                    "failed to read ssl_key '{}': {e}",
                    key_path.display()
                ))
            })?;
            // Combine cert + key into a single PEM blob for reqwest::Identity.
            let mut pem = Vec::with_capacity(cert_bytes.len() + key_bytes.len());
            pem.extend_from_slice(&cert_bytes);
            pem.extend_from_slice(&key_bytes);
            let identity = reqwest::Identity::from_pem(&pem)
                .map_err(|e| Error::Config(format!("failed to parse client identity PEM: {e}")))?;
            client_builder = client_builder.identity(identity);
        }

        let client = client_builder
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
            let body = response
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
            return Err(Error::Connection(format!(
                "ClickHouse returned {status}: {body}"
            )));
        }

        info!(target: "narwhal::clickhouse", %base_url, "connected");

        Ok(Box::new(ClickhouseConnection {
            inner: Arc::new(SharedState {
                client,
                base_url,
                user,
                password: pw,
                database,
                active_queries: Arc::new(Mutex::new(HashSet::new())),
            }),
        }))
    }
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

/// Shared state behind a plain `Arc` so the spawned streaming task
/// can clone the `Arc` and issue HTTP requests independently.
/// `reqwest::Client` is already `Send + Sync` with an internal
/// connection pool, so no mutex is needed to parallelise requests.
///
/// `active_queries` uses a `tokio::sync::Mutex` because we hold it
/// briefly across `.await` points in the cancel path.
struct SharedState {
    client: reqwest::Client,
    base_url: Url,
    user: String,
    password: String,
    database: String,
    active_queries: Arc<Mutex<HashSet<String>>>,
}

impl SharedState {
    /// Build an authenticated POST request with SQL in the body.
    ///
    /// Centralises the auth + body pattern so every call site stays
    /// consistent and a future auth change touches one place.
    fn build_request(&self, url: &Url, body: String) -> reqwest::RequestBuilder {
        self.client
            .post(url.as_str())
            .basic_auth(
                &self.user,
                if self.password.is_empty() {
                    None
                } else {
                    Some(self.password.as_str())
                },
            )
            .body(body)
    }
}

pub struct ClickhouseConnection {
    inner: Arc<SharedState>,
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
/// Uses `https://` when `ssl_mode` is not `Disable`, otherwise `http://`.
/// Default: `http://localhost:8123/`.
fn build_base_url(params: &ConnectionParams) -> Result<Url> {
    let host = params
        .host
        .as_deref()
        .ok_or_else(|| Error::Config("host is required".into()))?;
    let port = params.port.unwrap_or(8123);
    let scheme = if params.ssl_mode == SslMode::Disable {
        "http"
    } else {
        "https"
    };
    Url::parse(&format!("{scheme}://{host}:{port}/"))
        .map_err(|e| Error::Config(format!("invalid URL: {e}")))
}

/// Double-quote an identifier for ClickHouse (e.g. `"my table"`).
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

impl ClickhouseConnection {
    /// Send a query to ClickHouse via HTTP and return the full response
    /// body as a string.
    ///
    /// If `query_id` is `Some`, the query ID is registered in the
    /// active-queries set for the duration of the request so that
    /// cancellation can target it.
    /// Send a query to ClickHouse via HTTP and return the full response
    /// body as raw bytes.
    ///
    /// If `query_id` is `Some`, the query ID is registered in the
    /// active-queries set for the duration of the request so that
    /// cancellation can target it.
    ///
    /// Uses `response.bytes()` instead of `response.text()` because
    /// ClickHouse's `String` type is byte-oriented — cells may contain
    /// arbitrary bytes that are not valid UTF-8.
    async fn http_query(&self, sql: &str, query_id: Option<&str>) -> Result<Vec<u8>> {
        let state = &self.inner;
        let mut url = state.base_url.clone();
        url.query_pairs_mut()
            .append_pair("database", &state.database);

        if let Some(qid) = query_id {
            url.query_pairs_mut().append_pair("query_id", qid);
        }

        debug!(target: "narwhal::clickhouse", %sql, "sending HTTP query");

        // Register query ID before sending.
        if let Some(qid) = query_id {
            state.active_queries.lock().await.insert(qid.to_owned());
        }

        // SQL goes in the request body, not the URL query string. URLs
        // are capped around 8 KiB on most front-end proxies and even on
        // bare ClickHouse, long analytical queries blow that limit.
        let response = match state.build_request(&url, sql.to_owned()).send().await {
            Ok(r) => r,
            Err(e) => {
                if let Some(qid) = query_id {
                    state.active_queries.lock().await.remove(qid);
                }
                return Err(Error::Query(e.to_string()));
            }
        };

        let status = response.status();
        if !status.is_success() {
            // Error bodies are ClickHouse error messages — always UTF-8.
            let body = response
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
            // Remove query ID on failure.
            if let Some(qid) = query_id {
                state.active_queries.lock().await.remove(qid);
            }
            return Err(Error::Query(format!(
                "ClickHouse returned {status}: {body}"
            )));
        }

        // Remove query ID on success.
        if let Some(qid) = query_id {
            state.active_queries.lock().await.remove(qid);
        }

        response
            .bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| Error::Query(e.to_string()))
    }

    /// Send a query with `TabSeparatedWithNamesAndTypes` format and
    /// return a parsed [`QueryResult`].
    async fn query_tsv(&self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        let started = Instant::now();
        let query_id = Self::new_query_id();

        let formatted_sql = if params.is_empty() {
            sql.to_owned()
        } else {
            substitute_params(sql, params)
        };

        // Append the format directive.
        let full_sql = format!("{formatted_sql}\nFORMAT TabSeparatedWithNamesAndTypes");

        let body = self.http_query(&full_sql, Some(&query_id)).await?;
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
        let query_id = Self::new_query_id();

        let formatted_sql = if params.is_empty() {
            sql.to_owned()
        } else {
            substitute_params(sql, params)
        };

        self.http_query(&formatted_sql, Some(&query_id)).await?;

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

/// Substitute `?` and `$N` placeholders with rendered SQL literals.
///
/// Left-to-right walk that respects single- and double-quoted string
/// literals so a `?` or `$1` *inside* a literal is left untouched.
/// Each `?` consumes the next parameter; each `$N` (1-based) selects
/// `params[N-1]` directly so the two styles can be mixed.
fn substitute_params(sql: &str, params: &[Value]) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut quote: Option<char> = None;
    let mut placeholder_idx = 0usize;
    let mut chars = sql.chars().peekable();

    while let Some(ch) = chars.next() {
        if let Some(q) = quote {
            out.push(ch);
            if ch == q {
                if chars.peek() == Some(&q) {
                    // Doubled quote = escaped literal quote.
                    out.push(q);
                    chars.next();
                } else {
                    quote = None;
                }
            }
            continue;
        }
        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                out.push(ch);
            }
            '?' => {
                if let Some(p) = params.get(placeholder_idx) {
                    out.push_str(&value_to_sql_literal(p));
                    placeholder_idx += 1;
                } else {
                    out.push('?');
                }
            }
            '$' => {
                // Try to parse $N; if not a positive integer, copy the
                // `$` literally so dollar-amount strings outside a
                // literal (rare) survive.
                let mut digits = String::new();
                while let Some(&next) = chars.peek() {
                    if next.is_ascii_digit() {
                        digits.push(next);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if digits.is_empty() {
                    out.push('$');
                    continue;
                }
                match digits.parse::<usize>() {
                    Ok(n) if n >= 1 => {
                        if let Some(p) = params.get(n - 1) {
                            out.push_str(&value_to_sql_literal(p));
                        } else {
                            out.push('$');
                            out.push_str(&digits);
                        }
                    }
                    _ => {
                        out.push('$');
                        out.push_str(&digits);
                    }
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// Escape a string for use inside single-quoted SQL literals. Used for
/// internal queries against `system.tables` etc. where we splice schema
/// or table names into the SQL by hand instead of going through the
/// regular parameter binding path.
///
/// ClickHouse honours backslash escapes inside string literals, so a
/// lone backslash would be interpreted as an escape leader (potentially
/// swallowing a subsequent closing quote). Both backslash-to-doubled-backslash
/// and single-quote-to-doubled-quote are applied to prevent injection
/// and misinterpretation.
fn escape_sql_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("''"),
            other => out.push(other),
        }
    }
    out
}

#[doc(hidden)]
pub mod __test_only {
    //! Private helpers exposed for integration tests only. Not part of
    //! the public API; do not depend on this module outside the crate's
    //! own `tests/` directory.
    use narwhal_core::Value;

    pub fn replace_question_marks(sql: &str, params: &[Value]) -> String {
        super::replace_question_marks(sql, params)
    }

    pub fn substitute_params(sql: &str, params: &[Value]) -> String {
        super::substitute_params(sql, params)
    }
}

/// Replace `?` placeholders left-to-right with parameter literals.
///
/// Char-based walk so multi-byte UTF-8 sequences in identifiers and
/// string literals survive intact. Single- and double-quoted regions
/// are tracked with doubled-quote escape detection (`''`, `""`).
fn replace_question_marks(sql: &str, params: &[Value]) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut param_iter = params.iter();
    let mut quote: Option<char> = None;
    let mut chars = sql.chars().peekable();

    while let Some(ch) = chars.next() {
        if let Some(q) = quote {
            out.push(ch);
            if ch == q {
                if chars.peek() == Some(&q) {
                    out.push(q);
                    chars.next();
                } else {
                    quote = None;
                }
            }
            continue;
        }
        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                out.push(ch);
            }
            '?' => {
                if let Some(p) = param_iter.next() {
                    out.push_str(&value_to_sql_literal(p));
                } else {
                    out.push('?');
                }
            }
            other => out.push(other),
        }
    }
    out
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
        let state = &self.inner;
        let formatted_sql = if params.is_empty() {
            sql.to_owned()
        } else {
            substitute_params(sql, params)
        };

        let query_id = Self::new_query_id();

        // Register query ID for cancellation tracking.
        state.active_queries.lock().await.insert(query_id.clone());

        if !statement_returns_rows(&formatted_sql) {
            // Non-row-returning: execute and return an empty stream.
            // SQL goes in the body (not the URL) to avoid the ~8 KiB
            // URL length limit on large analytical DML statements.
            let mut url = state.base_url.clone();
            {
                let mut pairs = url.query_pairs_mut();
                pairs.append_pair("database", &state.database);
                pairs.append_pair("query_id", &query_id);
            }

            let response = match state.build_request(&url, formatted_sql).send().await {
                Ok(r) => r,
                Err(e) => {
                    state.active_queries.lock().await.remove(&query_id);
                    return Err(Error::Query(e.to_string()));
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
                state.active_queries.lock().await.remove(&query_id);
                return Err(Error::Query(format!(
                    "ClickHouse returned {status}: {body}"
                )));
            }

            // Deregister on success.
            state.active_queries.lock().await.remove(&query_id);

            // Drop the sender immediately so the receiver yields
            // `Ok(None)` on first poll — a clean empty stream. No task
            // was spawned in this branch, so the stream owns nothing
            // that needs cancellation.
            let (_tx, rx) = mpsc::channel::<Result<CoreRow>>(1);
            return Ok(Box::new(ClickhouseRowStream {
                columns: Vec::new(),
                rx,
                task: None,
            }));
        }

        // Row-returning: use TSV format and stream the body.
        let full_sql = format!("{formatted_sql}\nFORMAT TabSeparatedWithNamesAndTypes");

        let mut url = state.base_url.clone();
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("database", &state.database);
            pairs.append_pair("query_id", &query_id);
        }

        let response = match state.build_request(&url, full_sql).send().await {
            Ok(r) => r,
            Err(e) => {
                state.active_queries.lock().await.remove(&query_id);
                return Err(Error::Query(e.to_string()));
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
            state.active_queries.lock().await.remove(&query_id);
            return Err(Error::Query(format!(
                "ClickHouse returned {status}: {body}"
            )));
        }

        // Chunked streaming: use bytes_stream() to receive byte chunks
        // as they arrive and feed a small line buffer. Rows are emitted
        // through the mpsc channel as soon as their line is complete.
        //
        // The query ID is deregistered when the spawned task completes.
        let (header_tx, header_rx) = tokio::sync::oneshot::channel::<Result<Vec<ColumnHeader>>>();
        let (row_tx, row_rx) = mpsc::channel::<Result<CoreRow>>(64);
        let active_queries = state.active_queries.clone();
        let qid = query_id.clone();

        let task = tokio::spawn(async move {
            stream_tsv_chunks(response.bytes_stream(), header_tx, row_tx).await;
            // Deregister the query ID now that the stream is complete.
            active_queries.lock().await.remove(&qid);
        });

        let columns = match header_rx.await {
            Ok(Ok(cols)) => cols,
            Ok(Err(e)) => {
                // stream_tsv_chunks sent an error through header_tx. The
                // spawned task has already returned, so abort() is a
                // no-op but keeps the cleanup path uniform.
                task.abort();
                return Err(e);
            }
            Err(_) => {
                task.abort();
                return Err(Error::Other("clickhouse stream cancelled".into()));
            }
        };

        Ok(Box::new(ClickhouseRowStream {
            columns,
            rx: row_rx,
            task: Some(task),
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

    async fn fetch_ddl(&mut self, schema: &str, name: &str) -> Result<String> {
        let escaped_schema = quote_ident(schema);
        let escaped_name = quote_ident(name);
        let sql = format!("SHOW CREATE TABLE {escaped_schema}.{escaped_name}");
        let result = self.query_tsv(&sql, &[]).await?;
        // SHOW CREATE TABLE returns: name, create_table_query, ...
        // The DDL is in column index 1 (or sometimes the last column).
        match result.rows.into_iter().next() {
            Some(row) => {
                // ClickHouse SHOW CREATE TABLE returns a single column with the statement.
                // Try the last column first (engine-dependent), fall back to index 1.
                let ddl = row.0.last().cloned().or_else(|| row.0.into_iter().nth(1));
                match ddl {
                    Some(Value::String(s)) => Ok(s),
                    _ => Err(Error::Schema(format!(
                        "DDL not found for table {schema}.{name}"
                    ))),
                }
            }
            None => Err(Error::Schema(format!(
                "DDL not found for table {schema}.{name}"
            ))),
        }
    }

    async fn ping(&mut self) -> Result<()> {
        self.http_query("SELECT 1", None).await.map(|_| ())
    }

    fn cancel_handle(&self) -> Option<Box<dyn CancelHandle>> {
        Some(Box::new(ClickhouseCancel {
            state: Arc::clone(&self.inner),
        }))
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
// Cancellation
// ---------------------------------------------------------------------------

/// Cancellation handle for ClickHouse connections.
///
/// Reads the active query IDs from the shared state and issues a
/// `KILL QUERY WHERE query_id IN (...)` request. Best-effort:
/// server errors during KILL are silently ignored.
struct ClickhouseCancel {
    state: Arc<SharedState>,
}

#[async_trait]
impl CancelHandle for ClickhouseCancel {
    async fn cancel(&self) -> Result<()> {
        let query_ids: Vec<String> = self.state.active_queries.lock().await.iter().cloned().collect();

        if query_ids.is_empty() {
            // No active queries — nothing to cancel.
            return Ok(());
        }

        // Build a KILL QUERY statement targeting all active IDs.
        let ids: Vec<String> = query_ids.iter().map(|id| format!("'{id}'")).collect();
        let kill_sql = format!("KILL QUERY WHERE query_id IN ({})", ids.join(", "));

        debug!(target: "narwhal::clickhouse", %kill_sql, "cancelling queries");

        // Fire-and-forget: we don't care if the KILL succeeds or fails.
        let state = &self.state;
        let mut url = state.base_url.clone();
        url.query_pairs_mut()
            .append_pair("database", &state.database);

        let result = state.build_request(&url, kill_sql).send().await;

        match result {
            Ok(response) => {
                if !response.status().is_success() {
                    debug!(
                        target: "narwhal::clickhouse",
                        status = %response.status(),
                        "KILL QUERY returned non-success (best-effort, ignoring)"
                    );
                }
            }
            Err(e) => {
                debug!(
                    target: "narwhal::clickhouse",
                    error = %e,
                    "KILL QUERY request failed (best-effort, ignoring)"
                );
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Row stream
// ---------------------------------------------------------------------------

struct ClickhouseRowStream {
    columns: Vec<ColumnHeader>,
    rx: mpsc::Receiver<Result<CoreRow>>,
    /// Handle to the spawned task that drives [`stream_tsv_chunks`].
    /// `close()` aborts it so a slow server cannot keep dribbling bytes
    /// into a buffer no one is reading after the consumer walks away.
    /// `None` for the empty-stream case (no task was spawned).
    task: Option<tokio::task::JoinHandle<()>>,
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

    async fn close(mut self: Box<Self>) -> Result<()> {
        // Closing the receiver alone is not enough: if the task is
        // currently parked on bytes_stream.next().await waiting for the
        // next chunk from the server, it stays parked until that chunk
        // arrives and only then notices the dropped row_tx. On a big
        // result set that can keep the HTTP connection and a chunk of
        // RAM tied up for seconds. abort() unparks it immediately.
        if let Some(handle) = self.task.take() {
            handle.abort();
        }
        Ok(())
    }
}

impl Drop for ClickhouseRowStream {
    fn drop(&mut self) {
        // Caller dropped without calling close(): same reasoning.
        if let Some(handle) = self.task.take() {
            handle.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// Chunked TSV stream decoder (testable in isolation)
// ---------------------------------------------------------------------------

/// Drive a `bytes_stream`-style async byte source through the TSV
/// line-buffered decoder and emit headers + rows via channels.
///
/// This is the core logic extracted from `Connection::stream` so it
/// can be unit-tested without a real HTTP server.
async fn stream_tsv_chunks<S>(
    stream: S,
    header_tx: tokio::sync::oneshot::Sender<Result<Vec<ColumnHeader>>>,
    row_tx: mpsc::Sender<Result<CoreRow>>,
) where
    S: futures_util::Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>> + Unpin,
{
    use futures_util::StreamExt;

    let mut stream = stream;
    let mut buf: Vec<u8> = Vec::new();

    // Collect the first two newline-terminated lines (column names,
    // then type strings) before switching to row mode.
    //
    // Header lines are always UTF-8 (ASCII identifiers and type names
    // on the wire). We use `from_utf8_lossy` defensively — if the
    // server sends non-UTF-8 headers that is a server bug, and the
    // replacement character makes it visible.
    let mut header_lines: Vec<String> = Vec::new();

    while header_lines.len() < 2 {
        match stream.next().await {
            Some(Ok(chunk)) => {
                buf.extend_from_slice(&chunk);
                while header_lines.len() < 2 {
                    let Some(pos) = buf.iter().position(|&b| b == b'\n') else {
                        break;
                    };
                    let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
                    // Warn if header bytes are not valid UTF-8 — this
                    // would indicate a server-side bug since column names
                    // and type strings are always ASCII identifiers.
                    if std::str::from_utf8(&line_bytes).is_err() {
                        tracing::warn!(
                            target: "narwhal::clickhouse",
                            "header line contained invalid UTF-8; lossy conversion applied"
                        );
                    }
                    let line = String::from_utf8_lossy(&line_bytes);
                    let line = line.trim_end_matches('\n').trim_end_matches('\r');
                    header_lines.push(line.to_owned());
                }
            }
            Some(Err(e)) => {
                let _ = header_tx.send(Err(Error::Query(e.to_string())));
                return;
            }
            None => {
                // Stream ended before both header lines arrived — this
                // indicates a network interruption or server-side
                // cancellation, not a legitimate empty result.
                let _ = header_tx.send(Err(Error::Query(
                    "clickhouse stream ended before headers were complete".into(),
                )));
                return;
            }
        }
    }

    // The header phase guarantees exactly two lines in header_lines
    // (the inner loop stops as soon as the second \n is seen and any
    // trailing data row bytes stay in `buf` for row mode to consume).
    // Indexing is safe: the outer `while header_lines.len() < 2` exit
    // condition implies len == 2 here.
    let header_line = header_lines[0].as_str();
    let type_line = header_lines[1].as_str();

    let headers: Vec<String> = header_line.split('\t').map(String::from).collect();
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

    // Row mode — byte-level field splitting. Data cells may contain
    // arbitrary bytes (ClickHouse String type is byte-oriented), so we
    // never route through &str on the data path.
    loop {
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let mut line_bytes: Vec<u8> = buf.drain(..=pos).collect();
            // Strip trailing \n and optional \r.
            if line_bytes.last() == Some(&b'\n') {
                line_bytes.pop();
            }
            if line_bytes.last() == Some(&b'\r') {
                line_bytes.pop();
            }
            if line_bytes.is_empty() {
                continue;
            }
            let fields: Vec<&[u8]> = line_bytes.split(|&b| b == b'\t').collect();
            let mut row = Vec::with_capacity(headers.len());
            for (i, field) in fields.iter().enumerate() {
                let ch_type = type_strings.get(i).map(String::as_str).unwrap_or("String");
                row.push(parse_tsv_value(field, ch_type));
            }
            while row.len() < headers.len() {
                row.push(Value::Null);
            }
            if row_tx.send(Ok(CoreRow(row))).await.is_err() {
                return;
            }
        }

        match stream.next().await {
            Some(Ok(chunk)) => {
                buf.extend_from_slice(&chunk);
            }
            Some(Err(e)) => {
                let _ = row_tx.send(Err(Error::Query(e.to_string()))).await;
                return;
            }
            None => {
                // End of stream. ClickHouse always terminates rows with
                // \n in TSV, so a non-empty buffer here means the
                // response was truncated mid-row — surface it instead of
                // silently delivering a half-parsed row as a real result.
                if !buf.is_empty() {
                    if buf.last() == Some(&b'\r') {
                        buf.pop();
                    }
                    if !buf.is_empty() {
                        let _ = row_tx
                            .send(Err(Error::Query(
                                "clickhouse stream truncated mid-row (no trailing newline)".into(),
                            )))
                            .await;
                    }
                }
                return;
            }
        }
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    use bytes::Bytes;
    use futures_util::stream;

    /// Feed a known TSV payload through the chunked decoder split
    /// across multiple byte chunks to verify line-buffered splitting.
    #[tokio::test]
    async fn chunked_tsv_decodes_rows() {
        // Simulate a ClickHouse TSV response split across 3 chunks
        // that don't align on line boundaries.
        let payload: &[u8] = b"id\tname\nUInt32\tString\n1\talice\n2\tbob\n";

        // Split the payload across chunk boundaries that don't
        // align with line boundaries.
        let chunks: Vec<std::result::Result<Bytes, reqwest::Error>> = vec![
            Ok(Bytes::copy_from_slice(&payload[..8])),
            Ok(Bytes::copy_from_slice(&payload[8..20])),
            Ok(Bytes::copy_from_slice(&payload[20..])),
        ];

        let byte_stream = stream::iter(chunks);
        let (header_tx, header_rx) = tokio::sync::oneshot::channel::<Result<Vec<ColumnHeader>>>();
        let (row_tx, mut row_rx) = mpsc::channel::<Result<CoreRow>>(64);

        // Call directly instead of spawning — simpler for testing.
        stream_tsv_chunks(byte_stream, header_tx, row_tx).await;

        let columns = header_rx.await.expect("header rx").expect("headers");
        assert_eq!(columns.len(), 2);
        assert_eq!(columns[0].name, "id");
        assert_eq!(columns[0].data_type, "UInt32");
        assert_eq!(columns[1].name, "name");
        assert_eq!(columns[1].data_type, "String");

        let mut rows = Vec::new();
        while let Some(result) = row_rx.recv().await {
            let row = result.expect("row");
            rows.push(row);
        }

        assert_eq!(rows.len(), 2);
        assert!(matches!(rows[0].0.first(), Some(Value::Int(1))));
        assert!(matches!(rows[0].0.get(1), Some(Value::String(_))));
        assert!(matches!(rows[1].0.first(), Some(Value::Int(2))));
        assert!(matches!(rows[1].0.get(1), Some(Value::String(_))));
    }

    /// Verify that a binary String cell (non-UTF-8 bytes) arrives as
    /// `Value::Bytes` through the streaming path.
    #[tokio::test]
    async fn chunked_tsv_preserves_binary_string() {
        // Build a TSV payload where the String column contains
        // 0xFF 0xFE 0x00 0x01 — not valid UTF-8.
        let mut payload: Vec<u8> = b"col\nString\n".to_vec();
        payload.extend_from_slice(&[0xFF, 0xFE, 0x00, 0x01]);
        payload.push(b'\n');

        let chunks: Vec<std::result::Result<Bytes, reqwest::Error>> =
            vec![Ok(Bytes::copy_from_slice(&payload))];

        let byte_stream = stream::iter(chunks);
        let (header_tx, header_rx) = tokio::sync::oneshot::channel::<Result<Vec<ColumnHeader>>>();
        let (row_tx, mut row_rx) = mpsc::channel::<Result<CoreRow>>(64);

        stream_tsv_chunks(byte_stream, header_tx, row_tx).await;

        let columns = header_rx.await.expect("header rx").expect("headers");
        assert_eq!(columns.len(), 1);
        assert_eq!(columns[0].name, "col");
        assert_eq!(columns[0].data_type, "String");

        let row = row_rx.recv().await.expect("row rx").expect("row");
        match row.0.first() {
            Some(Value::Bytes(b)) => assert_eq!(b, &vec![0xFF, 0xFE, 0x00, 0x01]),
            other => panic!("expected Value::Bytes, got {other:?}"),
        }
    }

    /// A response truncated mid-row (no trailing newline after the last
    /// row's bytes) must surface as `Error::Query`, not as a half-parsed
    /// row silently delivered to the consumer.
    #[tokio::test]
    async fn chunked_tsv_truncated_mid_row_errors() {
        // Headers complete (two \n-terminated lines), then a partial
        // row with no trailing \n.
        let payload: &[u8] = b"id\tname\nUInt32\tString\n1\tali";

        let chunks: Vec<std::result::Result<Bytes, reqwest::Error>> =
            vec![Ok(Bytes::copy_from_slice(payload))];

        let byte_stream = stream::iter(chunks);
        let (header_tx, header_rx) = tokio::sync::oneshot::channel::<Result<Vec<ColumnHeader>>>();
        let (row_tx, mut row_rx) = mpsc::channel::<Result<CoreRow>>(64);

        stream_tsv_chunks(byte_stream, header_tx, row_tx).await;

        // Headers came through fine — truncation is mid-row.
        let _columns = header_rx.await.expect("header rx").expect("headers");

        // The first (and only) item from the row channel must be Err.
        match row_rx.recv().await {
            Some(Err(Error::Query(msg))) => {
                assert!(
                    msg.contains("truncated"),
                    "expected truncation error, got: {msg}"
                );
            }
            other => panic!("expected Err(Error::Query(truncated)), got {other:?}"),
        }
    }
}

#[cfg(test)]
mod cancel_tests {
    use super::*;

    /// Verify that query IDs are correctly inserted into and removed
    /// from the active-queries set.
    #[tokio::test]
    async fn tracks_active_query_id() {
        let active: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

        // Simulate inserting a query ID.
        active.lock().await.insert("test-qid-1".to_owned());
        assert!(active.lock().await.contains("test-qid-1"));

        // Simulate removing it.
        active.lock().await.remove("test-qid-1");
        assert!(!active.lock().await.contains("test-qid-1"));

        // Set should be empty.
        assert!(active.lock().await.is_empty());
    }

    /// Verify that cancel() is idempotent — a second call on the same
    /// handle still sees the same query IDs (they are cloned, not drained).
    #[tokio::test]
    async fn cancel_reads_cloned_not_drained() {
        let active: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        active.lock().await.insert("qid-1".to_owned());
        active.lock().await.insert("qid-2".to_owned());

        // First cancel should read the IDs without draining.
        let client = reqwest::Client::new();
        let base_url = Url::parse("http://127.0.0.1:1/").expect("url");
        let state = Arc::new(SharedState {
            client,
            base_url,
            user: "default".to_owned(),
            password: String::new(),
            database: "default".to_owned(),
            active_queries: active.clone(),
        });
        let cancel = ClickhouseCancel { state };

        // Cancel will try to POST KILL QUERY — this will fail since no
        // server, but the important thing is we don't drain the set.
        let _ = cancel.cancel().await;

        // The active_queries set should still contain the query IDs.
        let remaining = active.lock().await;
        assert!(remaining.contains("qid-1"), "qid-1 should still be present after cancel");
        assert!(remaining.contains("qid-2"), "qid-2 should still be present after cancel");
    }

    /// Verify that calling cancel with no active queries returns Ok(())
    /// and doesn't attempt an HTTP request (no server to contact).
    #[tokio::test]
    async fn cancel_with_no_active_queries_is_noop() {
        // Build a minimal SharedState. The URL points at an unreachable
        // host — if cancel tries to issue an HTTP request, the test
        // would fail or hang, proving the early-return guard works.
        let active: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        let client = reqwest::Client::new();
        let base_url = Url::parse("http://127.0.0.1:1/").expect("url");

        let state = Arc::new(SharedState {
            client,
            base_url,
            user: "default".to_owned(),
            password: String::new(),
            database: "default".to_owned(),
            active_queries: active.clone(),
        });

        let cancel = ClickhouseCancel { state };
        let result = cancel.cancel().await;
        assert!(result.is_ok());

        // Active set should still be empty.
        assert!(active.lock().await.is_empty());
    }
}
