use std::collections::HashMap;
use std::sync::Arc;

use narwhal_core::{
    Capabilities, ColumnHeader, ConnectionConfig, DatabaseDriver, Error, IsolationLevel, Result,
    Schema, SshTunnel,
};
use narwhal_domain::SchemaListing;
use narwhal_pool::{Pool, PoolConfig, PooledConnection};
use narwhal_sql::Dialect;
use tokio::sync::Mutex;

/// Pinned connection plus auxiliary transaction state. Created by the
/// `begin_transaction` host method and consumed by `end_transaction`.
pub struct TxnHandle {
    /// Connection checked out of the pool for the duration of the
    /// transaction. Wrapped in a tokio mutex so the run worker and command
    /// dispatcher can share it.
    pub conn: Arc<Mutex<PooledConnection>>,
    /// Active savepoint names, outermost first.
    pub savepoints: Vec<String>,
    pub isolation: Option<IsolationLevel>,
}

/// Open connection plus its driver capabilities and cached metadata.
///
/// Implements a non-trivial [`std::fmt::Debug`] so the meta channel
/// (`MetaUpdate::SessionOpened`) can derive `Debug` without dragging
/// driver / pool internals into the format output.
pub struct Session {
    pub config: ConnectionConfig,
    pub driver: Arc<dyn DatabaseDriver>,
    /// Snapshot of the driver's [`Capabilities`] taken at session
    /// open. Cached here so the host doesn't have to acquire a pool
    /// connection on every capability check (notably the L36 row-CRUD
    /// gate, which runs on every keystroke).
    pub capabilities: Capabilities,
    pub pool: Pool,
    pub schemas: Vec<SchemaListing>,
    pub transaction: Option<TxnHandle>,
    /// Lazily-populated column cache. Keys are lowercased table names;
    /// values are `(schema_name, columns)` tuples. Populated when
    /// `describe_table` is called (e.g. from sidebar preview).
    pub column_cache: HashMap<String, (String, Vec<ColumnHeader>)>,
    /// Live SSH tunnel for the duration of this session. `None` when
    /// the connection talks to the database directly. Dropped together
    /// with the session so the forwarded port goes away as soon as
    /// the user runs `:close`.
    pub _ssh_tunnel: Option<Arc<SshTunnel>>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("config.name", &self.config.name)
            .field("config.driver", &self.config.driver)
            .field("schema_count", &self.schemas.len())
            .field("has_transaction", &self.transaction.is_some())
            .finish_non_exhaustive()
    }
}

/// Options that modulate [`Session::open`] without bloating its
/// positional signature.
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionOpenOptions {
    /// When `true`, [`crate::pre_connect`] shell steps are **skipped**
    /// entirely. The CLI flips this on under `--read-only` so an
    /// auditor who thought they were only reading a database can't be
    /// tricked into running an arbitrary `kubectl delete pod …` or
    /// `rm -rf` step that someone left in their connections file.
    /// Any `${preconnect:NAME}` placeholder in the params then fails
    /// substitution (no var saved → `MissingVar`), surfacing the
    /// situation immediately instead of silently dropping the step.
    pub skip_pre_connect: bool,
}

impl Session {
    pub async fn open(
        driver: Arc<dyn DatabaseDriver>,
        config: ConnectionConfig,
        password: Option<String>,
    ) -> Result<Self> {
        Self::open_with(driver, config, password, SessionOpenOptions::default()).await
    }

