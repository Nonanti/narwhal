//! Audit orchestrator.
//!
//! Sits between emit sites (query dispatch, session lifecycle, plugin
//! load, config changes) and the configured set of [`AuditSink`]s.
//!
//! ## Architecture
//!
//! Emit sites push [`AuditEvent`] into a bounded `tokio::sync::mpsc`
//! channel. A single worker task receives, applies the [`Redactor`],
//! renders the canonical JSON line, then writes to every sink in
//! sequence. Sinks are independent — a failure on one is logged via
//! `tracing::warn` and does **not** stop the others.
//!
//! ## Back-pressure
//!
//! - **Lossy mode (default).** When the channel is full, the emitter
//!   drops the event and bumps a `tracing::warn` counter. Query
//!   dispatch is never blocked.
//! - **Block mode** (`block_on_full = true`). The emitter awaits the
//!   channel. Query dispatch waits with it. Use this only in
//!   compliance-first deployments.
//!
//! ## Shutdown
//!
//! Dropping all [`AuditService`] handles closes the channel; the
//! worker drains any in-flight events, calls `flush` on every sink,
//! then exits. Call [`AuditService::shutdown`] for an awaitable
//! flush guarantee (used by the app's outer `Drop`-equivalent
//! shutdown sequence).

use std::sync::Arc;

use chrono::Utc;
use tokio::sync::{Mutex, Notify, mpsc};
use tokio::task::JoinHandle;
use tracing::warn;

use crate::event::{AuditEvent, render_line};
use crate::redactor::Redactor;
use crate::sinks::AuditSink;

/// Channel depth between emitters and the worker. Sized to absorb a
/// handful of bursts (e.g. a `:run-all` execution) without dropping in
/// lossy mode. Configurable via [`AuditServiceBuilder::channel_capacity`].
const DEFAULT_CHANNEL_CAPACITY: usize = 1024;

/// Handle held by every emit site.
///
/// Cheap to clone (`Arc` inside). Dropping a clone is fine — the
/// worker only stops once **every** handle is dropped.
#[derive(Clone)]
pub struct AuditService {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for AuditService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditService")
            .field("block_on_full", &self.inner.block_on_full)
            .field("sink_count", &self.inner.sink_count)
            .finish_non_exhaustive()
    }
}

struct Inner {
    tx: mpsc::Sender<AuditEvent>,
    block_on_full: bool,
    sink_count: usize,
    /// Cross-clone shutdown signal. The worker selects on both the
    /// receive channel and this notify; on shutdown the worker
    /// closes its receiver, which is what releases parked
    /// `block_on_full=true` emitters with `Closed` instead of the
    /// deadlock we had before review fix M1 / MR-C1.
    shutdown: Arc<Notify>,
    /// Worker join handle, taken once on shutdown. `Mutex` because
    /// shutdown is callable from any task.
    join: Mutex<Option<JoinHandle<()>>>,
}

/// Builder for an [`AuditService`].
///
/// Holds the sink list and configuration; [`AuditServiceBuilder::start`]
/// spawns the worker and returns the live handle.
#[allow(missing_debug_implementations)]
pub struct AuditServiceBuilder {
    sinks: Vec<Arc<dyn AuditSink>>,
    redactor: Redactor,
    block_on_full: bool,
    channel_capacity: usize,
}

impl Default for AuditServiceBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl AuditServiceBuilder {
    /// Empty builder — lossy mode, default channel capacity, no sinks,
    /// pass-through redactor.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sinks: Vec::new(),
            redactor: Redactor::default(),
            block_on_full: false,
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
        }
    }

    /// Add one sink. Order is preserved; sinks receive events in the
    /// order they were registered.
    #[must_use]
    pub fn with_sink(mut self, sink: Arc<dyn AuditSink>) -> Self {
        self.sinks.push(sink);
        self
    }

    /// Install the redactor.
    #[must_use]
    pub fn with_redactor(mut self, redactor: Redactor) -> Self {
        self.redactor = redactor;
        self
    }

    /// Switch to block-on-full mode.
    #[must_use]
    pub const fn block_on_full(mut self, enabled: bool) -> Self {
        self.block_on_full = enabled;
        self
    }

    /// Override the channel capacity. Useful in tests; in production
    /// the default is usually fine.
    #[must_use]
    pub const fn channel_capacity(mut self, capacity: usize) -> Self {
        self.channel_capacity = capacity;
        self
    }

    /// Spawn the worker and return a live service handle.
    ///
    /// Returns `None` when no sinks are configured: there is nothing
    /// to forward events to, and emit sites should treat the optional
    /// service as absent.
    pub fn start(self) -> Option<AuditService> {
        if self.sinks.is_empty() {
            return None;
        }
        let (tx, rx) = mpsc::channel::<AuditEvent>(self.channel_capacity);
        let sinks = self.sinks.clone();
        let redactor = self.redactor.clone();
        let sink_count = self.sinks.len();
        let shutdown = Arc::new(Notify::new());
        let join = tokio::spawn(worker(rx, sinks, redactor, Arc::clone(&shutdown)));
        Some(AuditService {
            inner: Arc::new(Inner {
                tx,
                block_on_full: self.block_on_full,
                sink_count,
                shutdown,
                join: Mutex::new(Some(join)),
            }),
        })
    }
}

