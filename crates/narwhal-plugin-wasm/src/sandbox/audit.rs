//! Capability-denial audit log.
//!
//! Two layers:
//!
//! * [`tracing`]-based structured emission under the target
//!   `narwhal::plugin::audit` — the operator-facing log.
//! * [`AuditSink`] trait so tests and embedders that need a
//!   programmatic feed can subscribe to denial events.
//!
//! The production path is the tracing one: subscribers (the binary's
//! global tracing layer) already filter by target, so the sandbox
//! never needs a config flag to silence the log.

use std::sync::{Arc, Mutex};

use crate::capability::CapabilityKind;

use super::decision::AuditId;
use super::operation::Operation;

/// Tracing target every denial event uses. Filter by this in your
/// `RUST_LOG`/`tracing-subscriber` config to surface a denial-only
/// stream.
///
/// Exposed at the crate root via [`crate::AUDIT_TARGET`] so embedders
/// configuring `tracing-subscriber` filters do not have to reach
/// into the sandbox sub-module.
pub const AUDIT_TARGET: &str = "narwhal::plugin::audit";

/// Structured payload one denial emits.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AuditEvent {
    pub plugin: String,
    pub kind: CapabilityKind,
    pub operation: String,
    pub reason: String,
    pub audit_id: AuditId,
}

/// Subscriber for denial audit events. Implemented by:
///
/// * [`TracingAuditSink`] — production. Forwards to `tracing::warn!`.
/// * [`RecordingAuditSink`] — tests. Captures every event in a
///   shared `Vec`.
/// * [`NoopAuditSink`] — silent; used by hosts that handle audit
///   via the tracing layer alone.
pub trait AuditSink: Send + Sync {
    fn emit(&self, event: AuditEvent);
}

/// Forwards every event to `tracing::warn!` under [`AUDIT_TARGET`].
#[derive(Debug, Clone, Copy, Default)]
pub struct TracingAuditSink;

impl AuditSink for TracingAuditSink {
    fn emit(&self, event: AuditEvent) {
        tracing::warn!(
            target: AUDIT_TARGET,
            plugin = %event.plugin,
            kind = %event.kind,
            operation = %event.operation,
            reason = %event.reason,
            audit_id = event.audit_id.get(),
            "plugin capability denied",
        );
    }
}

/// Silent sink. Embedders may pick this when they only want the
/// tracing layer's stream (the default [`TracingAuditSink`] already
/// emits through tracing — using [`NoopAuditSink`] is the explicit
/// opt-out).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopAuditSink;

impl AuditSink for NoopAuditSink {
    fn emit(&self, _event: AuditEvent) {}
}

/// Test sink: stores every captured event for later assertions.
#[derive(Debug, Clone, Default)]
pub struct RecordingAuditSink {
    inner: Arc<Mutex<Vec<AuditEvent>>>,
}

impl RecordingAuditSink {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Clone every captured event. Returns the snapshot so the
    /// caller can drop the lock immediately.
    #[must_use]
    pub fn snapshot(&self) -> Vec<AuditEvent> {
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Drain captured events.
    pub fn drain(&self) -> Vec<AuditEvent> {
        self.inner
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default()
    }

    /// Total event count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().map_or(0, |g| g.len())
    }

    /// True when no events have been captured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl AuditSink for RecordingAuditSink {
    fn emit(&self, event: AuditEvent) {
        if let Ok(mut g) = self.inner.lock() {
            g.push(event);
        }
    }
}

/// Build an [`AuditEvent`] from the enforcer's state and emit it
/// through `sink`. Returns the newly-allocated [`AuditId`] so the
/// caller can stash it inside the resulting [`super::Decision`].
pub(super) fn record(sink: &dyn AuditSink, plugin: &str, op: &Operation, reason: &str) -> AuditId {
    let audit_id = AuditId::next();
    sink.emit(AuditEvent {
        plugin: plugin.to_owned(),
        kind: op.kind(),
        operation: op.describe(),
        reason: reason.to_owned(),
        audit_id,
    });
    audit_id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_sink_captures_in_order() {
        let sink = RecordingAuditSink::new();
        record(&sink, "p", &Operation::StateAccess, "no state");
        record(
            &sink,
            "p",
            &Operation::CmdInvoke { name: "run".into() },
            "no cmd",
        );
        let snap = sink.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].reason, "no state");
        assert_eq!(snap[1].operation, "cmd.invoke:run");
    }

    #[test]
    fn noop_sink_records_nothing() {
        let sink = NoopAuditSink;
        sink.emit(AuditEvent {
            plugin: "p".into(),
            kind: CapabilityKind::State,
            operation: "state".into(),
            reason: "nope".into(),
            audit_id: AuditId::next(),
        });
        // No panics, no observable state.
    }

    #[test]
    fn drain_resets_recording_sink() {
        let sink = RecordingAuditSink::new();
        record(&sink, "p", &Operation::StateAccess, "x");
        assert_eq!(sink.len(), 1);
        let drained = sink.drain();
        assert_eq!(drained.len(), 1);
        assert!(sink.is_empty());
    }
}
