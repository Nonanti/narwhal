use std::num::NonZeroUsize;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::time::{Duration, Instant};

use narwhal_core::{Connection, ConnectionConfig, DatabaseDriver};
use parking_lot::Mutex;
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{debug, warn};

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PoolError {
    #[error("pool is closed")]
    Closed,
    #[error("connection error: {0}")]
    Connection(#[from] narwhal_core::Error),
    #[error("timed out after {0:?} waiting for a new connection")]
    ConnectTimeout(Duration),
}

/// Tunables for a [`Pool`].
///
/// L6: zero-capacity pools cannot exist — `max_size` is `NonZeroUsize`
/// so a configuration error is rejected at the type level instead of
/// panicking in [`Pool::new`].
#[derive(Debug, Clone, Copy)]
pub struct PoolConfig {
    /// Maximum number of live connections the pool will manage.
    pub max_size: NonZeroUsize,
    /// When true, every connection is pinged before being handed to the
    /// caller. Adds one round-trip per [`Pool::acquire`].
    pub test_on_acquire: bool,
    /// Discard idle connections that have been sitting in the pool for
    /// longer than this duration. `None` disables the check (legacy
    /// behaviour).
    ///
    /// Cloud load balancers and edge proxies routinely sever idle TCP
    /// sockets after a few minutes; bouncing the connection lazily on
    /// acquire is cheaper than waiting for a hard close to surface as a
    /// query error.
    pub idle_timeout: Option<Duration>,
    /// Maximum wall-clock age of a pooled connection (counted from when
    /// the connection was first opened, regardless of how often it was
    /// reused). `None` disables the check.
    ///
    /// Protects against gradual server-side state accumulation
    /// (prepared statement caches, temp tables, server-side cursors)
    /// and against long-lived rotated credentials remaining in flight.
    pub max_lifetime: Option<Duration>,
    /// Maximum time [`Pool::acquire`] will wait for the underlying
    /// driver's `connect()` call to complete. `None` disables the
    /// timeout. The wait for a [`tokio::sync::Semaphore`] permit is
    /// *not* covered — back-pressure on a full pool is intentional and
    /// distinct from a hung TCP handshake.
    pub connect_timeout: Option<Duration>,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            // SAFETY: 8 is a non-zero literal.
            max_size: NonZeroUsize::new(8).expect("8 is non-zero"),
            test_on_acquire: true,
            idle_timeout: Some(Duration::from_secs(300)),
            max_lifetime: Some(Duration::from_secs(1800)),
            connect_timeout: Some(Duration::from_secs(10)),
        }
    }
}

/// Pool of [`Connection`] instances backed by a single driver and
/// configuration. Cloning a [`Pool`] shares the same underlying state.
#[derive(Clone)]
pub struct Pool {
    inner: Arc<Inner>,
}

/// One pooled connection plus the timestamps used to enforce
/// `idle_timeout` and `max_lifetime`. Kept private; callers see only
/// the wrapped `Box<dyn Connection>` through [`PooledConnection`].
struct Entry {
    connection: Box<dyn Connection>,
    /// When the connection was returned to the pool (or initially
    /// created, in which case it equals `created_at`).
    idle_since: Instant,
    /// When the connection was first opened by the driver.
    created_at: Instant,
}

struct Inner {
    driver: Arc<dyn DatabaseDriver>,
    config: ConnectionConfig,
    password: Option<String>,
    settings: PoolConfig,
    /// Idle connections. `parking_lot::Mutex` is used instead of
    /// `std::sync::Mutex` because it is unpoisonable — a panic inside a
    /// lock holder does not prevent subsequent lock acquisition. This
    /// eliminates the need for `.expect("poisoned")` on every lock call.
    idle: Mutex<Vec<Entry>>,
    semaphore: Arc<Semaphore>,
}

