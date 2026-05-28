// Trait definitions intentionally keep explicit `'a` lifetimes on the
// dyn-safe sibling methods: every borrowed parameter shares the same
// lifetime as the returned `BoxFuture`, which elision cannot express
// (multi-input borrows would each get an independent anonymous
// lifetime).
#![allow(clippy::needless_lifetimes, clippy::elidable_lifetime_names)]

use crate::future::BoxFuture;
use std::future::Future;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::cancel::DynCancelHandle;
use crate::capabilities::Capabilities;
use crate::error::Result;
use crate::query_stream::QueryStream;
use crate::schema::{QueryResult, Schema, SchemaCatalog, Table, TableSchema};
use crate::stream::DynRowStream;
use crate::value::Value;

/// Visual accent colour applied to the TUI border + status bar when a
/// connection is active. The intent is operational safety: prod = red,
/// staging = yellow, dev = green. Six named colours so terminal
/// compatibility is trivial — no hex / RGB to render-degrade.
///
/// Serialises as lowercase (`color = "red"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ConnectionColor {
    Red,
    Yellow,
    Green,
    Blue,
    Magenta,
    Cyan,
}

/// TLS/SSL mode for a database connection.
///
/// Mirrors the standard libpq `sslmode` parameter. Serialises as
/// kebab-case in TOML (`"verify-full"`, `"verify-ca"`, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum SslMode {
    Disable,
    #[default]
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

/// Static metadata describing how to reach a database.
///
/// The credential itself is not stored here; it is retrieved separately from
/// the configured credential store and passed to
/// [`crate::DatabaseDriver::connect`] at runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionConfig {
    pub id: uuid::Uuid,
    pub name: String,
    pub driver: String,
    pub params: ConnectionParams,
}

/// Driver-agnostic connection parameters.
///
/// Each driver decides which fields are required; unused fields remain
/// `None`. Engine-specific tuning is expressed through [`Self::options`].
///
/// Marked `#[non_exhaustive]` so adding new optional fields
/// (`color`, `confirm_writes`, `read_only`, future TLS knobs, …)
/// is a non-breaking change. Construct with `..Default::default()`
/// or via the public setter pattern.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ConnectionParams {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub username: Option<String>,
    pub path: Option<String>,
    /// Optional password material declared *in the configuration file*.
    ///
    /// v1.x stored passwords exclusively in the OS keyring (or fell
    /// back to `~/.pgpass` / env vars). v2.0 accepts an
    /// optional in-file value so users can express:
    ///
    /// * a literal password (discouraged, but supported for parity);
    /// * an `${env:VAR}` placeholder, expanded by
    /// `narwhal_config::interpolate` at load time — same vocabulary
    /// as every other string field;
    /// * a vault reference: `vault:hashicorp/<path>#<field>` or
    /// `1password:op://Vault/Item/field`, resolved at connect time
    /// by `narwhal_config::vault::VaultRegistry`.
    ///
    /// Resolution order at runtime is:
    ///
    /// 1. If `password` is present and parses as a vault reference →
    /// the configured provider returns the secret.
    /// 2. If `password` is present and is *not* a reference → it is
    /// used verbatim (after env interpolation).
    /// 3. Else, the keyring is consulted by `connection.id`.
    /// 4. Else, the `~/.pgpass` / env-var fallback runs.
    ///
    /// The reference is stored as a plain `String` because that is
    /// the on-disk shape; the resolved secret is held in
    /// `secrecy::SecretString` from the moment the resolver runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default)]
    pub options: std::collections::BTreeMap<String, String>,
    /// TLS/SSL mode. Defaults to [`SslMode::Prefer`] for network drivers
    /// and [`SslMode::Disable`] for file-local drivers (sqlite, duckdb).
    #[serde(default)]
    pub ssl_mode: SslMode,
    /// Path to the CA/root certificate bundle (PEM format).
    #[serde(default)]
    pub ssl_root_cert: Option<PathBuf>,
    /// Path to the client certificate (PEM format).
    #[serde(default)]
    pub ssl_cert: Option<PathBuf>,
    /// Path to the client private key (PEM format).
    #[serde(default)]
    pub ssl_key: Option<PathBuf>,
    /// Optional SSH tunnel. When `Some`, [`crate::ssh::SshTunnel::spawn`]
    /// brings up a local-port-forward before the driver connects and
    /// rewrites `host`/`port` to the loopback side of the tunnel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<SshConfig>,
    /// L36 #7: ordered list of shell commands executed before the
    /// connection is opened. Each step's stdout can be captured into
    /// a named variable and substituted into the remaining string
    /// fields of [`ConnectionParams`] via `${preconnect:NAME}`
    /// placeholders. The canonical use case is fetching a short-lived
    /// password from a secrets manager (`vault kv get …`) or a
    /// kubectl pod IP before the driver dials in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_connect: Vec<PreConnectStep>,
    /// optional accent colour for the TUI border + status
    /// bar while this connection is active. `None` keeps the theme
    /// default. Production users typically set `color = "red"` so
    /// "am I on prod?" is answered by a glance at the screen edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<ConnectionColor>,
    /// when `true`, mutating statements (`INSERT`, `UPDATE`,
    /// `DELETE`, DDL, …) prompt for a confirmation modal before they
    /// reach the driver. Bare reads run without confirmation.
    /// Recommended on every connection that touches production data.
    #[serde(default, skip_serializing_if = "is_false")]
    pub confirm_writes: bool,
    /// when `true`, the session is opened in driver-enforced
    /// read-only mode (`SET default_transaction_read_only TO ON` on
    /// PG, `PRAGMA query_only = ON` on `SQLite`, etc.) **and** the TUI
    /// applies the same syntactic guard MCP uses
    /// (`narwhal_sql::guard_read_only`) before each run. Either
    /// layer rejecting the statement aborts it without driver round
    /// trip.
    #[serde(default, skip_serializing_if = "is_false")]
    pub read_only: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(b: &bool) -> bool {
    !*b
}