impl AuditService {
    /// Build a service from configuration. Convenience over the
    /// builder for the common path.
    #[must_use]
    pub fn builder() -> AuditServiceBuilder {
        AuditServiceBuilder::new()
    }

    /// Number of sinks the worker is fanning out to. Surface this in
    /// `:audit status` once the CLI lands.
    #[must_use]
    pub fn sink_count(&self) -> usize {
        self.inner.sink_count
    }

    /// Forward one event.
    ///
    /// In lossy mode this returns immediately even when the channel
    /// is full (the event is dropped and a `tracing::warn` is
    /// emitted). In block mode this awaits the channel.
    ///
    /// Review fix M1 / MR-C1: no per-emit mutex. The shutdown path
    /// closes the receiver, so any `block_on_full=true` emitter
    /// parked here wakes up with `SendError::Closed` instead of
    /// deadlocking.
    pub async fn emit(&self, event: AuditEvent) {
        if self.inner.block_on_full {
            if let Err(err) = self.inner.tx.send(event).await {
                warn!(target: "narwhal::audit", error = %err, "audit channel closed; event dropped");
            }
            return;
        }
        if let Err(err) = self.inner.tx.try_send(event) {
            match err {
                mpsc::error::TrySendError::Full(_) => {
                    warn!(
                        target: "narwhal::audit",
                        "audit channel full; event dropped (set audit.block_on_full=true to block)"
                    );
                }
                mpsc::error::TrySendError::Closed(_) => {
                    warn!(target: "narwhal::audit", "audit channel closed; event dropped");
                }
            }
        }
    }

    /// Signal the worker to drain and exit, then await its `flush`
    /// pass on every sink.
    ///
    /// Other clones of [`AuditService`] may still exist when shutdown
    /// is called — the worker uses an out-of-band [`Notify`] rather
    /// than `mpsc` close so the call is correct regardless of how
    /// many handles are alive. Subsequent `emit` calls on surviving
    /// clones will fail to send (channel is drained, worker is gone)
    /// and log a `tracing::warn`; the channel itself stays open until
    /// the last sender drops.
    ///
    /// Idempotent: calling shutdown twice from concurrent tasks is
    /// safe — the second call observes `None` and returns immediately.
    ///
    /// Review fix M1 / MR-C1: the worker calls `rx.close()` as the
    /// first thing it does on shutdown. That closes the channel
    /// from the receiver side and wakes any `block_on_full=true`
    /// emitter parked on `send().await` with `SendError::Closed` —
    /// no Mutex-wrapped sender required.
    pub async fn shutdown(&self) {
        // R3-N2: take the join handle inside a tight scope so we
        // release the Mutex before awaiting termination. A second
        // concurrent `shutdown()` call now observes `None`
        // immediately instead of blocking on the live first call.
        let join = {
            let mut guard = self.inner.join.lock().await;
            guard.take()
        };
        let Some(join) = join else {
            return;
        };
        self.inner.shutdown.notify_one();
        let _ = join.await;
    }
}

async fn worker(
    mut rx: mpsc::Receiver<AuditEvent>,
    sinks: Vec<Arc<dyn AuditSink>>,
    redactor: Redactor,
    shutdown: Arc<Notify>,
) {
    loop {
        tokio::select! {
            biased;
            // Drain the channel first so a `shutdown` that races
            // with an in-flight `emit` doesn't lose the event.
            maybe_event = rx.recv() => {
                let Some(event) = maybe_event else { break };
                process_one(event, &sinks, &redactor).await;
            }
            () = shutdown.notified() => {
                // MR-C1: close the receiver first so any parked
                // `block_on_full=true` emitter wakes with `Closed`,
                // then drain whatever is already in the buffer and
                // exit cleanly.
                rx.close();
                while let Some(event) = rx.recv().await {
                    process_one(event, &sinks, &redactor).await;
                }
                break;
            }
        }
    }
    for sink in &sinks {
        if let Err(error) = sink.flush().await {
            warn!(target: "narwhal::audit", error = %error, "sink flush failed on shutdown");
        }
    }
}

