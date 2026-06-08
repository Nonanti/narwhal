//! Microsoft SQL Server driver backed by [tiberius] (pure-Rust TDS).
//!
//! Feature gate: `mssql`. Pulled in by
//! `all-drivers` so the `narwhaldb` binary keeps shipping every backend
//! without consumers having to opt in field-by-field.
//!
//! # Transport
//!
//! Tiberius is runtime-agnostic — the [`Client`] takes any
//! `AsyncRead + AsyncWrite` from the *futures-rs* trait family, not
//! Tokio's. We bridge a [`tokio::net::TcpStream`] through
//! [`tokio_util::compat::TokioAsyncWriteCompatExt`] so the same Tokio
//! runtime hosts every driver in this crate.
//!
//! # TLS
//!
//! Built with tiberius' `rustls` feature, so the workspace stays free
//! of `openssl-sys`. The encryption level is driven by
//! [`narwhal_core::SslMode`]:
//!
//! - [`SslMode::Disable`] → `EncryptionLevel::NotSupported`
//! (`DANGER_PLAINTEXT` — only useful on a private trusted network;
//! SQL Server still negotiates TLS for the *login* packet by default
//! so this mode is rarely what the user wants).
//! - [`SslMode::Prefer`] / [`SslMode::Require`] (default) → `On`.
//! - [`SslMode::VerifyCa`] / [`SslMode::VerifyFull`] → `Required`.
//!
//! `TrustServerCertificate` is exposed via the
//! `trust_server_certificate` option key for dev/test connections
//! against self-signed certificates. It is documented as unsafe and is
//! the only way to talk to the official `mcr.microsoft.com/mssql/server`
//! image without setting up a CA.
//!
//! # Cancellation
//!
//! Tiberius has no out-of-band cancel signal (no equivalent of the
//! `PostgreSQL` `CancelRequest` message); we surface `cancel_handle() ->
//! None` rather than offering a `KILL SESSION` shim that would silently
//! kill unrelated traffic on the same SPID after a network round-trip.
//! [`Capabilities::cancellation`] reflects that.

#![forbid(unsafe_code)]

mod ddl;
mod types;

#[doc(hidden)]
pub mod __test_only {
    //! Private helpers exposed for integration tests only. Not part of
    //! the public API; do not depend on this module outside the crate's
    //! own `tests/` directory.
    use narwhal_core::{Index, TableKind, UniqueConstraint};
    use tiberius::ColumnType;

    pub fn column_type_name(ty: ColumnType) -> String {
        super::types::column_type_name(ty)
    }

    pub fn build_config(
        config: &narwhal_core::ConnectionConfig,
        password: Option<&str>,
    ) -> narwhal_core::Result<tiberius::Config> {
        super::build_config(config, password)
    }

    pub fn map_table_kind(table_type: Option<&str>) -> TableKind {
        super::map_table_kind(table_type)
    }

    pub fn unique_constraints_from_indexes(indexes: &[Index]) -> Vec<UniqueConstraint> {
        super::unique_constraints_from_indexes(indexes)
    }

    /// Wraps the internal statement classifier. Used by the binding
    /// test suite to lock in the comment / CTE / OUTPUT routing
    /// rules.
    pub fn classify_statement(sql: &str) -> super::StatementShape {
        super::classify_statement(sql)
    }

    pub use super::StatementShape;

    pub fn leading_keyword(sql: &str) -> Option<&str> {
        super::leading_keyword(sql)
    }

    pub fn contains_top_level_keyword(sql: &str, keyword: &str) -> bool {
        super::contains_top_level_keyword(sql, keyword)
    }
}

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures::TryStreamExt;
use narwhal_core::{
    Capabilities, ColumnHeader, Connection, ConnectionConfig, DatabaseDriver, Error, ForeignKey,
    Index, IsolationLevel, QueryResult, ReferentialAction, Result, Row as CoreRow, RowStream,
    Schema, SslMode, Table, TableKind, TableSchema, UniqueConstraint, Value,
};
use tiberius::{AuthMethod, Client, Config, EncryptionLevel, QueryItem};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};
use tracing::{debug, info, warn};

use self::types::{Param, column_header, column_to_value};

/// Driver-side adapter type: a Tokio TCP stream lifted into the
/// futures-rs IO traits that tiberius consumes.
type TiberiusStream = Compat<TcpStream>;

/// Concrete `tiberius::Client` shape we hold inside [`MssqlConnection`].
type TiberiusClient = Client<TiberiusStream>;

/// SQL Server driver. Feature-gated by `mssql`.
#[derive(Debug, Default)]
pub struct MssqlDriver;

impl MssqlDriver {
    pub const NAME: &'static str = "mssql";

    pub const fn new() -> Self {
        Self
    }

    fn capabilities() -> Capabilities {
        Capabilities::default()
            .with_transactions(true)
            // tiberius has no out-of-band cancel; see module docs.
            .with_cancellation(false)
            .with_multiple_schemas(true)
            .with_prepared_statements(true)
            .with_savepoints(true)
            .with_rows_affected(true)
            // We materialise the result into a `BufferedRowStream`
            // because tiberius' QueryStream borrows the client; the
            // mysql driver makes the same trade-off (bug H5 sibling).
            .with_streaming(false)
            .with_row_level_dml(true)
    }
}

impl DatabaseDriver for MssqlDriver {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn display_name(&self) -> &'static str {
        "Microsoft SQL Server"
    }

    fn validate(&self, config: &ConnectionConfig) -> Vec<String> {
        let mut errors = Vec::new();
        if config.params.host.is_none() {
            errors.push("host is required".into());
        }
        if config.params.username.is_none() && !integrated_security(&config.params.options) {
            errors.push("username is required (unless integrated_security=true)".into());
        }
        errors
    }

    async fn connect(
        &self,
        config: &ConnectionConfig,
        password: Option<&str>,
    ) -> Result<Box<dyn narwhal_core::DynConnection>> {
        let cfg = build_config(config, password)?;
        let connect_timeout = read_connect_timeout(&config.params.options)?;
        let addr = cfg.get_addr();
        debug!(
            target: "narwhal::mssql",
            host = %addr,
            connect_timeout_ms = connect_timeout.as_millis() as u64,
            "establishing connection"
        );

        // Whole-handshake budget: TCP connect + TLS handshake + LOGIN7
        // all share the same timeout. A misconfigured host that black-
        // holes packets must not wedge the TUI — default 10s, override
        // via `connect_timeout` option.
        let connect_fut = async {
            let tcp = TcpStream::connect(&addr)
                .await
                .map_err(|e| Error::connection_with("tcp connect", e))?;
            tcp.set_nodelay(true)
                .map_err(|e| Error::connection_with("set_nodelay", e))?;
            Client::connect(cfg, tcp.compat_write())
                .await
                .map_err(map_tiberius_error_conn)
        };

        let client = match tokio::time::timeout(connect_timeout, connect_fut).await {
            Ok(result) => result?,
            Err(_elapsed) => {
                return Err(Error::Connection(format!(
                    "connect to {addr} timed out after {}s",
                    connect_timeout.as_secs(),
                )));
            }
        };

        info!(target: "narwhal::mssql", host = %addr, "connection established");

        Ok(Box::new(MssqlConnection {
            inner: Arc::new(Mutex::new(Some(client))),
            read_only: Arc::new(AtomicBool::new(config.params.read_only)),
        }))
    }
}

