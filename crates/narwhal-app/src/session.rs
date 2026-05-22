use std::collections::HashMap;
use std::sync::Arc;

use narwhal_core::{
    ColumnHeader, ConnectionConfig, DatabaseDriver, Error, IsolationLevel, Result, Schema,
    SshTunnel,
};
use narwhal_pool::{Pool, PoolConfig, PooledConnection};
use narwhal_sql::Dialect;
use narwhal_tui::SchemaListing;
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
pub struct Session {
    pub config: ConnectionConfig,
    pub driver: Arc<dyn DatabaseDriver>,
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

impl Session {
    pub async fn open(
        driver: Arc<dyn DatabaseDriver>,
        config: ConnectionConfig,
        password: Option<String>,
    ) -> Result<Self> {
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
            pool,
            schemas: Vec::new(),
            transaction: None,
            column_cache: HashMap::new(),
            _ssh_tunnel: tunnel,
        })
    }

    /// True while a transaction is open.
    pub fn in_transaction(&self) -> bool {
        self.transaction.is_some()
    }

    /// Refresh the cached schema listing.
    ///
    /// Uses [`narwhal_core::Connection::list_all_tables`] which issues a single
    /// catalogue query when the driver supports it (e.g. PG, MySQL,
    /// ClickHouse) and falls back to the N+1 `list_schemas` +
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
