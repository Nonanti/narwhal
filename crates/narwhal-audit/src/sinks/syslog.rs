//! Optional syslog sink (Linux / Unix).
//!
//! Gated behind the `syslog` cargo feature so non-Linux targets don't
//! pull the dependency. Connects to the local syslog daemon via Unix
//! domain socket and forwards each JSONL line as a single message.
//!
//! Falls back gracefully: if the daemon is unreachable at open time,
//! returns `SinkError::Syslog`. The supervising orchestrator decides
//! whether to retry, fail open, or fail closed.

use std::pin::Pin;

use syslog::{Facility, Formatter3164};
use tokio::sync::Mutex;

use super::{AuditSink, SinkError};

/// Syslog-backed sink.
pub struct SyslogSink {
    logger: Mutex<syslog::Logger<syslog::LoggerBackend, Formatter3164>>,
}

impl std::fmt::Debug for SyslogSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyslogSink").finish_non_exhaustive()
    }
}

impl SyslogSink {
    /// Open a connection to the local syslog daemon.
    ///
    /// # Errors
    ///
    /// Returns [`SinkError::Syslog`] when the daemon socket can't be
    /// opened.
    pub fn open() -> Result<Self, SinkError> {
        let formatter = Formatter3164 {
            facility: Facility::LOG_AUTH,
            hostname: None,
            process: "narwhal".into(),
            pid: std::process::id(),
        };
        let logger = syslog::unix(formatter).map_err(|e| SinkError::Syslog(e.to_string()))?;
        Ok(Self {
            logger: Mutex::new(logger),
        })
    }
}

impl AuditSink for SyslogSink {
    fn write<'a>(
        &'a self,
        line: &'a str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), SinkError>> + Send + 'a>> {
        Box::pin(async move {
            let mut logger = self.logger.lock().await;
            logger
                .info(line)
                .map_err(|e| SinkError::Syslog(e.to_string()))?;
            Ok(())
        })
    }

    fn flush<'a>(
        &'a self,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), SinkError>> + Send + 'a>> {
        Box::pin(async move { Ok(()) })
    }
}