/// Whitelist of option keys accepted by [`build_config`]. Anything
/// outside this set is rejected at connect time so a typo or attacker-
/// supplied options blob can't silently flip security-sensitive
/// settings.
const OPTIONS_WHITELIST: &[&str] = &[
    "application_name",
    "trust_server_certificate",
    "integrated_security",
    "instance_name",
    "encrypt",
    "connect_timeout",
];

const fn default_port() -> u16 {
    1433
}

/// Default whole-handshake budget when the connection does not set
/// `connect_timeout` explicitly. Picked to be longer than a healthy
/// LAN handshake (well under a second) and shorter than the TCP
/// SYN retransmit default (~127s on Linux) so a black-holed host
/// fails fast.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Read the `connect_timeout` option (seconds) or fall back to the
/// default. Pulled out so the connect path stays linear.
fn read_connect_timeout(options: &std::collections::BTreeMap<String, String>) -> Result<Duration> {
    match options.get("connect_timeout") {
        None => Ok(DEFAULT_CONNECT_TIMEOUT),
        Some(raw) => {
            let secs: u64 = raw
                .parse()
                .map_err(|_| Error::Config(format!("invalid connect_timeout: {raw}")))?;
            if secs == 0 {
                return Err(Error::Config("connect_timeout must be > 0 seconds".into()));
            }
            Ok(Duration::from_secs(secs))
        }
    }
}

/// Reads the `integrated_security` option case-insensitively, matching
/// the parser used by `trust_server_certificate` / `encrypt`. M2 fix:
/// the old version only matched lower-case `true`/`yes`/`1` so a user
/// writing `True` would land on the SQL-Auth path and see an unhelpful
/// `username missing` error.
fn integrated_security(options: &std::collections::BTreeMap<String, String>) -> bool {
    match options.get("integrated_security") {
        Some(raw) => parse_bool_option("integrated_security", raw).unwrap_or(false),
        None => false,
    }
}

fn parse_bool_option(key: &str, value: &str) -> Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" => Ok(true),
        "false" | "no" | "0" => Ok(false),
        other => Err(Error::Config(format!("invalid boolean for {key}: {other}"))),
    }
}

/// Translate [`narwhal_core::ConnectionParams`] + the credential store
/// password into a tiberius [`Config`]. Pure function so unit tests can
/// exercise it without spinning up a real server.
pub(crate) fn build_config(config: &ConnectionConfig, password: Option<&str>) -> Result<Config> {
    let host = config
        .params
        .host
        .as_deref()
        .ok_or_else(|| Error::Config("host missing".into()))?;

    let mut cfg = Config::new();
    cfg.host(host);
    cfg.port(config.params.port.unwrap_or_else(default_port));
    if let Some(db) = config.params.database.as_deref() {
        cfg.database(db);
    }

    // Whitelist validation must happen before we react to individual
    // option keys so a typo (e.g. `applicaiton_name`) surfaces as a
    // hard error rather than a silent default.
    for key in config.params.options.keys() {
        if !OPTIONS_WHITELIST.contains(&key.as_str()) {
            return Err(Error::Config(format!(
                "unsupported connection option: {key}"
            )));
        }
    }

    if let Some(name) = config.params.options.get("application_name") {
        cfg.application_name(name);
    }

    if let Some(instance) = config.params.options.get("instance_name") {
        if instance.trim().is_empty() {
            return Err(Error::Config(
                "instance_name must not be empty when set".into(),
            ));
        }
        // Named instance support requires SQL Browser; we don't enable
        // the `sql-browser-tokio` feature because it brings in a
        // separate UDP discovery path. Document the workaround:
        // resolve the dynamic port out-of-band (e.g. `sqlcmd -L`) and
        // set `port=` explicitly.
        cfg.instance_name(instance);
    }

    // Auth: SQL Auth by default. Windows-integrated (SSPI) and
    // GSSAPI/Kerberos are gated behind separate tiberius features
    // (`winauth`, `integrated-auth-gssapi`) which we do **not** enable
    // here — the brief explicitly defers Linux Kerberos to v2.4. So
    // any `integrated_security=true` request is rejected at config-
    // build time with a clear, actionable message rather than
    // failing later with an opaque tiberius error.
    if integrated_security(&config.params.options) {
        return Err(Error::Config(
            "integrated_security=true is not supported in this build; \
             rebuild narwhal-drivers with tiberius `winauth` (Windows) \
             or `integrated-auth-gssapi` (Linux) features, or use SQL \
             Server authentication"
                .into(),
        ));
    }
    let user = config
        .params
        .username
        .as_deref()
        .ok_or_else(|| Error::Config("username missing".into()))?;
    // explicit warning when an empty password lands at the auth
    // layer. SQL Server still accepts empty-password connections when
    // the account is so configured (typically misconfigured dev SAs),
    // and silent acceptance has bitten operators before.
    let pw = password.unwrap_or("");
    if pw.is_empty() {
        warn!(
            target: "narwhal::mssql",
            user = %user,
            "connecting with empty password; verify this is intentional"
        );
    }
    cfg.authentication(AuthMethod::sql_server(user, pw));

    // Encryption: ssl_mode → EncryptionLevel.
    let encryption = match config.params.ssl_mode {
        SslMode::Disable => EncryptionLevel::NotSupported,
        SslMode::Prefer | SslMode::Require => EncryptionLevel::On,
        SslMode::VerifyCa | SslMode::VerifyFull => EncryptionLevel::Required,
        // Future SslMode variants: fail safe (full encryption).
        _ => EncryptionLevel::Required,
    };
    cfg.encryption(encryption);

    // Trust knobs.
    if let Some(raw) = config.params.options.get("trust_server_certificate") {
        if parse_bool_option("trust_server_certificate", raw)? {
            cfg.trust_cert();
        }
    }
    if let Some(ca_path) = &config.params.ssl_root_cert {
        cfg.trust_cert_ca(ca_path.to_string_lossy().as_ref());
    }

    // Explicit `encrypt=` overrides ssl_mode for backward compat with
    // the ADO.NET / JDBC connection-string convention some users
    // expect. Honoured last so it wins against the ssl_mode default.
    if let Some(raw) = config.params.options.get("encrypt") {
        match raw.to_ascii_lowercase().as_str() {
            "danger_plaintext" => cfg.encryption(EncryptionLevel::NotSupported),
            "false" | "no" | "0" => cfg.encryption(EncryptionLevel::Off),
            "true" | "yes" | "1" => cfg.encryption(EncryptionLevel::Required),
            other => {
                return Err(Error::Config(format!(
                    "invalid encrypt value: {other} \
                     (use true|false|DANGER_PLAINTEXT)"
                )));
            }
        }
    }

    Ok(cfg)
}

