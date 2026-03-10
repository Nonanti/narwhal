use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::cancel::CancelHandle;
use crate::capabilities::Capabilities;
use crate::error::Result;
use crate::schema::{QueryResult, Schema, Table, TableSchema};
use crate::stream::RowStream;
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
    /// v1.1 #2: optional accent colour for the TUI border + status
    /// bar while this connection is active. `None` keeps the theme
    /// default. Production users typically set `color = "red"` so
    /// "am I on prod?" is answered by a glance at the screen edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<ConnectionColor>,
    /// v1.1 #2: when `true`, mutating statements (`INSERT`, `UPDATE`,
    /// `DELETE`, DDL, …) prompt for a confirmation modal before they
    /// reach the driver. Bare reads run without confirmation.
    /// Recommended on every connection that touches production data.
    #[serde(default, skip_serializing_if = "is_false")]
    pub confirm_writes: bool,
    /// v1.1 #2: when `true`, the session is opened in driver-enforced
    /// read-only mode (`SET default_transaction_read_only TO ON` on
    /// PG, `PRAGMA query_only = ON` on `SQLite`, etc.) **and** the TUI
    /// applies the same syntactic guard MCP uses
    /// ([`narwhal_sql::guard_read_only`]) before each run. Either
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
    ///     p.host = Some("db.local".into());
    ///     p.port = Some(5432);
    /// });
    /// assert_eq!(p.port, Some(5432));
    /// ```
    ///
    /// Production-tagged connection with the v1.1 safety knobs:
    ///
    /// ```
    /// use narwhal_core::{ConnectionColor, ConnectionParams};
    /// let p = ConnectionParams::with(|p| {
    ///     p.host = Some("prod-db.example.com".into());
    ///     p.port = Some(5432);
    ///     p.database = Some("appdb".into());
    ///     p.color = Some(ConnectionColor::Red);
    ///     p.confirm_writes = true;
    ///     p.read_only = true;
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
#[async_trait]
pub trait Connection: Send + Sync {
    /// Execute a single statement and return the materialised result set.
    ///
    /// Parameters are bound positionally. Drivers that do not implement
    /// server-side prepared statements emulate binding by escaping.
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult>;

    /// Execute a single statement and return a row stream.
    ///
    /// Streams release server-side resources only when the returned
    /// [`RowStream::close`] is called or the stream is dropped.
    async fn stream(&mut self, sql: &str, params: &[Value]) -> Result<Box<dyn RowStream>>;

    /// Begin a transaction with the engine's default isolation level.
    async fn begin(&mut self) -> Result<()>;

    /// Begin a transaction with the requested isolation level.
    async fn begin_with(&mut self, isolation: IsolationLevel) -> Result<()>;

    /// Commit the current transaction.
    async fn commit(&mut self) -> Result<()>;

    /// Roll back the current transaction.
    async fn rollback(&mut self) -> Result<()>;

    /// Establish a savepoint inside the current transaction.
    ///
    /// The default implementation reports the feature as unsupported;
    /// drivers whose [`Capabilities::savepoints`] is `true` override it.
    async fn savepoint(&mut self, name: &str) -> Result<()> {
        let _ = name;
        Err(crate::Error::unsupported("savepoints"))
    }

    /// Release a previously created savepoint.
    async fn release_savepoint(&mut self, name: &str) -> Result<()> {
        let _ = name;
        Err(crate::Error::unsupported("savepoints"))
    }

    /// Roll back to a previously created savepoint without ending the
    /// surrounding transaction.
    async fn rollback_to_savepoint(&mut self, name: &str) -> Result<()> {
        let _ = name;
        Err(crate::Error::unsupported("savepoints"))
    }

    /// List logical schemas/namespaces visible to the session.
    async fn list_schemas(&mut self) -> Result<Vec<Schema>>;

    /// List tables and views inside `schema`.
    async fn list_tables(&mut self, schema: &str) -> Result<Vec<Table>>;

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
    async fn list_all_tables(&mut self) -> Result<Vec<(Schema, Vec<Table>)>> {
        let schemas = self.list_schemas().await?;
        let mut out = Vec::with_capacity(schemas.len());
        for schema in schemas {
            let tables = self.list_tables(&schema.name).await?;
            out.push((schema, tables));
        }
        Ok(out)
    }

    /// Describe the columns, defaults and constraints of `schema.name`.
    async fn describe_table(&mut self, schema: &str, name: &str) -> Result<TableSchema>;

    /// Liveness probe.
    async fn ping(&mut self) -> Result<()>;

    /// Return a cancellation handle that may be used to abort the next query
    /// dispatched on this connection. `None` means the driver does not
    /// support out-of-band cancellation.
    fn cancel_handle(&self) -> Option<Box<dyn CancelHandle>>;

    /// Static capability descriptor for this driver.
    fn capabilities(&self) -> Capabilities;

    /// Fetch the DDL (CREATE statement) for the given table.
    ///
    /// The default implementation returns [`crate::Error::Unsupported`];
    /// drivers override this to return engine-native DDL.
    async fn fetch_ddl(&mut self, _schema: &str, _table: &str) -> Result<String> {
        Err(crate::Error::unsupported("fetch_ddl"))
    }

    /// Toggle session-level read-only enforcement.
    ///
    /// When `true`, the driver instructs the database engine to refuse
    /// writes for the lifetime of the session (until this method is
    /// called again with `false`). Mapping per driver:
    ///
    /// - `PostgreSQL`: `SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY`
    ///   + `SET default_transaction_read_only TO ON`.
    /// - `MySQL`/`MariaDB`: `SET SESSION TRANSACTION READ ONLY`.
    /// - `SQLite`: `PRAGMA query_only = ON`.
    /// - `ClickHouse`: `SET readonly = 2` (allow SELECT + SET).
    /// - `DuckDB`: opens are file-mode driven; per-session flip is
    ///   reported as [`crate::Error::Unsupported`] so callers can fall
    ///   back to the connection-string toggle.
    ///
    /// The default implementation reports the feature as unsupported so
    /// driver authors are forced to make an explicit choice (and so a
    /// security-sensitive caller can detect the absence of enforcement).
    async fn set_read_only(&mut self, read_only: bool) -> Result<()> {
        let _ = read_only;
        Err(crate::Error::unsupported("set_read_only"))
    }

    /// Tear down the underlying connection.
    async fn close(self: Box<Self>) -> Result<()>;
}
