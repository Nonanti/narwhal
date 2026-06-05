//! Stdout sink.
//!
//! Writes one JSON object per line to the process's stdout. Intended
//! for ephemeral debug runs (`narwhal --audit-stdout`) and for
//! integration with external pipelines (`narwhal ... | jq …`).
//!
//! The sink does **not** colourise output and does not interleave with
//! TUI rendering — callers should only enable it when running in
//! headless / batch mode.

use std::pin::Pin;

use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use super::{AuditSink, SinkError};

/// Stdout-backed sink.
#[derive(Debug, Default)]
pub struct StdoutSink {
    /// Mutex serialises async writes so interleaved tokio tasks can't
    /// shred each other's lines.
    lock: Mutex<()>,
}

impl StdoutSink {
    /// Build a fresh stdout sink. Cheap; no I/O performed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl AuditSink for StdoutSink {
    fn write<'a>(
        &'a self,
        line: &'a str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), SinkError>> + Send + 'a>> {
        Box::pin(async move {
            let _guard = self.lock.lock().await;
            let mut out = tokio::io::stdout();
            out.write_all(line.as_bytes()).await?;
            out.write_all(b"\n").await?;
            // No flush here — stdout is line-buffered in cooked mode
            // and per-event flush is wasteful. `flush` does it.
            Ok(())
        })
    }

    fn flush<'a>(
        &'a self,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), SinkError>> + Send + 'a>> {
        Box::pin(async move {
            let _guard = self.lock.lock().await;
            tokio::io::stdout().flush().await?;
            Ok(())
        })
    }
}