/// Wrap a tiberius error in our [`Error`] taxonomy. Cancellation is
/// reported as [`Error::Cancelled`] when the server hands us the
/// well-known TDS 1205 (deadlock victim) or the synthetic message
/// tiberius emits on local cancel; everything else is bucketed as
/// `Connection` or `Query` depending on the wire phase. The source
/// chain is preserved via [`Error::query_with`] so downstream
/// debugging (`Error::find_source::<tiberius::error::Error>()`)
/// keeps working.
fn map_tiberius_error(error: tiberius::error::Error) -> Error {
    // SQL Server lets the client distinguish "connection setup failed"
    // (auth, TLS, instance not found) from "in-flight query failed"
    // (syntax error, deadlock, perm denied). At the tiberius level
    // both arrive as `tiberius::error::Error`; we use the error
    // variant to disambiguate.
    use tiberius::error::Error as Te;
    match error {
        Te::Server(ref token) if token.code() == 1205 => Error::Cancelled,
        Te::Server(_) | Te::Conversion(_) => Error::query_with("tiberius query failed", error),
        Te::Tls(_) | Te::Routing { .. } | Te::Protocol(_) => {
            Error::connection_with("tiberius connection failed", error)
        }
        _ => Error::connection_with("tiberius error", error),
    }
}

/// Same as [`map_tiberius_error`] but always tags the result as a
/// connection error. Used on the connect handshake path where
/// `Te::Server` (login failure) is conceptually a connection problem,
/// not a query problem.
fn map_tiberius_error_conn(error: tiberius::error::Error) -> Error {
    Error::connection_with("tiberius login failed", error)
}

/// Connection handle. The underlying tiberius client lives behind an
/// `Arc<Mutex<Option<_>>>` so:
///
/// 1. `&mut self` on the trait surface translates into a single async
/// mutex lock — tiberius itself requires `&mut Client` for every
/// call.
/// 2. `close(self: Box<Self>)` can take the client out of the option
/// and drop it (tiberius has no explicit close, but dropping the
/// Client closes the TCP socket).
/// 3. The cancel-handle path (had we implemented one) could clone the
/// `Arc` without holding the mutex.
pub struct MssqlConnection {
    inner: Arc<Mutex<Option<TiberiusClient>>>,
    /// C2: driver-level read-only enforcement. SQL Server has no
    /// per-session READ-ONLY mode at the engine level (read-only is a
    /// per-database `ALTER DATABASE` flag). When this flag is set,
    /// [`Connection::execute`] refuses any mutating statement (as
    /// determined by [`classify_statement`]) before it reaches the
    /// wire. This matches the contract of
    /// [`Connection::set_read_only`]: the driver guarantees write
    /// refusal for the lifetime of the session.
    ///
    /// Initialised from `ConnectionParams.read_only` so the value the
    /// user configured on disk is honoured from the first statement,
    /// even before a TUI ever calls `set_read_only`.
    read_only: Arc<AtomicBool>,
}

impl MssqlConnection {
    /// Helper that locks the mutex and runs `f` against the live
    /// client, surfacing a clear error if the connection has already
    /// been closed.
    async fn with_client<R, F>(&self, f: F) -> Result<R>
    where
        F: for<'a> FnOnce(
            &'a mut TiberiusClient,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<R>> + Send + 'a>,
        >,
    {
        let mut guard = self.inner.lock().await;
        let client = guard
            .as_mut()
            .ok_or_else(|| Error::Connection("connection closed".into()))?;
        f(client).await
    }

    /// Query path: routes through `Client::query` and returns the
    /// materialised row set. Used for `Read` and `MutatingWithRows`
    /// (OUTPUT / EXEC / CTE-with-mutation) shapes.
    ///
    /// `rows_affected` is always `None` from this path because
    /// tiberius' `QueryStream` does not surface the DONE-token
    /// rowcount. Callers that want the affected count for a known
    /// OUTPUT-DML synthesize it from `rows.len()` (see
    /// [`Connection::execute`]).
    async fn run(&self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        let owned_sql = sql.to_owned();
        let owned_params: Vec<Value> = params.to_vec();
        let started = Instant::now();

        self.with_client(move |client| {
            Box::pin(async move {
                let bindings: Vec<Param<'_>> = owned_params.iter().map(Param).collect();
                let binding_refs: Vec<&dyn tiberius::ToSql> =
                    bindings.iter().map(|p| p as &dyn tiberius::ToSql).collect();

                let mut stream = client
                    .query(owned_sql.as_str(), &binding_refs[..])
                    .await
                    .map_err(map_tiberius_error)?;

                let mut columns: Vec<ColumnHeader> = Vec::new();
                let mut column_types: Vec<tiberius::ColumnType> = Vec::new();
                let mut rows: Vec<CoreRow> = Vec::new();

                while let Some(item) = stream.try_next().await.map_err(map_tiberius_error)? {
                    match item {
                        QueryItem::Metadata(meta) => {
                            // The first metadata frame describes the
                            // entire result set; subsequent frames
                            // would belong to additional result sets
                            // produced by `SELECT 1; SELECT 2`. We
                            // keep the columns from the first frame —
                            // multi-result queries land all rows
                            // serialised into the same Vec, which
                            // matches the postgres driver's behaviour
                            // for batched statements.
                            if columns.is_empty() {
                                columns = meta.columns().iter().map(column_header).collect();
                                column_types = meta
                                    .columns()
                                    .iter()
                                    .map(tiberius::Column::column_type)
                                    .collect();
                            }
                        }
                        QueryItem::Row(row) => {
                            let mut values = Vec::with_capacity(column_types.len());
                            for (idx, ty) in column_types.iter().enumerate() {
                                values.push(column_to_value(&row, idx, *ty)?);
                            }
                            rows.push(CoreRow(values));
                        }
                    }
                }

                // `rows_affected` is only meaningful for DML; the
                // `execute()` trait method routes those through
                // `run_execute` (Client::execute), which exposes the
                // affected-row count. Anything that lands here is a
                // row-returning query — leave the field None.
                Ok(QueryResult {
                    columns,
                    rows,
                    rows_affected: None,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                })
            })
        })
        .await
    }

    /// DML/DDL path that needs the affected-row count. Used for
    /// `execute()` when the statement is clearly mutating.
    async fn run_execute(&self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        let owned_sql = sql.to_owned();
        let owned_params: Vec<Value> = params.to_vec();
        let started = Instant::now();

        self.with_client(move |client| {
            Box::pin(async move {
                let bindings: Vec<Param<'_>> = owned_params.iter().map(Param).collect();
                let binding_refs: Vec<&dyn tiberius::ToSql> =
                    bindings.iter().map(|p| p as &dyn tiberius::ToSql).collect();

                let result = client
                    .execute(owned_sql.as_str(), &binding_refs[..])
                    .await
                    .map_err(map_tiberius_error)?;

                let total: u64 = result.rows_affected().iter().sum();

                Ok(QueryResult {
                    columns: Vec::new(),
                    rows: Vec::new(),
                    rows_affected: Some(total),
                    elapsed_ms: started.elapsed().as_millis() as u64,
                })
            })
        })
        .await
    }
}

impl Connection for MssqlConnection {
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        let shape = classify_statement(sql);

        // C2: driver-level read-only enforcement. Refuse mutating
        // statements before they reach the wire, mirroring the
        // contract of `set_read_only` on the trait.
        if self.read_only.load(Ordering::Relaxed) && !matches!(shape, StatementShape::Read) {
            return Err(Error::Unsupported(
                "connection is in read-only mode; mutating SQL refused at driver layer".into(),
            ));
        }

