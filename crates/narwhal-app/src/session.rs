use std::sync::Arc;

use narwhal_core::{ConnectionConfig, DatabaseDriver, Error, Result, Schema, Table};
use narwhal_pool::{Pool, PoolConfig};
use narwhal_sql::Dialect;
use narwhal_tui::SchemaListing;

/// Open connection plus its driver capabilities and cached metadata.
pub struct Session {
    pub config: ConnectionConfig,
    pub driver: Arc<dyn DatabaseDriver>,
    pub pool: Pool,
    pub schemas: Vec<SchemaListing>,
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
        })
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