    /// Variant of [`Self::open`] that takes a [`SessionOpenOptions`].
    /// Existing callers stay on the three-arg shortcut; the TUI's
    /// read-only path threads its CLI flag through this entry point.
    pub async fn open_with(
        driver: Arc<dyn DatabaseDriver>,
        config: ConnectionConfig,
        password: Option<String>,
        opts: SessionOpenOptions,
    ) -> Result<Self> {
        // L36 #7: pre-connect step pipeline. Runs *before* the SSH
        // tunnel because users typically need to fetch credentials
        // (vault) or look up a target host (kubectl) before either
        // the tunnel or the driver can dial in. Each step's stdout
        // is captured and the resulting `${preconnect:NAME}`
        // substitutions are applied to the connection params in
        // place — the SSH tunnel + driver see the fully-resolved
        // string fields.
        //
        // L36 #C4: when `opts.skip_pre_connect` is set the whole
        // pipeline is skipped — see [`SessionOpenOptions`] for why.
        let mut config = config;
        let mut password = password;
        if opts.skip_pre_connect {
            if !config.params.pre_connect.is_empty() {
                tracing::warn!(
                    target: "narwhal::session",
                    name = %config.name,
                    steps = config.params.pre_connect.len(),
                    "skipping pre-connect steps because session was opened in read-only mode"
                );
            }
        } else {
            let pc_vars = crate::pre_connect::run_pre_connect(&config.params.pre_connect)
                .await
                .map_err(|e| Error::Connection(format!("pre-connect: {e}")))?;
            if !pc_vars.is_empty() {
                crate::pre_connect::substitute_pre_connect(&mut config.params, &pc_vars)
                    .map_err(|e| Error::Connection(format!("pre-connect substitution: {e}")))?;
                // L36 #C3: expand `${preconnect:NAME}` in the password
                // channel too — this is the headline use case (vault
                // step writes the password, keyring stores the
                // placeholder).
                password = crate::pre_connect::substitute_password(password, &pc_vars)
                    .map_err(|e| Error::Connection(format!("pre-connect password: {e}")))?;
            }
        }
        // Bring up the SSH tunnel (if any) before the driver touches
        // the network. The returned `effective_config` carries the
        // loopback host/port the driver should target; the tunnel
        // handle is parked in the session so its Drop tears the
        // forward down when the user runs `:close`.
        let (effective_config, tunnel) = maybe_open_tunnel(config.clone())?;

        // Verify reachability eagerly so the user gets immediate feedback.
        // Use the trait's async `close` instead of letting the box drop
        // synchronously — some drivers (mysql, clickhouse) maintain
        // server-side state that only releases on a clean COM_QUIT, and
        // implicit drop leaves the server waiting for the idle timeout.
        let probe = driver
            .connect(&effective_config, password.as_deref())
            .await?;
        let capabilities = probe.capabilities();
        if let Err(error) = probe.close().await {
            tracing::debug!(
                target: "narwhal::session",
                error = %error,
                "probe close failed; the pool will still proceed"
            );
        }

        let pool = Pool::new(
            Arc::clone(&driver),
            effective_config,
            password,
            PoolConfig::default(),
        );

        Ok(Self {
            // Keep the original config around so the status bar / sidebar
            // still show the user-facing host instead of `127.0.0.1`.
            config,
            driver,
            capabilities,
            pool,
            schemas: Vec::new(),
            transaction: None,
            column_cache: HashMap::new(),
            _ssh_tunnel: tunnel,
        })
    }

    /// True while a transaction is open.
    pub const fn in_transaction(&self) -> bool {
        self.transaction.is_some()
    }

    /// Refresh the cached schema listing.
    ///
    /// Uses [`narwhal_core::Connection::list_all_tables`] which issues a single
    /// catalogue query when the driver supports it (e.g. PG, `MySQL`,
    /// `ClickHouse`) and falls back to the N+1 `list_schemas` +
    /// `list_tables` loop otherwise (H12).
    pub async fn refresh_schemas(&mut self) -> Result<()> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        let mut listing = conn.list_all_tables().await?;
        // If no schemas (e.g. SQLite returns "main" synthetic), still try to
        // list tables under the empty-string schema.
        if listing.is_empty() {
            if let Ok(tables) = conn.list_tables("").await {
                listing.push((
                    Schema {
                        name: String::new(),
                    },
                    tables,
                ));
            }
        }
        drop(conn);
        self.schemas = listing;
        Ok(())
    }

    pub fn dialect(&self) -> Dialect {
        match self.driver.name() {
            "postgres" => Dialect::Postgres,
            "sqlite" => Dialect::Sqlite,
            "mysql" => Dialect::MySql,
            _ => Dialect::Generic,
        }
    }
}

/// If `config.params.ssh` is set, bring up the tunnel and rewrite the
/// effective host/port to the loopback side. Returns the (possibly
/// rewritten) config plus the tunnel handle that must outlive every
/// connection opened against it.
fn maybe_open_tunnel(
    mut config: ConnectionConfig,
) -> Result<(ConnectionConfig, Option<Arc<SshTunnel>>)> {
    let Some(ssh) = config.params.ssh.clone() else {
        return Ok((config, None));
    };
    let target_host = config
        .params
        .host
        .clone()
        .ok_or_else(|| Error::Connection("ssh tunnel requested but host is empty".into()))?;
    let target_port = config
        .params
        .port
        .ok_or_else(|| Error::Connection("ssh tunnel requested but port is empty".into()))?;
    let tunnel = SshTunnel::spawn(&ssh, &target_host, target_port)
        .map_err(|e| Error::Connection(format!("ssh tunnel: {e}")))?;
    config.params.host = Some(tunnel.local_host().to_owned());
    config.params.port = Some(tunnel.local_port());
    // Strip the ssh marker so downstream copies of the config don't
    // try to bring up a *second* tunnel against the loopback target.
    config.params.ssh = None;
    Ok((config, Some(Arc::new(tunnel))))
}
