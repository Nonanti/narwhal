//! Audit sinks.
//!
//! A sink consumes a single rendered JSONL line and persists it. The
//! orchestrator (built in a follow-up commit) owns the channel and
//! fans out every event to all configured sinks; sinks themselves are
//! single-consumer.
//!
//! Available sinks:
//!
//! - [`FileSink`] — append-only file with size-based rotation.
//! - [`StdoutSink`] — line-buffered stdout, useful for `--audit-stdout`.
//! - `SyslogSink` — local syslog daemon, gated by the `syslog` cargo
//!   feature.

pub mod file;
pub mod stdout;
#[cfg(feature = "syslog")]
pub mod syslog;

pub use file::FileSink;
pub use stdout::StdoutSink;
#[cfg(feature = "syslog")]
pub use syslog::SyslogSink;

/// Errors raised by any sink.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SinkError {
    /// Underlying I/O failure (open, write, fsync, rename).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Configured path resolved to an unusable form (e.g. empty after
    /// strftime expansion or a non-UTF8 component).
    #[error("invalid path: {0}")]
    Path(String),
    /// Syslog connection refused / unreachable.
    #[cfg(feature = "syslog")]
    #[error("syslog: {0}")]
    Syslog(String),
}

/// One audit sink. Writes are append-only and must not block beyond
/// the inherent cost of the underlying I/O.
///
/// The trait is intentionally async: the file sink fsyncs through the
/// tokio runtime, and the syslog sink may block on a unix socket.
pub trait AuditSink: Send + Sync + std::fmt::Debug {
    /// Append one JSONL line (no trailing newline — the sink adds it).
    fn write<'a>(
        &'a self,
        line: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SinkError>> + Send + 'a>>;

    /// Flush any buffered state. Called on shutdown and on each rotate.
    fn flush<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SinkError>> + Send + 'a>>;
}