        // C1: route based on the parsed statement shape rather than
        // a bare leading-keyword match. The classifier skips comments
        // and string literals and peeks past CTEs so:
        //
        // - `-- log\nINSERT …`        → Mutating  (was: lost rows_affected)
        // - `WITH cte AS (…) INSERT …` → MutatingWithRows
        // - `INSERT … OUTPUT inserted.*` → MutatingWithRows (was: rows dropped)
        // - `EXEC sp_who`               → MutatingWithRows (was: rows dropped)
        match shape {
            StatementShape::Read => self.run(sql, params).await,
            StatementShape::Mutating => self.run_execute(sql, params).await,
            StatementShape::MutatingWithRows => {
                // tiberius' QueryStream surfaces every row from an
                // OUTPUT clause / EXEC / mutating CTE; we keep them.
                // The DONE-token rowcount is unrecoverable from this
                // path, so we synthesise rows_affected from rows.len()
                // for plain DML+OUTPUT (1:1 correspondence) and leave
                // it None for EXEC / mutating CTE where the row count
                // and affected count are not equivalent.
                let mut result = self.run(sql, params).await?;
                if has_dml_with_output(sql) {
                    result.rows_affected = Some(result.rows.len() as u64);
                }
                Ok(result)
            }
        }
    }

    async fn stream(
        &mut self,
        sql: &str,
        params: &[Value],
    ) -> Result<Box<dyn narwhal_core::DynRowStream>> {
        // Tiberius' QueryStream borrows `&mut Client` for its
        // lifetime, which would tie the stream to the mutex guard.
        // We materialise like the mysql driver does (bug H5
        // companion) and replay through a BufferedRowStream. Real
        // server-side cursoring (FAST_FORWARD) is a v2.x follow-up.
        let materialised = self.execute(sql, params).await?;
        Ok(Box::new(BufferedRowStream {
            columns: materialised.columns,
            rows: materialised.rows.into_iter(),
        }))
    }

    async fn begin(&mut self) -> Result<()> {
        // SQL Server's auto-commit mode is on by default; explicit
        // transactions start with BEGIN TRAN. Use `simple_query` so
        // the statement bypasses the prepared-statement protocol —
        // SQL Server refuses to prepare transaction control.
        self.with_client(|client| {
            Box::pin(async move {
                client
                    .simple_query("BEGIN TRAN")
                    .await
                    .map_err(map_tiberius_error)?
                    .into_results()
                    .await
                    .map_err(map_tiberius_error)?;
                Ok(())
            })
        })
        .await
    }

    async fn begin_with(&mut self, isolation: IsolationLevel) -> Result<()> {
        let level = match isolation {
            IsolationLevel::ReadUncommitted => "READ UNCOMMITTED",
            IsolationLevel::ReadCommitted => "READ COMMITTED",
            IsolationLevel::RepeatableRead => "REPEATABLE READ",
            IsolationLevel::Serializable => "SERIALIZABLE",
            _ => "SERIALIZABLE",
        };
        let stmt = format!("SET TRANSACTION ISOLATION LEVEL {level}; BEGIN TRAN");
        self.with_client(move |client| {
            Box::pin(async move {
                client
                    .simple_query(stmt.as_str())
                    .await
                    .map_err(map_tiberius_error)?
                    .into_results()
                    .await
                    .map_err(map_tiberius_error)?;
                Ok(())
            })
        })
        .await
    }

    async fn commit(&mut self) -> Result<()> {
        self.with_client(|client| {
            Box::pin(async move {
                client
                    .simple_query("COMMIT TRAN")
                    .await
                    .map_err(map_tiberius_error)?
                    .into_results()
                    .await
                    .map_err(map_tiberius_error)?;
                Ok(())
            })
        })
        .await
    }

    async fn rollback(&mut self) -> Result<()> {
        self.with_client(|client| {
            Box::pin(async move {
                client
                    .simple_query("ROLLBACK TRAN")
                    .await
                    .map_err(map_tiberius_error)?
                    .into_results()
                    .await
                    .map_err(map_tiberius_error)?;
                Ok(())
            })
        })
        .await
    }

    async fn savepoint(&mut self, name: &str) -> Result<()> {
        let stmt = format!("SAVE TRAN {}", quote_ident(name));
        self.with_client(move |client| {
            Box::pin(async move {
                client
                    .simple_query(stmt.as_str())
                    .await
                    .map_err(map_tiberius_error)?
                    .into_results()
                    .await
                    .map_err(map_tiberius_error)?;
                Ok(())
            })
        })
        .await
    }

    async fn release_savepoint(&mut self, _name: &str) -> Result<()> {
        // SQL Server does not have a SAVEPOINT release counterpart —
        // savepoints disappear when the surrounding TRAN ends. Report
        // as a no-op rather than `unsupported`, which would break
        // generic transaction wrappers that always call release.
        Ok(())
    }

    async fn rollback_to_savepoint(&mut self, name: &str) -> Result<()> {
        let stmt = format!("ROLLBACK TRAN {}", quote_ident(name));
        self.with_client(move |client| {
            Box::pin(async move {
                client
                    .simple_query(stmt.as_str())
                    .await
                    .map_err(map_tiberius_error)?
                    .into_results()
                    .await
                    .map_err(map_tiberius_error)?;
                Ok(())
            })
        })
        .await
    }

    async fn list_schemas(&mut self) -> Result<Vec<Schema>> {
        const SQL: &str = "
            SELECT name FROM sys.schemas
            WHERE name NOT IN ('sys', 'INFORMATION_SCHEMA', 'guest',
                               'db_owner', 'db_accessadmin', 'db_securityadmin',
                               'db_ddladmin', 'db_backupoperator', 'db_datareader',
                               'db_datawriter', 'db_denydatareader',
                               'db_denydatawriter')
            ORDER BY name";
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
            SELECT t.name, 'U' AS kind FROM sys.tables t
              JOIN sys.schemas s ON s.schema_id = t.schema_id
             WHERE s.name = @P1
            UNION ALL
            SELECT v.name, 'V' AS kind FROM sys.views v
              JOIN sys.schemas s ON s.schema_id = v.schema_id
             WHERE s.name = @P1
            ORDER BY 1";
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
                    "V" => TableKind::View,
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
            SELECT s.name AS schema_name, t.name AS object_name, 'U' AS kind
              FROM sys.tables t
              JOIN sys.schemas s ON s.schema_id = t.schema_id
            UNION ALL
            SELECT s.name, v.name, 'V'
              FROM sys.views v
              JOIN sys.schemas s ON s.schema_id = v.schema_id
            ORDER BY 1, 2";
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
                    "V" => TableKind::View,
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
        // Columns come from INFORMATION_SCHEMA.COLUMNS — portable and
        // populated by every supported edition (Express → Enterprise,
        // Azure SQL DB). The PK lookup joins sys.key_constraints +
        // sys.index_columns so composite PKs surface correctly on each
        // column.
        const SQL: &str = "
            SELECT
                c.COLUMN_NAME,
                CASE
                    WHEN c.DATA_TYPE IN ('varchar','nvarchar','char','nchar','varbinary','binary')
                      AND c.CHARACTER_MAXIMUM_LENGTH IS NOT NULL THEN
                        c.DATA_TYPE + '(' +
                          CASE WHEN c.CHARACTER_MAXIMUM_LENGTH = -1
                               THEN 'max'
                               ELSE CAST(c.CHARACTER_MAXIMUM_LENGTH AS varchar(11))
                          END + ')'
                    WHEN c.DATA_TYPE IN ('decimal','numeric')
                      AND c.NUMERIC_PRECISION IS NOT NULL THEN
                        c.DATA_TYPE + '(' +
                          CAST(c.NUMERIC_PRECISION AS varchar(11)) + ',' +
                          CAST(COALESCE(c.NUMERIC_SCALE, 0) AS varchar(11)) + ')'
                    ELSE c.DATA_TYPE
                END AS data_type,
                CASE WHEN c.IS_NULLABLE = 'YES' THEN CAST(1 AS bit) ELSE CAST(0 AS bit) END,
                CASE WHEN pk.column_id IS NULL THEN CAST(0 AS bit) ELSE CAST(1 AS bit) END,
                c.COLUMN_DEFAULT
            FROM INFORMATION_SCHEMA.COLUMNS c
            LEFT JOIN (
                SELECT ic.object_id, ic.column_id
                  FROM sys.indexes i
                  JOIN sys.index_columns ic
                    ON ic.object_id = i.object_id AND ic.index_id = i.index_id
                 WHERE i.is_primary_key = 1
            ) pk
              ON pk.object_id = OBJECT_ID(QUOTENAME(c.TABLE_SCHEMA) + '.' + QUOTENAME(c.TABLE_NAME))
             AND pk.column_id = COLUMNPROPERTY(
                   OBJECT_ID(QUOTENAME(c.TABLE_SCHEMA) + '.' + QUOTENAME(c.TABLE_NAME)),
                   c.COLUMN_NAME, 'ColumnId')
            WHERE c.TABLE_SCHEMA = @P1 AND c.TABLE_NAME = @P2
            ORDER BY c.ORDINAL_POSITION";

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
            columns.push(narwhal_core::Column {
                name: col_name,
                data_type,
                nullable,
                primary_key,
                default,
            });
        }

        // Object kind: distinguish base table from view.
        let kind = self
            .fetch_kind(schema, name)
            .await
            .unwrap_or(TableKind::Table);

        // Best-effort secondary catalogues. Permission errors on a
        // single helper must not fail describe_table; mirror the
        // postgres driver's behaviour.
        let indexes = match self.list_indexes(schema, name).await {
            Ok(v) => v,
            Err(error) => {
                tracing::warn!(
                    target: "narwhal::mssql",
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
                    target: "narwhal::mssql",
                    schema, table = name, error = %error,
                    "list_foreign_keys failed; continuing without"
                );
                Vec::new()
            }
        };
        let unique_constraints = unique_constraints_from_indexes(&indexes);

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
        self.with_client(|client| {
            Box::pin(async move {
                client
                    .simple_query("SELECT 1")
                    .await
                    .map_err(map_tiberius_error)?
                    .into_results()
                    .await
                    .map_err(map_tiberius_error)?;
                Ok(())
            })
        })
        .await
    }

    /// Toggle session-level read-only enforcement.
    ///
    /// SQL Server has no per-session READ-ONLY flag at the engine
    /// level (the equivalent is a per-database `ALTER DATABASE … SET
    /// READ_ONLY`, which is global and disruptive). We therefore
    /// enforce it at the **driver** layer: every subsequent call to
    /// [`Connection::execute`] is screened by [`classify_statement`]
    /// and any mutating shape is refused with [`Error::Unsupported`].
    /// This satisfies the trait contract that "the driver instructs
    /// the database engine to refuse writes for the lifetime of the
    /// session" — the writes never reach the engine in the first
    /// place.
    ///
    /// Snapshot isolation is not used as a substitute because it does
    /// **not** by itself prevent writes; it only changes locking. The
    /// previous implementation issued `SET TRANSACTION ISOLATION
    /// LEVEL SNAPSHOT` and returned `Ok(())`, which silently lied
    /// about read-only enforcement.
    async fn set_read_only(&mut self, read_only: bool) -> Result<()> {
        self.read_only.store(read_only, Ordering::Relaxed);
        debug!(
            target: "narwhal::mssql",
            read_only,
            "driver-level read-only flag updated"
        );
        Ok(())
    }

    fn cancel_handle(&self) -> Option<Box<dyn narwhal_core::DynCancelHandle>> {
        None
    }

    fn capabilities(&self) -> Capabilities {
        MssqlDriver::capabilities()
    }

    async fn close(self: Box<Self>) -> Result<()> {
        let mut guard = self.inner.lock().await;
        // Dropping the client closes the TCP socket; tiberius has no
        // explicit close handshake.
        guard.take();
        // read_only Arc is dropped with self.
        Ok(())
    }
}