impl ConnectionParams {
    /// Construct a [`ConnectionParams`] by mutating the default via
    /// `f`. The canonical way to build a `ConnectionParams` from
    /// outside the `narwhal-core` crate — the struct is marked
    /// `#[non_exhaustive]` so struct-literal construction (including
    /// functional update syntax `..Default::default()`) is forbidden.
    ///
    /// Minimal network connection:
    ///
    /// ```
    /// use narwhal_core::ConnectionParams;
    /// let p = ConnectionParams::with(|p| {
    /// p.host = Some("db.local".into());
    /// p.port = Some(5432);
    /// });
    /// assert_eq!(p.port, Some(5432));
    /// ```
    ///
    /// Production-tagged connection with the v1.1 safety knobs:
    ///
    /// ```
    /// use narwhal_core::{ConnectionColor, ConnectionParams};
    /// let p = ConnectionParams::with(|p| {
    /// p.host = Some("prod-db.example.com".into());
    /// p.port = Some(5432);
    /// p.database = Some("appdb".into());
    /// p.color = Some(ConnectionColor::Red);
    /// p.confirm_writes = true;
    /// p.read_only = true;
    /// });
    /// assert_eq!(p.color, Some(ConnectionColor::Red));
    /// assert!(p.read_only);
    /// ```
    #[must_use]
    pub fn with(f: impl FnOnce(&mut Self)) -> Self {
        let mut p = Self::default();
        f(&mut p);
        p
    }
}

/// One pre-connect command.
///
/// The `command` string is handed to `sh -c` so users can compose
/// pipes / redirections without us shipping a parser. Stdout is
/// captured (trimmed of trailing whitespace) and, when
/// `save_output_to` is set, stored under that key in the
/// pre-connect variable map.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct PreConnectStep {
    /// Shell command line. Run via `sh -c`.
    pub command: String,
    /// When set, the trimmed stdout of `command` is stored under
    /// this key in the variable map exposed to the rest of the
    /// connection params via `${preconnect:NAME}` placeholders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_output_to: Option<String>,
    /// Time budget for this step. Defaults to 30 seconds. The whole
    /// pre-connect sequence is capped at the sum of its steps'
    /// timeouts so a wedged kubectl call cannot freeze the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u32>,
    /// When `true`, a non-zero exit aborts the entire connection
    /// open. When `false`, the failure is logged and the sequence
    /// continues to the next step. Defaults to `true`.
    #[serde(default = "default_required")]
    pub required: bool,
}

