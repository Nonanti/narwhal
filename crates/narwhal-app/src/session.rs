use std::collections::HashMap;
use std::sync::Arc;

use narwhal_core::{
    ColumnHeader, ConnectionConfig, DatabaseDriver, Error, IsolationLevel, Result, Schema, Table,
};
use narwhal_pool::{Pool, PoolConfig, PooledConnection};
use narwhal_sql::Dialect;
use narwhal_tui::SchemaListing;
use tokio::sync::Mutex;

/// Pinned connection plus auxiliary transaction state. Created by
/// [`Session::begin`] and consumed by [`Session::end_transaction`].
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
}

impl Session {
    pub async fn open(
        driver: Arc<dyn DatabaseDriver>,
        config: ConnectionConfig,
        password: Option<String>,
    ) -> Result<Self> {
        // Verify reachability eagerly so the user gets immediate feedback.
        let probe = driver.connect(&config, password.as_deref()).await?;
        drop(probe);

        let pool = Pool::new(
            Arc::clone(&driver),
            config.clone(),
            password,
            PoolConfig::default(),
        );

        Ok(Self {
            config,
            driver,
            pool,
            schemas: Vec::new(),
            transaction: None,
            column_cache: HashMap::new(),
        })
    }

    /// True while a transaction is open.
    pub fn in_transaction(&self) -> bool {
        self.transaction.is_some()
    }

    /// Refresh the cached schema listing.
    pub async fn refresh_schemas(&mut self) -> Result<()> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        let schemas = conn.list_schemas().await?;
        let mut listing = Vec::with_capacity(schemas.len());
        for schema in schemas {
            let tables: Vec<Table> = conn.list_tables(&schema.name).await.unwrap_or_default();
            listing.push((schema, tables));
        }
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