async fn process_one(mut event: AuditEvent, sinks: &[Arc<dyn AuditSink>], redactor: &Redactor) {
    redactor.apply(&mut event);
    let line = match render_line(&event, Utc::now()) {
        Ok(s) => s,
        Err(error) => {
            warn!(target: "narwhal::audit", error = %error, "render_line failed; event dropped");
            return;
        }
    };
    for sink in sinks {
        if let Err(error) = sink.write(&line).await {
            warn!(
                target: "narwhal::audit",
                error = %error,
                "sink write failed; continuing with remaining sinks"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::AuditEvent;
    use crate::sinks::SinkError;
    use std::sync::Mutex as StdMutex;
    use uuid::Uuid;

    /// Captures every line written, for assertion.
    #[derive(Debug, Default)]
    struct CapturingSink {
        lines: StdMutex<Vec<String>>,
        flushed: StdMutex<u32>,
    }

    impl AuditSink for CapturingSink {
        fn write<'a>(
            &'a self,
            line: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SinkError>> + Send + 'a>>
        {
            Box::pin(async move {
                self.lines.lock().unwrap().push(line.to_owned());
                Ok(())
            })
        }
        fn flush<'a>(
            &'a self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SinkError>> + Send + 'a>>
        {
            Box::pin(async move {
                *self.flushed.lock().unwrap() += 1;
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn fans_out_to_all_sinks_and_flushes_on_shutdown() {
        let s1 = Arc::new(CapturingSink::default());
        let s2 = Arc::new(CapturingSink::default());
        let svc = AuditService::builder()
            .with_sink(s1.clone())
            .with_sink(s2.clone())
            .start()
            .expect("two sinks installed");
        svc.emit(AuditEvent::Configuration {
            change: "test".into(),
            by: "rust".into(),
        })
        .await;
        // Force one round-trip through the worker.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        svc.shutdown().await;

        assert_eq!(s1.lines.lock().unwrap().len(), 1);
        assert_eq!(s2.lines.lock().unwrap().len(), 1);
        assert_eq!(*s1.flushed.lock().unwrap(), 1);
        assert_eq!(*s2.flushed.lock().unwrap(), 1);
        let line = s1.lines.lock().unwrap()[0].clone();
        assert!(line.contains(r#""kind":"configuration""#));
        assert!(line.contains(r#""change":"test""#));
    }

    #[tokio::test]
    async fn redaction_applied_before_render() {
        let s = Arc::new(CapturingSink::default());
        let svc = AuditService::builder()
            .with_sink(s.clone())
            .with_redactor(Redactor::new(crate::RedactorConfig {
                redact_passwords: true,
                redact_columns: vec![],
            }))
            .start()
            .unwrap();
        svc.emit(AuditEvent::Query {
            session_id: Uuid::nil(),
            sql: "ALTER USER alice WITH PASSWORD 'topsecret'".into(),
            params: vec![],
            rows: None,
            elapsed_ms: 0,
            succeeded: true,
            error: None,
        })
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        svc.shutdown().await;
        let line = s.lines.lock().unwrap()[0].clone();
        assert!(!line.contains("topsecret"), "password leaked: {line}");
        assert!(line.contains("***"));
    }

    #[tokio::test]
    async fn no_sinks_returns_none() {
        assert!(AuditService::builder().start().is_none());
    }

    /// Review fix M1: a shutdown that races with a block-mode
    /// `emit` must close the channel rather than deadlock. Without
    /// the fix this test hangs indefinitely.
    #[tokio::test]
    async fn block_on_full_shutdown_does_not_deadlock() {
        let s = Arc::new(CapturingSink::default());
        let svc = AuditService::builder()
            .with_sink(s.clone())
            .block_on_full(true)
            .channel_capacity(1)
            .start()
            .unwrap();
        // Fill the channel; the worker is still draining so capacity
        // is racy, but with channel_capacity=1 we're guaranteed to
        // hit `Full` quickly. Spawn a couple of emitters parked on
        // the channel, then shut down.
        let mut handles = Vec::new();
        for _ in 0..4 {
            let svc = svc.clone();
            handles.push(tokio::spawn(async move {
                svc.emit(AuditEvent::Configuration {
                    change: "x".into(),
                    by: "x".into(),
                })
                .await;
            }));
        }
        // Give the emitters time to park inside `sender.send().await`.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        // The shutdown call must return without waiting on the
        // parked emitters.
        tokio::time::timeout(std::time::Duration::from_secs(2), svc.shutdown())
            .await
            .expect("shutdown deadlocked despite block_on_full");
        for handle in handles {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
        }
    }

    #[tokio::test]
    async fn block_on_full_waits_for_room() {
        let s = Arc::new(CapturingSink::default());
        let svc = AuditService::builder()
            .with_sink(s.clone())
            .block_on_full(true)
            .channel_capacity(1)
            .start()
            .unwrap();
        for i in 0..5 {
            svc.emit(AuditEvent::Configuration {
                change: format!("c{i}"),
                by: "test".into(),
            })
            .await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        svc.shutdown().await;
        assert_eq!(s.lines.lock().unwrap().len(), 5);
    }
}