impl Pool {
    /// Construct a new [`Pool`].
    pub fn new(
        driver: Arc<dyn DatabaseDriver>,
        config: ConnectionConfig,
        password: Option<String>,
        settings: PoolConfig,
    ) -> Self {
        let cap = settings.max_size.get();
        Self {
            inner: Arc::new(Inner {
                driver,
                config,
                password,
                settings,
                idle: Mutex::new(Vec::new()),
                semaphore: Arc::new(Semaphore::new(cap)),
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
        self.inner.idle.lock().len()
    }

    /// Acquire a connection from the pool. The returned guard returns the
    /// connection to the pool when dropped.
    pub async fn acquire(&self) -> Result<PooledConnection, PoolError> {
        let permit = Arc::clone(&self.inner.semaphore)
            .acquire_owned()
            .await
            .map_err(|_| PoolError::Closed)?;

        // Loop so that a stale (timed-out) idle connection can be
        // discarded and replaced without forcing the caller to retry.
        loop {
            let reused = self.pop_fresh_idle();

            let mut entry = if let Some(e) = reused {
                e
            } else {
                debug!(target: "narwhal::pool", "creating new connection");
                let conn = self.connect_with_timeout().await?;
                Entry {
                    connection: conn,
                    idle_since: Instant::now(),
                    created_at: Instant::now(),
                }
            };

            if self.inner.settings.test_on_acquire {
                if let Err(error) = entry.connection.ping().await {
                    warn!(
                        target: "narwhal::pool",
                        error = %error,
                        "discarding unhealthy connection"
                    );
                    // Bad connection goes to async close, then loop and
                    // try to either pop another idle or create new.
                    spawn_close(entry.connection);
                    continue;
                }
            }

            return Ok(PooledConnection {
                connection: Some(entry.connection),
                created_at: entry.created_at,
                inner: Arc::clone(&self.inner),
                invalidated: false,
                _permit: permit,
            });
        }
    }

    /// Pop the most-recently-returned idle entry, discarding any whose
    /// `idle_timeout` or `max_lifetime` has expired. Expired entries
    /// are closed in background tasks so the caller is not charged for
    /// the round-trip.
    fn pop_fresh_idle(&self) -> Option<Entry> {
        let now = Instant::now();
        let settings = self.inner.settings;
        let mut idle = self.inner.idle.lock();
        while let Some(entry) = idle.pop() {
            let idle_expired = settings
                .idle_timeout
                .is_some_and(|t| now.saturating_duration_since(entry.idle_since) > t);
            let life_expired = settings
                .max_lifetime
                .is_some_and(|t| now.saturating_duration_since(entry.created_at) > t);
            if idle_expired || life_expired {
                debug!(
                    target: "narwhal::pool",
                    idle_expired,
                    life_expired,
                    "discarding stale idle connection"
                );
                spawn_close(entry.connection);
                continue;
            }
            return Some(entry);
        }
        None
    }

    async fn connect_with_timeout(&self) -> Result<Box<dyn Connection>, PoolError> {
        let fut = self
            .inner
            .driver
            .connect(&self.inner.config, self.inner.password.as_deref());
        let Some(timeout) = self.inner.settings.connect_timeout else {
            return fut.await.map_err(PoolError::from);
        };
        match tokio::time::timeout(timeout, fut).await {
            Ok(res) => res.map_err(PoolError::from),
            Err(_) => Err(PoolError::ConnectTimeout(timeout)),
        }
    }

    /// Close every idle connection. Connections currently checked out are
    /// not affected; they will be released as their guards drop. The pool
    /// remains usable.
    pub async fn drain(&self) {
        let drained: Vec<Entry> = {
            let mut idle = self.inner.idle.lock();
            std::mem::take(&mut *idle)
        };
        for entry in drained {
            if let Err(error) = entry.connection.close().await {
                warn!(
                    target: "narwhal::pool",
                    error = %error,
                    "error while closing pooled connection"
                );
            }
        }
    }
}

/// Spawn an async close so callers are not charged for the round-trip
/// when a stale/unhealthy connection is discarded. If no Tokio runtime
/// is available (e.g. during shutdown) the connection is dropped, which
/// matches the legacy behaviour.
fn spawn_close(connection: Box<dyn Connection>) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            if let Err(error) = connection.close().await {
                warn!(
                    target: "narwhal::pool",
                    error = %error,
                    "background close failed"
                );
            }
        });
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Async cleanup at drop time: if a Tokio runtime is still live,
        // schedule each idle connection's async `close` on it. When called
        // outside any runtime the connections are dropped synchronously,
        // which is the same behaviour the underlying drivers exhibit when
        // their handles fall out of scope.
        let entries: Vec<Entry> = std::mem::take(self.idle.get_mut());
        if entries.is_empty() {
            return;
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            for entry in entries {
                handle.spawn(async move {
                    if let Err(error) = entry.connection.close().await {
                        warn!(
                            target: "narwhal::pool",
                            error = %error,
                            "background close failed"
                        );
                    }
                });
            }
        }
    }
}

/// RAII guard that returns its [`Connection`] to the pool on drop.
///
/// # Invariant
///
/// `connection` is `Some` from construction until `Drop::drop` takes it.
/// `Deref` / `DerefMut` rely on this invariant and will panic if it is
/// violated (which should be impossible through the public API).
pub struct PooledConnection {
    connection: Option<Box<dyn Connection>>,
    created_at: Instant,
    inner: Arc<Inner>,
    invalidated: bool,
    _permit: OwnedSemaphorePermit,
}

impl PooledConnection {
    /// Mark this connection as bad. On drop it will be closed in the
    /// background instead of being returned to the pool.
    ///
    /// Use this after observing a hard error (broken protocol state,
    /// dropped TCP, deadlock victim) to prevent the next caller from
    /// re-acquiring a corrupted session.
    pub fn invalidate(&mut self) {
        self.invalidated = true;
    }

    /// Whether this connection will be returned to the pool on drop.
    pub const fn is_invalidated(&self) -> bool {
        self.invalidated
    }
}

impl Deref for PooledConnection {
    type Target = dyn Connection;