const fn default_required() -> bool {
    true
}

impl PreConnectStep {
    /// Build a step from the bare command line. Convenience for
    /// tests and any future config-tooling that wants to assemble a
    /// step without going through serde.
    #[must_use]
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            save_output_to: None,
            timeout_secs: None,
            required: true,
        }
    }

    #[must_use]
    pub fn with_save_output_to(mut self, key: impl Into<String>) -> Self {
        self.save_output_to = Some(key.into());
        self
    }

    #[must_use]
    pub const fn with_timeout_secs(mut self, secs: u32) -> Self {
        self.timeout_secs = Some(secs);
        self
    }

    #[must_use]
    pub const fn with_required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }
}

/// SSH tunnel parameters. Only the host + user are required; everything
/// else falls back to the OpenSSH client defaults (`~/.ssh/config`,
/// the ssh agent, port 22) so a one-line `ssh_host=jump.example.com`
/// suffices for the common case.
///
/// Passwords are deliberately absent: production environments are
/// expected to authenticate via key files or the ssh-agent, both of
/// which the underlying `ssh` subprocess picks up for free.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct SshConfig {
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub user: String,
    /// Path to the private key. When `None`, the ssh subprocess
    /// consults `~/.ssh/config` and the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<PathBuf>,
    /// Optional jump host (`-J user@host`). Useful for bastion
    /// topologies where the actual database host is only reachable
    /// from inside the bastion's network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump_host: Option<String>,
}

impl SshConfig {
    /// Construct a minimal tunnel spec from the two required fields.
    /// Tests use this; production code goes through serde.
    pub fn new(host: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port: None,
            user: user.into(),
            key_path: None,
            jump_host: None,
        }
    }
}

/// Standard ANSI transaction isolation levels.
///
/// Drivers map this to the engine's native syntax; unsupported levels yield
/// [`crate::Error::Unsupported`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum IsolationLevel {
    ReadUncommitted,
    ReadCommitted,
    RepeatableRead,
    Serializable,
}

/// Open session against a database.
///
/// All methods that mutate session state take `&mut self` to make ownership
/// explicit and to surface accidental concurrent use at compile time.
///
/// # Trait shape
///
/// This trait uses **native `async fn` in trait** (RPITIT) — every
/// `async fn` desugars to `-> impl Future + Send`. Because RPITIT is
/// **not** dyn-compatible, callers that need a trait object should use
/// [`DynConnection`] instead: it boxes the returned future, costing an
/// allocation per call but enabling `Box<dyn DynConnection>` /
/// `Arc<dyn DynConnection>` sites. A blanket
/// `impl<T: Connection> DynConnection for T` is provided, so any type
/// that implements `Connection` automatically implements `DynConnection`.
///
/// Driver authors implement [`Connection`] directly with `async fn`
/// bodies — the compiler enforces that every returned future is `Send`.
pub trait Connection: Send + Sync {
    /// Execute a single statement and return the materialised result set.
    ///
    /// Parameters are bound positionally. Drivers that do not implement
    /// server-side prepared statements emulate binding by escaping.
    fn execute(
        &mut self,
        sql: &str,
        params: &[Value],
    ) -> impl Future<Output = Result<QueryResult>> + Send;

    /// Execute a single statement and return a row stream.
    ///
    /// Streams release server-side resources only when the returned
    /// [`crate::RowStream::close`] is called or the stream is dropped.
    fn stream(
        &mut self,
        sql: &str,
        params: &[Value],
    ) -> impl Future<Output = Result<Box<dyn DynRowStream>>> + Send;

