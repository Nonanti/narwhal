use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};

use narwhal_core::{Connection, ConnectionConfig, DatabaseDriver};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{debug, warn};

#[derive(Debug, Error)]
pub enum PoolError {
    #[error("pool is closed")]
    Closed,
    #[error("connection error: {0}")]
    Connection(#[from] narwhal_core::Error),
}

/// Tunables for a [`Pool`].
#[derive(Debug, Clone, Copy)]
pub struct PoolConfig {
    /// Maximum number of live connections the pool will manage.
    pub max_size: usize,
    /// When true, every connection is pinged before being handed to the
    /// caller. Adds one round-trip per [`Pool::acquire`].
    pub test_on_acquire: bool,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_size: 8,
            test_on_acquire: true,
        }
    }
}

/// Pool of [`Connection`] instances backed by a single driver and
/// configuration. Cloning a [`Pool`] shares the same underlying state.
#[derive(Clone)]
pub struct Pool {
    inner: Arc<Inner>,
}

struct Inner {
    driver: Arc<dyn DatabaseDriver>,
    config: ConnectionConfig,
    password: Option<String>,
    settings: PoolConfig,
    idle: Mutex<Vec<Box<dyn Connection>>>,
    semaphore: Arc<Semaphore>,
}

impl Pool {
    pub fn new(
        driver: Arc<dyn DatabaseDriver>,
        config: ConnectionConfig,
        password: Option<String>,
        settings: PoolConfig,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                driver,
                config,
                password,
                settings,
                idle: Mutex::new(Vec::new()),
                semaphore: Arc::new(Semaphore::new(settings.max_size)),
            }),
        }
    }

    pub fn config(&self) -> &ConnectionConfig {
        &self.inner.config
    }

    pub fn settings(&self) -> PoolConfig {
        self.inner.settings
    }

    /// Number of currently idle, ready-to-hand-out connections.
    pub fn idle_count(&self) -> usize {
        self.inner.idle.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// Acquire a connection from the pool. The returned guard returns the
    /// connection to the pool when dropped.
    pub async fn acquire(&self) -> Result<PooledConnection, PoolError> {
        let permit = Arc::clone(&self.inner.semaphore)
            .acquire_owned()
            .await
            .map_err(|_| PoolError::Closed)?;

        let reused = {
            let mut idle = self.inner.idle.lock().expect("idle lock poisoned");
            idle.pop()
        };

        let mut connection = if let Some(conn) = reused {
            conn
        } else {
            debug!(target: "narwhal::pool", "creating new connection");
            self.inner
                .driver
                .connect(&self.inner.config, self.inner.password.as_deref())
                .await?
        };

        if self.inner.settings.test_on_acquire {
            if let Err(error) = connection.ping().await {
                warn!(
                    target: "narwhal::pool",
                    error = %error,
                    "discarding unhealthy connection"
                );
                // Drop the bad connection and create a fresh one.
                connection = self
                    .inner
                    .driver
                    .connect(&self.inner.config, self.inner.password.as_deref())
                    .await?;
            }
        }

        Ok(PooledConnection {
            connection: Some(connection),
            inner: Arc::clone(&self.inner),
            _permit: permit,
        })
    }

    /// Close every idle connection. Connections currently checked out are
    /// not affected; they will be released as their guards drop. The pool
    /// remains usable.
    pub async fn drain(&self) {
        let drained: Vec<Box<dyn Connection>> = {
            let mut idle = self.inner.idle.lock().expect("idle lock poisoned");
            std::mem::take(&mut *idle)
        };
        for conn in drained {
            if let Err(error) = conn.close().await {
                warn!(
                    target: "narwhal::pool",
                    error = %error,
                    "error while closing pooled connection"
                );
            }
        }
    }
}

/// RAII guard that returns its [`Connection`] to the pool on drop.
pub struct PooledConnection {
    connection: Option<Box<dyn Connection>>,
    inner: Arc<Inner>,
    _permit: OwnedSemaphorePermit,
}

impl Deref for PooledConnection {
    type Target = dyn Connection;

    fn deref(&self) -> &Self::Target {
        self.connection
            .as_deref()
            .expect("connection must be present until drop")
    }
}

impl DerefMut for PooledConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.connection
            .as_deref_mut()
            .expect("connection must be present until drop")
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        let Some(connection) = self.connection.take() else {
            return;
        };
        if let Ok(mut idle) = self.inner.idle.lock() {
            idle.push(connection);
        } else {
            warn!(
                target: "narwhal::pool",
                "idle lock poisoned; dropping connection",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use narwhal_core::{ConnectionParams, Value};
    use narwhal_driver_sqlite::SqliteDriver;
    use uuid::Uuid;

    fn config(path: &str) -> ConnectionConfig {
        ConnectionConfig {
            id: Uuid::nil(),
            name: "test".into(),
            driver: SqliteDriver::NAME.into(),
            params: ConnectionParams {
                path: Some(path.into()),
                ..Default::default()
            },
        }
    }

    fn pool() -> Pool {
        let driver: Arc<dyn DatabaseDriver> = Arc::new(SqliteDriver::new());
        Pool::new(
            driver,
            config(":memory:"),
            None,
            PoolConfig {
                max_size: 2,
                test_on_acquire: true,
            },
        )
    }

    #[tokio::test]
    async fn acquire_returns_to_pool_on_drop() {
        let pool = pool();
        assert_eq!(pool.idle_count(), 0);
        {
            let mut conn = pool.acquire().await.unwrap();
            let result = conn.execute("SELECT 1", &[]).await.unwrap();
            assert_eq!(result.rows.len(), 1);
        }
        assert_eq!(pool.idle_count(), 1);

        let mut conn = pool.acquire().await.unwrap();
        let _ = conn.execute("SELECT 1", &[]).await.unwrap();
        // The idle count drops to zero while the guard is alive.
        assert_eq!(pool.idle_count(), 0);
    }

    #[tokio::test]
    async fn capacity_is_bounded() {
        let pool = pool();
        let a = pool.acquire().await.unwrap();
        let b = pool.acquire().await.unwrap();

        let pool_clone = pool.clone();
        let waiter = tokio::spawn(async move { pool_clone.acquire().await.map(|_| ()) });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "third acquire must block");

        drop(a);
        waiter.await.unwrap().unwrap();
        drop(b);
    }

    #[tokio::test]
    async fn drain_empties_idle_connections() {
        let pool = pool();
        for _ in 0..3 {
            let mut conn = pool.acquire().await.unwrap();
            let _ = conn.execute("SELECT 1", &[]).await.unwrap();
        }
        assert!(pool.idle_count() > 0);
        pool.drain().await;
        assert_eq!(pool.idle_count(), 0);
        // Pool stays usable after a drain.
        let mut conn = pool.acquire().await.unwrap();
        let row = conn.execute("SELECT ?1", &[Value::Int(42)]).await.unwrap();
        assert_eq!(row.rows[0].get(0).map(Value::render), Some("42".into()));
    }
}