    fn deref(&self) -> &Self::Target {
        let conn = self
            .connection
            .as_ref()
            .expect("PooledConnection::connection invariant: must be Some until drop");
        conn.as_ref()
    }
}

impl DerefMut for PooledConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let conn = self
            .connection
            .as_mut()
            .expect("PooledConnection::connection invariant: must be Some until drop");
        conn.as_mut()
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        let Some(connection) = self.connection.take() else {
            return;
        };
        if self.invalidated {
            debug!(target: "narwhal::pool", "discarding invalidated connection on drop");
            spawn_close(connection);
            return;
        }
        let mut idle = self.inner.idle.lock();
        idle.push(Entry {
            connection,
            idle_since: Instant::now(),
            created_at: self.created_at,
        });
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
            params: ConnectionParams::with(|p| {
                p.path = Some(path.into());
            }),
        }
    }

    fn pool() -> Pool {
        let driver: Arc<dyn DatabaseDriver> = Arc::new(SqliteDriver::new());
        Pool::new(
            driver,
            config(":memory:"),
            None,
            PoolConfig {
                max_size: NonZeroUsize::new(2).expect("2 is non-zero"),
                test_on_acquire: true,
                idle_timeout: None,
                max_lifetime: None,
                connect_timeout: None,
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

    /// H19: Drop runs close on inner connections (no panic).
    #[tokio::test]
    async fn pool_drop_runs_close_on_inner() {
        let pool = pool();
        // Acquire and release a connection so it goes idle.
        {
            let conn = pool.acquire().await.unwrap();
            drop(conn);
        }
        assert_eq!(pool.idle_count(), 1);
        // Drop the pool; the idle connection's close() should run without
        // panicking.
        drop(pool);
    }

    /// H19: `idle_count` does not panic under load (`parking_lot` Mutex has
    /// no poison).
    #[tokio::test]
    async fn idle_count_does_not_panic_under_load() {
        let pool = pool();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let p = pool.clone();
            handles.push(tokio::spawn(async move {
                let conn = p.acquire().await.unwrap();
                // Small delay to hold the connection
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                drop(conn);
                // idle_count should not panic even under contention
                let _ = p.idle_count();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
    }

    /// H14: invalidated connections are NOT returned to the pool.
    #[tokio::test]
    async fn invalidated_connection_is_not_pooled() {
        let pool = pool();
        {
            let mut conn = pool.acquire().await.unwrap();
            let _ = conn.execute("SELECT 1", &[]).await.unwrap();
            conn.invalidate();
            assert!(conn.is_invalidated());
        }
        // Even though one connection was acquired, the invalidate path
        // means it does not come back to the idle set.
        assert_eq!(pool.idle_count(), 0);
        // Pool is still usable: next acquire opens a fresh connection.
        let mut fresh = pool.acquire().await.unwrap();
        let _ = fresh.execute("SELECT 1", &[]).await.unwrap();
    }

    /// H14: `idle_timeout` discards connections that sat in the pool too long.
    #[tokio::test]
    async fn idle_timeout_discards_stale_connection() {
        let driver: Arc<dyn DatabaseDriver> = Arc::new(SqliteDriver::new());
        let pool = Pool::new(
            driver,
            config(":memory:"),
            None,
            PoolConfig {
                max_size: NonZeroUsize::new(2).expect("non-zero"),
                test_on_acquire: false,
                idle_timeout: Some(Duration::from_millis(50)),
                max_lifetime: None,
                connect_timeout: None,
            },
        );
        // Warm: acquire + release so an entry sits idle.
        {
            let _ = pool.acquire().await.unwrap();
        }
        assert_eq!(pool.idle_count(), 1);
        // Wait past the idle_timeout.
        tokio::time::sleep(Duration::from_millis(80)).await;
        // The stale entry is silently discarded and a fresh one is created.
        let guard = pool.acquire().await.unwrap();
        // The discarded entry was popped and closed in the background;
        // the new acquire put no entry back in idle (still checked out).
        assert_eq!(pool.idle_count(), 0);
        drop(guard);
    }

    /// H14: `max_lifetime` discards connections older than the cap.
    #[tokio::test]
    async fn max_lifetime_caps_connection_age() {
        let driver: Arc<dyn DatabaseDriver> = Arc::new(SqliteDriver::new());
        let pool = Pool::new(
            driver,
            config(":memory:"),
            None,
            PoolConfig {
                max_size: NonZeroUsize::new(2).expect("non-zero"),
                test_on_acquire: false,
                idle_timeout: None,
                max_lifetime: Some(Duration::from_millis(50)),
                connect_timeout: None,
            },
        );
        {
            let _ = pool.acquire().await.unwrap();
        }
        assert_eq!(pool.idle_count(), 1);
        tokio::time::sleep(Duration::from_millis(80)).await;
        let guard = pool.acquire().await.unwrap();
        assert_eq!(pool.idle_count(), 0);
        drop(guard);
    }
}