    /// Execute a single statement and return a [`QueryStream`] —
    /// columns up-front, rows arriving asynchronously.
    ///
    /// The default implementation builds the stream from
    /// [`Self::stream`] and captures the column headers eagerly so
    /// the caller can announce the schema before the first row
    /// crosses the wire. Drivers that can produce the headers and
    /// open the cursor in a single round trip can override this for
    /// a latency win; the trait contract is
    ///
    /// 1. `columns()` returns the final column list before the first
    /// `next_row()` resolves.
    /// 2. Dropping the [`QueryStream`] releases driver-side cursor /
    /// portal state.
    /// 3. [`QueryStream::close`] is awaitable so drivers that must
    /// flush a server-side close (PG portals, `ClickHouse` HTTP
    /// bodies) can surface release errors.
    ///
    /// `QueryStream` is the canonical entry point for the TUI run
    /// worker, the MCP query tool's bounded drain, and the v2.0
    /// export path. Use [`Self::execute`] when you specifically need
    /// the materialised `QueryResult` with `rows_affected` reporting
    /// (DDL / DML).
    fn query(
        &mut self,
        sql: &str,
        params: &[Value],
    ) -> impl Future<Output = Result<QueryStream>> + Send {
        async move {
            let inner = self.stream(sql, params).await?;
            Ok(QueryStream::new(inner))
        }
    }

    /// Begin a transaction with the engine's default isolation level.
    fn begin(&mut self) -> impl Future<Output = Result<()>> + Send;

    /// Begin a transaction with the requested isolation level.
    fn begin_with(&mut self, isolation: IsolationLevel) -> impl Future<Output = Result<()>> + Send;

    /// Commit the current transaction.
    fn commit(&mut self) -> impl Future<Output = Result<()>> + Send;

    /// Roll back the current transaction.
    fn rollback(&mut self) -> impl Future<Output = Result<()>> + Send;

    /// Establish a savepoint inside the current transaction.
    ///
    /// The default implementation reports the feature as unsupported;
    /// drivers whose [`Capabilities::savepoints`] is `true` override it.
    fn savepoint(&mut self, name: &str) -> impl Future<Output = Result<()>> + Send {
        let _ = name;
        async { Err(crate::Error::unsupported("savepoints")) }
    }

    /// Release a previously created savepoint.
    fn release_savepoint(&mut self, name: &str) -> impl Future<Output = Result<()>> + Send {
        let _ = name;
        async { Err(crate::Error::unsupported("savepoints")) }
    }

    /// Roll back to a previously created savepoint without ending the
    /// surrounding transaction.
    fn rollback_to_savepoint(&mut self, name: &str) -> impl Future<Output = Result<()>> + Send {
        let _ = name;
        async { Err(crate::Error::unsupported("savepoints")) }
    }

    /// List logical schemas/namespaces visible to the session.
    fn list_schemas(&mut self) -> impl Future<Output = Result<Vec<Schema>>> + Send;

    /// List tables and views inside `schema`.
    fn list_tables(&mut self, schema: &str) -> impl Future<Output = Result<Vec<Table>>> + Send;

    /// List every table/view across every visible schema in a single
    /// round trip when the driver can express it cheaply.
    ///
    /// The default implementation falls back to
    /// [`list_schemas`](Connection::list_schemas) followed by one
    /// [`list_tables`](Connection::list_tables) per schema, which is
    /// the historical N+1 path. Drivers that expose a catalogue
    /// (`information_schema.tables`, `sqlite_master`, `system.tables`)
    /// override this to issue a single query.
    ///
    /// Returned schemas preserve the order produced by `list_schemas`;
    /// tables inside each schema preserve the order produced by
    /// `list_tables`.
    fn list_all_tables(&mut self) -> impl Future<Output = Result<SchemaCatalog>> + Send {
        async move {
            let schemas = self.list_schemas().await?;
            let mut out = Vec::with_capacity(schemas.len());
            for schema in schemas {
                let tables = self.list_tables(&schema.name).await?;
                out.push((schema, tables));
            }
            Ok(out)
        }
    }

    /// Describe the columns, defaults and constraints of `schema.name`.
    fn describe_table(
        &mut self,
        schema: &str,
        name: &str,
    ) -> impl Future<Output = Result<TableSchema>> + Send;

    /// Liveness probe.
    fn ping(&mut self) -> impl Future<Output = Result<()>> + Send;

    /// Return a cancellation handle that may be used to abort the next query
    /// dispatched on this connection. `None` means the driver does not
    /// support out-of-band cancellation.
    fn cancel_handle(&self) -> Option<Box<dyn DynCancelHandle>>;

    /// Static capability descriptor for this driver.
    fn capabilities(&self) -> Capabilities;

