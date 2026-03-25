//! Append-only JSONL audit log for narwhal.
//!
//! Optional, opt-in compliance sink. Every executed query, connection
//! lifecycle event, configuration change, and plugin load is projected
//! as a self-contained JSON object on its own line.
//!
//! ## Design constraints
//!
//! - **Append-only.** No public API edits or deletes audit lines.
//!   Rotation moves files aside under their original name; it does
//!   not modify them.
//! - **UTC time discipline.** All timestamps are `chrono::DateTime<Utc>`
//!   serialised as RFC 3339 with millisecond precision.
//! - **Lossy by default, blocking on opt-in.** A bounded mpsc channel
//!   sits between the emit sites and the sink task. The default drops
//!   the oldest line under contention and logs a `tracing::warn`. Set
//!   `block_on_full = true` for compliance-first deployments where
//!   query dispatch must wait for audit acknowledgement.
//! - **Redaction is best-effort, not a security boundary.** SQL secret
//!   masking reuses [`narwhal_history::redact_sql_secrets`]; document
//!   filesystem-level ACLs as the real protection.
//!
//! The schema is documented in [`event::AuditEvent`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod event;
pub mod redactor;
pub mod service;
pub mod sinks;

pub use config::{AuditConfig, SinkSpec};
pub use event::{AUDIT_SCHEMA_VERSION, AuditEvent, render_line};
pub use redactor::{Redactor, RedactorConfig};
pub use service::{AuditService, AuditServiceBuilder};
pub use sinks::{AuditSink, SinkError};

/// Errors raised by the audit subsystem.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuditError {
    /// A sink failed during open, write, or rotation.
    #[error("sink: {0}")]
    Sink(#[from] SinkError),
    /// JSON serialisation of an event failed.
    ///
    /// This is exceptional — every `AuditEvent` variant is composed of
    /// types that are always serialisable, so a failure here points at
    /// a programmer error (e.g. a future variant introducing an
    /// unsupported type) rather than runtime data.
    #[error("serialisation: {0}")]
    Serde(#[from] serde_json::Error),
}