// ---------------------------------------------------------------------
// Internal helpers used by describe_table / fetch_ddl.
// ---------------------------------------------------------------------

impl MssqlConnection {
    /// Disambiguate base table vs view by object id.
    async fn fetch_kind(&self, schema: &str, name: &str) -> Result<TableKind> {
        const SQL: &str = "
            SELECT type FROM sys.objects
            WHERE schema_id = SCHEMA_ID(@P1) AND name = @P2 AND type IN ('U','V')";
        let result = self
            .run(
                SQL,
                &[
                    Value::String(schema.to_owned()),
                    Value::String(name.to_owned()),
                ],
            )
            .await?;
        let kind = result
            .rows
            .into_iter()
            .next()
            .and_then(|r| r.0.into_iter().next())
            .and_then(|v| match v {
                Value::String(s) => Some(s),
                _ => None,
            });
        Ok(match kind.as_deref() {
            Some(s) if s.trim() == "V" => TableKind::View,
            _ => TableKind::Table,
        })
    }

    /// Index list including the implicit PK index. Each row of the
    /// underlying join is one (index, column) pair; we fold into
    /// per-index column lists in name order.
    async fn list_indexes(&self, schema: &str, table: &str) -> Result<Vec<Index>> {
        const SQL: &str = "
            SELECT i.name, i.is_unique, i.is_primary_key, c.name AS column_name, ic.key_ordinal
              FROM sys.indexes i
              JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id
              JOIN sys.columns c        ON c.object_id  = ic.object_id AND c.column_id = ic.column_id
              JOIN sys.tables t         ON t.object_id  = i.object_id
              JOIN sys.schemas s        ON s.schema_id  = t.schema_id
             WHERE s.name = @P1 AND t.name = @P2 AND i.type > 0
             ORDER BY i.name, ic.key_ordinal";
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
            let column = match row.0.get(3) {
                Some(Value::String(s)) => s.clone(),
                _ => continue,
            };
            let entry = by_name.entry(name.clone()).or_insert(Index {
                name,
                columns: Vec::new(),
                unique,
                primary,
            });
            entry.columns.push(column);
        }
        Ok(by_name.into_values().collect())
    }

    /// Foreign-key list. The four sys.* catalogues join through
    /// `foreign_key_columns` which preserves ordinal position.
    async fn list_foreign_keys(&self, schema: &str, table: &str) -> Result<Vec<ForeignKey>> {
        const SQL: &str = "
            SELECT fk.name,
                   c1.name        AS column_name,
                   ref_s.name     AS ref_schema,
                   ref_t.name     AS ref_table,
                   c2.name        AS ref_column,
                   fk.update_referential_action_desc,
                   fk.delete_referential_action_desc
              FROM sys.foreign_keys fk
              JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id
              JOIN sys.tables   t     ON t.object_id     = fk.parent_object_id
              JOIN sys.schemas  s     ON s.schema_id     = t.schema_id
              JOIN sys.columns  c1    ON c1.object_id    = fk.parent_object_id
                                     AND c1.column_id    = fkc.parent_column_id
              JOIN sys.tables   ref_t ON ref_t.object_id = fk.referenced_object_id
              JOIN sys.schemas  ref_s ON ref_s.schema_id = ref_t.schema_id
              JOIN sys.columns  c2    ON c2.object_id    = fk.referenced_object_id
                                     AND c2.column_id    = fkc.referenced_column_id
             WHERE s.name = @P1 AND t.name = @P2
             ORDER BY fk.name, fkc.constraint_column_id";
        let rows = self
            .run(
                SQL,
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
                Value::String(s) => map_referential_action(s),
                _ => None,
            });
            let on_delete = row.0.get(6).and_then(|v| match v {
                Value::String(s) => map_referential_action(s),
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
}

/// SQL Server reports `update_referential_action_desc` as one of
/// `NO_ACTION`, `CASCADE`, `SET_NULL`, `SET_DEFAULT`. Convert to our
/// engine-agnostic [`ReferentialAction`].
///
/// Fallback semantics (m7): SQL Server's default referential action
/// when no `ON UPDATE` / `ON DELETE` clause is declared is
/// `NO_ACTION`. Unknown or empty tokens therefore round-trip to
/// `Some(NoAction)` rather than `None` so downstream schema diffs
/// don't flag the default as "unknown".
fn map_referential_action(token: &str) -> Option<ReferentialAction> {
    match token.trim().to_ascii_uppercase().as_str() {
        "NO_ACTION" | "" => Some(ReferentialAction::NoAction),
        "CASCADE" => Some(ReferentialAction::Cascade),
        "SET_NULL" => Some(ReferentialAction::SetNull),
        "SET_DEFAULT" => Some(ReferentialAction::SetDefault),
        unknown => {
            tracing::debug!(
                target: "narwhal::mssql",
                token = unknown,
                "unknown referential_action_desc; mapping to NO_ACTION"
            );
            Some(ReferentialAction::NoAction)
        }
    }
}

/// Derive the unique-constraint list from the index set. The implicit
/// PK index is excluded because primary-key membership is reported via
/// `Column::primary_key` already.
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

/// Map the `information_schema` style `TABLE_TYPE` string into our
/// [`TableKind`]. Used by the test-only helper module.
fn map_table_kind(table_type: Option<&str>) -> TableKind {
    match table_type {
        Some("VIEW" | "V") => TableKind::View,
        Some("SYSTEM TABLE" | "SYSTEM VIEW") => TableKind::SystemTable,
        _ => TableKind::Table,
    }
}

/// SQL keywords that, when they appear at the head of a statement,
/// indicate a mutating shape — the statement should run through
/// `Client::execute` so `rows_affected` is captured.
const MUTATING_KEYWORDS: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "MERGE", "TRUNCATE", "CREATE", "ALTER", "DROP", "GRANT", "REVOKE",
];