    /// Fetch the DDL (CREATE statement) for the given table.
    ///
    /// The default implementation returns [`crate::Error::Unsupported`];
    /// drivers override this to return engine-native DDL.
    fn fetch_ddl(
        &mut self,
        _schema: &str,
        _table: &str,
    ) -> impl Future<Output = Result<String>> + Send {
        async { Err(crate::Error::unsupported("fetch_ddl")) }
    }

    /// Toggle session-level read-only enforcement.
    ///
    /// When `true`, the driver instructs the database engine to refuse
    /// writes for the lifetime of the session (until this method is
    /// called again with `false`). Mapping per driver:
    ///
    /// - `PostgreSQL`: `SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY`
    /// + `SET default_transaction_read_only TO ON`.
    /// - `MySQL`/`MariaDB`: `SET SESSION TRANSACTION READ ONLY`.
    /// - `SQLite`: `PRAGMA query_only = ON`.
    /// - `ClickHouse`: `SET readonly = 2` (allow SELECT + SET).
    /// - `DuckDB`: opens are file-mode driven; per-session flip is
    /// reported as [`crate::Error::Unsupported`] so callers can fall
    /// back to the connection-string toggle.
    ///
    /// The default implementation reports the feature as unsupported so
    /// driver authors are forced to make an explicit choice (and so a
    /// security-sensitive caller can detect the absence of enforcement).
    fn set_read_only(&mut self, read_only: bool) -> impl Future<Output = Result<()>> + Send {
        let _ = read_only;
        async { Err(crate::Error::unsupported("set_read_only")) }
    }

    /// Tear down the underlying connection.
    fn close(self: Box<Self>) -> impl Future<Output = Result<()>> + Send;
}

/// Dyn-safe sibling of [`Connection`].
///
/// Native `async fn` in trait isn't dyn-compatible — the returned
/// future has an existential type that can't fit in a vtable slot.
/// `DynConnection` is the boxing wrapper: every async method returns
/// `Pin<Box<dyn Future + Send + '_>>`, which **is** vtable-friendly.
///
/// A blanket `impl<T: Connection> DynConnection for T` means any
/// `Connection` automatically satisfies `DynConnection`. Callers that
/// need a trait object — drivers handed out from a registry, pools
/// holding heterogeneous connections, the CLI dispatcher — use
/// `Box<dyn DynConnection>` / `Arc<dyn DynConnection>` and pay the
/// classic `Box<dyn Future>` alloc per call. Callers with a concrete
/// type call [`Connection`] directly and avoid the alloc.
pub trait DynConnection: Send + Sync {
    fn execute<'a>(
        &'a mut self,
        sql: &'a str,
        params: &'a [Value],
    ) -> BoxFuture<'a, Result<QueryResult>>;

    fn stream<'a>(
        &'a mut self,
        sql: &'a str,
        params: &'a [Value],
    ) -> BoxFuture<'a, Result<Box<dyn DynRowStream>>>;

    fn query<'a>(
        &'a mut self,
        sql: &'a str,
        params: &'a [Value],
    ) -> BoxFuture<'a, Result<QueryStream>>;

    fn begin<'a>(&'a mut self) -> BoxFuture<'a, Result<()>>;

    fn begin_with<'a>(&'a mut self, isolation: IsolationLevel) -> BoxFuture<'a, Result<()>>;

    fn commit<'a>(&'a mut self) -> BoxFuture<'a, Result<()>>;

    fn rollback<'a>(&'a mut self) -> BoxFuture<'a, Result<()>>;

    fn savepoint<'a>(&'a mut self, name: &'a str) -> BoxFuture<'a, Result<()>>;

    fn release_savepoint<'a>(&'a mut self, name: &'a str) -> BoxFuture<'a, Result<()>>;

    fn rollback_to_savepoint<'a>(&'a mut self, name: &'a str) -> BoxFuture<'a, Result<()>>;

    fn list_schemas<'a>(&'a mut self) -> BoxFuture<'a, Result<Vec<Schema>>>;

    fn list_tables<'a>(&'a mut self, schema: &'a str) -> BoxFuture<'a, Result<Vec<Table>>>;

    fn list_all_tables<'a>(&'a mut self) -> BoxFuture<'a, Result<SchemaCatalog>>;

    fn describe_table<'a>(
        &'a mut self,
        schema: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<TableSchema>>;

    fn ping<'a>(&'a mut self) -> BoxFuture<'a, Result<()>>;

    fn cancel_handle(&self) -> Option<Box<dyn DynCancelHandle>>;

    fn capabilities(&self) -> Capabilities;

    fn fetch_ddl<'a>(
        &'a mut self,
        schema: &'a str,
        table: &'a str,
    ) -> BoxFuture<'a, Result<String>>;

    fn set_read_only<'a>(&'a mut self, read_only: bool) -> BoxFuture<'a, Result<()>>;

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<()>>;
}