/// DML verbs that may carry an OUTPUT clause. EXEC is treated
/// separately because it can return rows even without OUTPUT.
const DML_VERBS_WITH_OUTPUT: &[&str] = &["INSERT", "UPDATE", "DELETE", "MERGE"];

/// What kind of statement is this, for the purpose of routing to
/// tiberius' `query` (preserves rows) vs `execute` (preserves
/// `rows_affected`)?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementShape {
    /// Pure read (SELECT, or WITH-CTE feeding a SELECT). Use
    /// `Client::query`. `rows_affected` is `None`.
    Read,
    /// Pure mutating with no returned rows expected (INSERT without
    /// OUTPUT, UPDATE without OUTPUT, DDL, GRANT, …). Use
    /// `Client::execute` to capture `rows_affected`.
    Mutating,
    /// Mutating that may also return rows: INSERT/UPDATE/DELETE/MERGE
    /// with an OUTPUT clause, EXEC of a stored procedure, or a
    /// mutating CTE. Use `Client::query` — we preserve the rows; for
    /// pure DML+OUTPUT, [`Connection::execute`] synthesises
    /// `rows_affected` from `rows.len()` (1:1 correspondence). For
    /// EXEC and mutating CTEs the rowcount and the affected-row
    /// count are not equivalent, so `rows_affected` stays `None`.
    MutatingWithRows,
}

/// Classify the statement. Comment-aware and CTE-aware; see the unit
/// tests at the bottom of this file for the full case grid.
fn classify_statement(sql: &str) -> StatementShape {
    let head = leading_keyword(sql).map(str::to_ascii_uppercase);
    match head.as_deref() {
        Some(verb)
            if DML_VERBS_WITH_OUTPUT.contains(&verb)
                && contains_top_level_keyword(sql, "OUTPUT") =>
        {
            StatementShape::MutatingWithRows
        }
        Some(verb) if MUTATING_KEYWORDS.contains(&verb) => StatementShape::Mutating,
        Some("EXEC" | "EXECUTE") => StatementShape::MutatingWithRows,
        Some("WITH") => {
            // A CTE is read-only unless its body invokes a DML verb.
            // We peek for INSERT/UPDATE/DELETE/MERGE as top-level
            // identifiers; this is pessimistic (any keyword match
            // routes to the query path, which preserves rows) and
            // matches the safety bias of mistakes — a mis-classified
            // mutating CTE on the Read path would silently lose
            // server-side state.
            if DML_VERBS_WITH_OUTPUT
                .iter()
                .any(|kw| contains_top_level_keyword(sql, kw))
            {
                StatementShape::MutatingWithRows
            } else {
                StatementShape::Read
            }
        }
        // Unknown / empty / SELECT / SET / DECLARE / IF / BEGIN-tx:
        // safe default is Read so any returned rows are preserved.
        _ => StatementShape::Read,
    }
}

/// True if `sql` is a known DML verb carrying an OUTPUT clause. Used
/// to decide whether to synthesise `rows_affected = Some(rows.len())`
/// in [`Connection::execute`].
fn has_dml_with_output(sql: &str) -> bool {
    matches!(
        leading_keyword(sql).map(str::to_ascii_uppercase).as_deref(),
        Some("INSERT" | "UPDATE" | "DELETE" | "MERGE")
    ) && contains_top_level_keyword(sql, "OUTPUT")
}

/// Return the first SQL keyword in `sql`, skipping ASCII whitespace,
/// `--` line comments and `/* … */` block comments. Returns `None` if
/// the input is empty or comment-only.
///
/// Identifiers may include underscores and digits after the first
/// character; we follow the standard SQL `[_A-Za-z][_A-Za-z0-9]*`
/// shape so `EXEC_PROC` doesn't get matched as `EXEC`.
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
            i = (i + 2).min(bytes.len());
            continue;
        }
        break;
    }
    let start = i;
    if i >= bytes.len() || !(bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
        return None;
    }
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    Some(&sql[start..i])
}