impl<T> DynConnection for T
where
    T: Connection + 'static,
{
    fn execute<'a>(
        &'a mut self,
        sql: &'a str,
        params: &'a [Value],
    ) -> BoxFuture<'a, Result<QueryResult>> {
        Box::pin(<Self as Connection>::execute(self, sql, params))
    }

    fn stream<'a>(
        &'a mut self,
        sql: &'a str,
        params: &'a [Value],
    ) -> BoxFuture<'a, Result<Box<dyn DynRowStream>>> {
        Box::pin(<Self as Connection>::stream(self, sql, params))
    }

    fn query<'a>(
        &'a mut self,
        sql: &'a str,
        params: &'a [Value],
    ) -> BoxFuture<'a, Result<QueryStream>> {
        Box::pin(<Self as Connection>::query(self, sql, params))
    }

    fn begin<'a>(&'a mut self) -> BoxFuture<'a, Result<()>> {
        Box::pin(<Self as Connection>::begin(self))
    }

    fn begin_with<'a>(&'a mut self, isolation: IsolationLevel) -> BoxFuture<'a, Result<()>> {
        Box::pin(<Self as Connection>::begin_with(self, isolation))
    }

    fn commit<'a>(&'a mut self) -> BoxFuture<'a, Result<()>> {
        Box::pin(<Self as Connection>::commit(self))
    }

    fn rollback<'a>(&'a mut self) -> BoxFuture<'a, Result<()>> {
        Box::pin(<Self as Connection>::rollback(self))
    }

    fn savepoint<'a>(&'a mut self, name: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(<Self as Connection>::savepoint(self, name))
    }

    fn release_savepoint<'a>(&'a mut self, name: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(<Self as Connection>::release_savepoint(self, name))
    }

    fn rollback_to_savepoint<'a>(&'a mut self, name: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(<Self as Connection>::rollback_to_savepoint(self, name))
    }

    fn list_schemas<'a>(&'a mut self) -> BoxFuture<'a, Result<Vec<Schema>>> {
        Box::pin(<Self as Connection>::list_schemas(self))
    }

    fn list_tables<'a>(&'a mut self, schema: &'a str) -> BoxFuture<'a, Result<Vec<Table>>> {
        Box::pin(<Self as Connection>::list_tables(self, schema))
    }

    fn list_all_tables<'a>(&'a mut self) -> BoxFuture<'a, Result<SchemaCatalog>> {
        Box::pin(<Self as Connection>::list_all_tables(self))
    }

    fn describe_table<'a>(
        &'a mut self,
        schema: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<TableSchema>> {
        Box::pin(<Self as Connection>::describe_table(self, schema, name))
    }

    fn ping<'a>(&'a mut self) -> BoxFuture<'a, Result<()>> {
        Box::pin(<Self as Connection>::ping(self))
    }

    fn cancel_handle(&self) -> Option<Box<dyn DynCancelHandle>> {
        <Self as Connection>::cancel_handle(self)
    }

    fn capabilities(&self) -> Capabilities {
        <Self as Connection>::capabilities(self)
    }

    fn fetch_ddl<'a>(
        &'a mut self,
        schema: &'a str,
        table: &'a str,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(<Self as Connection>::fetch_ddl(self, schema, table))
    }

    fn set_read_only<'a>(&'a mut self, read_only: bool) -> BoxFuture<'a, Result<()>> {
        Box::pin(<Self as Connection>::set_read_only(self, read_only))
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<()>> {
        Box::pin(<Self as Connection>::close(self))
    }
}