/// Case-insensitive whole-word search that skips comments and single-
/// quoted string literals. Used by [`classify_statement`] to detect
/// `OUTPUT` clauses and mutating verbs inside CTE bodies without
/// false-positive matches on literals like `'INSERT failed'`.
///
/// Pessimistic by design: anything we miss routes to the safer side
/// of the classifier (rows-preserving query path).
fn contains_top_level_keyword(sql: &str, keyword: &str) -> bool {
    let bytes = sql.as_bytes();
    let kw = keyword.as_bytes();
    let kw_len = kw.len();
    if kw_len == 0 {
        return false;
    }
    let mut i = 0;
    while i < bytes.len() {
        // -- line comment
        if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // /* block comment */
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        // Single-quoted string literal (SQL uses '' to escape ').
        if bytes[i] == b'\'' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\'' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        // Bracketed identifier [foo] — skip to its closing bracket so
        // a literal column name like [OUTPUT] does not match.
        if bytes[i] == b'[' {
            i += 1;
            while i < bytes.len() && bytes[i] != b']' {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
            continue;
        }
        // Word boundary on the left.
        let at_word_start = i == 0 || !is_word_byte(bytes[i - 1]);
        if at_word_start && i + kw_len <= bytes.len() {
            let slice = &bytes[i..i + kw_len];
            let matches_ci = slice
                .iter()
                .zip(kw.iter())
                .all(|(a, b)| a.eq_ignore_ascii_case(b));
            let at_word_end = i + kw_len == bytes.len() || !is_word_byte(bytes[i + kw_len]);
            if matches_ci && at_word_end {
                return true;
            }
        }
        i += 1;
    }
    false
}

const fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// SQL Server identifier quoting using bracketed form `[name]` with
/// `]]` escape. Matches what `QUOTENAME()` would produce server-side.
fn quote_ident(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

struct BufferedRowStream {
    columns: Vec<ColumnHeader>,
    rows: std::vec::IntoIter<CoreRow>,
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use narwhal_core::ConnectionParams;
    use uuid::Uuid;

    fn config(params: ConnectionParams) -> ConnectionConfig {
        ConnectionConfig {
            id: Uuid::nil(),
            name: "test".into(),
            driver: MssqlDriver::NAME.into(),
            params,
        }
    }

    #[test]
    fn validate_reports_missing_fields() {
        let driver = MssqlDriver::new();
        let errors = driver.validate(&config(ConnectionParams::default()));
        // host + username (no integrated_security set)
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn validate_accepts_integrated_security_without_username() {
        let driver = MssqlDriver::new();
        let mut options = std::collections::BTreeMap::new();
        options.insert("integrated_security".into(), "true".into());
        let params = ConnectionParams::with(|p| {
            p.host = Some("db".into());
            p.options = options;
        });
        let errors = driver.validate(&config(params));
        assert!(errors.is_empty(), "got {errors:?}");
    }

    #[test]
    fn build_config_with_sql_auth() {
        let mut options = std::collections::BTreeMap::new();
        options.insert("application_name".into(), "narwhal".into());
        let params = ConnectionParams::with(|p| {
            p.host = Some("sql.local".into());
            p.port = Some(1433);
            p.database = Some("appdb".into());
            p.username = Some("sa".into());
            p.options = options;
        });
        let cfg = config(params);
        let tib_cfg = build_config(&cfg, Some("hunter2")).expect("build cfg");
        assert_eq!(tib_cfg.get_addr(), "sql.local:1433");
    }

    #[test]
    fn build_config_rejects_unknown_option() {
        let mut options = std::collections::BTreeMap::new();
        options.insert("evil_inject".into(), "value".into());
        let params = ConnectionParams::with(|p| {
            p.host = Some("db".into());
            p.username = Some("sa".into());
            p.options = options;
        });
        let err = build_config(&config(params), Some("pw")).unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported connection option: evil_inject"),
            "got: {err}"
        );
    }

    #[test]
    fn build_config_trust_server_certificate_parses() {
        let mut options = std::collections::BTreeMap::new();
        options.insert("trust_server_certificate".into(), "yes".into());
        let params = ConnectionParams::with(|p| {
            p.host = Some("db".into());
            p.username = Some("sa".into());
            p.options = options;
        });
        assert!(build_config(&config(params), Some("pw")).is_ok());
    }

    #[test]
    fn build_config_invalid_encrypt_rejected() {
        let mut options = std::collections::BTreeMap::new();
        options.insert("encrypt".into(), "maybe".into());
        let params = ConnectionParams::with(|p| {
            p.host = Some("db".into());
            p.username = Some("sa".into());
            p.options = options;
        });
        let err = build_config(&config(params), Some("pw")).unwrap_err();
        assert!(err.to_string().contains("invalid encrypt"), "got: {err}");
    }

    #[test]
    fn integrated_security_rejected_without_winauth_feature() {
        // This build ships without the `winauth` /
        // `integrated-auth-gssapi` tiberius features, so any caller
        // that asks for integrated security must get a clear error.
        let mut options = std::collections::BTreeMap::new();
        options.insert("integrated_security".into(), "true".into());
        let params = ConnectionParams::with(|p| {
            p.host = Some("db".into());
            p.options = options;
        });
        let err = build_config(&config(params), None).unwrap_err();
        assert!(err.to_string().contains("integrated_security=true"));
    }

    #[test]
    fn quote_ident_doubles_close_bracket() {
        assert_eq!(quote_ident("my]table"), "[my]]table]");
        assert_eq!(quote_ident("plain"), "[plain]");
    }

    #[test]
    fn classify_statement_routes_basic_shapes() {
        use StatementShape::*;
        assert_eq!(classify_statement("SELECT 1"), Read);
        assert_eq!(classify_statement("  select 1"), Read);
        assert_eq!(classify_statement("INSERT INTO t VALUES (1)"), Mutating);
        assert_eq!(classify_statement("  update t SET x = 1"), Mutating);
        assert_eq!(classify_statement("DELETE FROM t"), Mutating);
        assert_eq!(classify_statement("CREATE TABLE foo (id int)"), Mutating);
        assert_eq!(classify_statement("DROP TABLE foo"), Mutating);
    }

    #[test]
    fn classify_statement_handles_comments() {
        // C1: leading line and block comments must not hide the real
        // verb. Previously the classifier returned Read for these
        // and we lost rows_affected.
        use StatementShape::*;
        assert_eq!(
            classify_statement("-- audit\nINSERT INTO t VALUES (1)"),
            Mutating
        );
        assert_eq!(
            classify_statement("/* batched */\n  UPDATE t SET x=1"),
            Mutating
        );
        assert_eq!(
            classify_statement("-- nested -- comment\n/* block */ DELETE FROM t"),
            Mutating
        );
        assert_eq!(
            classify_statement("/* multi\nline\nblock */ SELECT 1"),
            Read
        );
    }

    #[test]
    fn classify_statement_detects_output_clause() {
        // C1: INSERT/UPDATE/DELETE with OUTPUT returns rows that must
        // not be discarded. Route to MutatingWithRows so run() is
        // used (preserves rows) and rows_affected is synthesised.
        use StatementShape::*;
        assert_eq!(
            classify_statement("INSERT INTO t OUTPUT inserted.id VALUES (1)"),
            MutatingWithRows
        );
        assert_eq!(
            classify_statement("UPDATE t SET x=1 OUTPUT deleted.x, inserted.x WHERE id=1"),
            MutatingWithRows
        );
        assert_eq!(
            classify_statement("DELETE FROM t OUTPUT deleted.* WHERE id=1"),
            MutatingWithRows
        );
    }

    #[test]
    fn classify_statement_handles_cte() {
        // CTE feeding a SELECT is Read; CTE feeding a DML verb is
        // MutatingWithRows.
        use StatementShape::*;
        assert_eq!(
            classify_statement("WITH cte AS (SELECT 1) SELECT * FROM cte"),
            Read
        );
        assert_eq!(
            classify_statement(
                "WITH cte AS (SELECT id FROM src) \
                 INSERT INTO target (id) SELECT id FROM cte"
            ),
            MutatingWithRows
        );
    }

    #[test]
    fn classify_statement_handles_exec() {
        use StatementShape::*;
        assert_eq!(classify_statement("EXEC sp_who"), MutatingWithRows);
        assert_eq!(
            classify_statement("execute dbo.usp_get_users"),
            MutatingWithRows
        );
    }

    #[test]
    fn classify_statement_does_not_partial_match_keyword() {
        // C1: `EXECUTE_PROC` should NOT match `EXECUTE`. The new
        // leading-keyword reader uses identifier boundaries.
        use StatementShape::*;
        assert_eq!(classify_statement("INSERTED_AT something"), Read);
        assert_eq!(classify_statement("UPDATER_FN()"), Read);
    }

    #[test]
    fn contains_top_level_keyword_skips_literals() {
        // OUTPUT inside a string literal must NOT count.
        assert!(!contains_top_level_keyword(
            "INSERT INTO log VALUES ('OUTPUT not a clause')",
            "OUTPUT"
        ));
        // OUTPUT inside a bracketed identifier likewise must not count.
        assert!(!contains_top_level_keyword(
            "SELECT [OUTPUT] FROM t",
            "OUTPUT"
        ));
        // OUTPUT inside a line comment likewise.
        assert!(!contains_top_level_keyword(
            "INSERT INTO t VALUES (1) -- OUTPUT inserted.*",
            "OUTPUT"
        ));
        // OUTPUT as a real clause must count.
        assert!(contains_top_level_keyword(
            "INSERT INTO t OUTPUT inserted.id VALUES (1)",
            "OUTPUT"
        ));
        // Whole-word: PASSOUTPUT does not match.
        assert!(!contains_top_level_keyword("PASSOUTPUT()", "OUTPUT"));
    }

    #[test]
    fn leading_keyword_handles_underscore_and_digits() {
        assert_eq!(leading_keyword("  exec_proc()"), Some("exec_proc"));
        assert_eq!(leading_keyword("-- comment\nSELECT 1"), Some("SELECT"));
        assert_eq!(leading_keyword("/* x */\nUPDATE t"), Some("UPDATE"));
        assert_eq!(leading_keyword(""), None);
        assert_eq!(leading_keyword("-- only comment"), None);
    }

    #[test]
    fn integrated_security_parses_case_insensitively() {
        // "True", "TRUE", "YES" must all read as integrated
        // security — the old parser only matched lower-case.
        for raw in ["true", "True", "TRUE", "yes", "YES", "1"] {
            let mut options = std::collections::BTreeMap::new();
            options.insert("integrated_security".into(), raw.into());
            assert!(
                integrated_security(&options),
                "`{raw}` should read as integrated security"
            );
        }
        for raw in ["false", "False", "NO", "0", ""] {
            let mut options = std::collections::BTreeMap::new();
            options.insert("integrated_security".into(), raw.into());
            assert!(
                !integrated_security(&options),
                "`{raw}` should NOT read as integrated security"
            );
        }
    }

    #[test]
    fn connect_timeout_defaults_and_parses() {
        let mut opts = std::collections::BTreeMap::new();
        assert_eq!(
            read_connect_timeout(&opts).unwrap(),
            DEFAULT_CONNECT_TIMEOUT
        );
        opts.insert("connect_timeout".into(), "30".into());
        assert_eq!(
            read_connect_timeout(&opts).unwrap(),
            std::time::Duration::from_secs(30)
        );
        opts.insert("connect_timeout".into(), "0".into());
        assert!(read_connect_timeout(&opts).is_err());
        opts.insert("connect_timeout".into(), "thirty".into());
        assert!(read_connect_timeout(&opts).is_err());
    }

    #[test]
    fn build_config_rejects_empty_instance_name() {
        let mut options = std::collections::BTreeMap::new();
        options.insert("instance_name".into(), String::new());
        let params = ConnectionParams::with(|p| {
            p.host = Some("db".into());
            p.username = Some("sa".into());
            p.options = options;
        });
        let err = build_config(&config(params), Some("pw")).unwrap_err();
        assert!(err.to_string().contains("instance_name"), "got: {err}");
    }

    #[test]
    fn referential_action_unknown_token_maps_to_no_action() {
        // m7: unknown FK action tokens should default to NoAction
        // (SQL Server's no-clause default) instead of None, which
        // would confuse Tier-2 schema diff.
        assert_eq!(
            map_referential_action(""),
            Some(ReferentialAction::NoAction)
        );
        assert_eq!(
            map_referential_action("FUTURE_ACTION"),
            Some(ReferentialAction::NoAction)
        );
    }

    #[test]
    fn unique_constraints_skip_primary_index() {
        let indexes = vec![
            Index {
                name: "PK_t".into(),
                columns: vec!["id".into()],
                unique: true,
                primary: true,
            },
            Index {
                name: "uk_email".into(),
                columns: vec!["email".into()],
                unique: true,
                primary: false,
            },
            Index {
                name: "ix_lookup".into(),
                columns: vec!["k".into()],
                unique: false,
                primary: false,
            },
        ];
        let uc = unique_constraints_from_indexes(&indexes);
        assert_eq!(uc.len(), 1);
        assert_eq!(uc[0].name, "uk_email");
    }

    #[test]
    fn referential_action_mapping() {
        assert_eq!(
            map_referential_action("NO_ACTION"),
            Some(ReferentialAction::NoAction)
        );
        assert_eq!(
            map_referential_action("cascade"),
            Some(ReferentialAction::Cascade)
        );
        assert_eq!(
            map_referential_action("SET_NULL"),
            Some(ReferentialAction::SetNull)
        );
        // m7: unknown tokens fall back to NO_ACTION (SQL Server's
        // implicit default) rather than None, so schema diff doesn't
        // misreport "unknown". The full behaviour is locked in by
        // `referential_action_unknown_token_maps_to_no_action`.
        assert_eq!(
            map_referential_action("UNKNOWN"),
            Some(ReferentialAction::NoAction)
        );
    }

    #[test]
    fn map_table_kind_handles_view_and_table() {
        assert_eq!(map_table_kind(Some("VIEW")), TableKind::View);
        assert_eq!(map_table_kind(Some("V")), TableKind::View);
        assert_eq!(map_table_kind(Some("SYSTEM VIEW")), TableKind::SystemTable);
        assert_eq!(map_table_kind(None), TableKind::Table);
    }

    #[test]
    fn capabilities_match_engine() {
        let caps = MssqlDriver::capabilities();
        assert!(caps.transactions);
        assert!(caps.multiple_schemas);
        assert!(caps.savepoints);
        assert!(!caps.cancellation);
        assert!(!caps.streaming);
    }
}
