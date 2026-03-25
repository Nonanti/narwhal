//! File-backed JSONL sink.
//!
//! Opens the configured path in append mode and writes one line per
//! event. Rotates the file when its on-disk size crosses
//! [`FileSinkConfig::rotate_bytes`]; rotation renames the active file
//! to `<original>.<UTC-stamp>` and re-opens the original name fresh.
//!
//! ## Path expansion
//!
//! The path is run through `chrono::Utc::now().format` at open time,
//! so operators can write `audit-%Y-%m-%d.jsonl` and get a fresh file
//! per UTC day. The format string is the standard strftime alphabet
//! (`%Y`, `%m`, `%d`, `%H`, `%M`, `%S`).
//!
//! ## Durability
//!
//! `fsync_each_write` (default true) calls `tokio::fs::File::sync_data`
//! after every line. This is what makes the log usable for SOC2 / ISO
//! evidence: a crash leaves the log truncated at a whole line, never
//! losing already-acknowledged events. The cost is one fsync per
//! query; operators who can tolerate replay loss for throughput may
//! set it false.

use std::path::{Path, PathBuf};
use std::pin::Pin;

use chrono::Utc;
use tokio::fs::{File, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use super::{AuditSink, SinkError};

/// Knobs for [`FileSink`].
#[derive(Debug, Clone)]
pub struct FileSinkConfig {
    /// Path template (strftime tokens permitted).
    pub path: PathBuf,
    /// Rotate when the active file exceeds this many bytes. Default
    /// 100 MiB. Set to `u64::MAX` to disable rotation.
    pub rotate_bytes: u64,
    /// Call `sync_data` after every write. Default true.
    pub fsync_each_write: bool,
}

impl FileSinkConfig {
    /// Reasonable defaults for `path`. 100 MiB rotation, fsync on.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            rotate_bytes: 100 * 1024 * 1024,
            fsync_each_write: true,
        }
    }
}

/// Append-only file sink.
#[derive(Debug)]
pub struct FileSink {
    cfg: FileSinkConfig,
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    /// The resolved (post-strftime) path the active handle points at.
    active_path: PathBuf,
    /// Open append handle. Recreated after rotation.
    file: File,
    /// Bytes written since last open. Tracked locally to avoid an
    /// fstat per write.
    bytes: u64,
}

impl FileSink {
    /// Open the sink. Resolves the path template, creates parent
    /// directories if missing, and opens the file in append mode.
    ///
    /// # Errors
    ///
    /// Returns [`SinkError::Io`] if directory creation or open fails,
    /// or [`SinkError::Path`] if the resolved path is empty.
    pub async fn open(cfg: FileSinkConfig) -> Result<Self, SinkError> {
        let active_path = resolve_path(&cfg.path)?;
        if let Some(parent) = active_path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&active_path)
            .await?;
        let bytes = file.metadata().await?.len();
        Ok(Self {
            cfg,
            state: Mutex::new(State {
                active_path,
                file,
                bytes,
            }),
        })
    }

    async fn rotate(&self, state: &mut State) -> Result<(), SinkError> {
        // Suffix the active file with a UTC timestamp, then reopen
        // the original template. We sync_data the outgoing handle so
        // operators can ship the rotated artefact safely.
        state.file.sync_data().await?;
        drop(std::mem::replace(
            &mut state.file,
            // Placeholder; replaced before the next read.
            tokio::fs::File::from_std(
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(devnull_path())
                    .map_err(SinkError::Io)?,
            ),
        ));
        let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let rotated = rotated_name(&state.active_path, &stamp);
        tokio::fs::rename(&state.active_path, &rotated).await?;

        let fresh_path = resolve_path(&self.cfg.path)?;
        if let Some(parent) = fresh_path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }
        let fresh = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&fresh_path)
            .await?;
        state.active_path = fresh_path;
        state.file = fresh;
        state.bytes = 0;
        Ok(())
    }
}

impl AuditSink for FileSink {
    fn write<'a>(
        &'a self,
        line: &'a str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), SinkError>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            // Line is JSON without a trailing newline; rotation
            // decision is made *before* the write so a single line
            // never straddles two files.
            let line_bytes = line.len() as u64 + 1;
            if state.bytes.saturating_add(line_bytes) > self.cfg.rotate_bytes {
                self.rotate(&mut state).await?;
            }
            state.file.write_all(line.as_bytes()).await?;
            state.file.write_all(b"\n").await?;
            state.bytes += line_bytes;
            if self.cfg.fsync_each_write {
                state.file.sync_data().await?;
            }
            Ok(())
        })
    }

    fn flush<'a>(
        &'a self,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), SinkError>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            state.file.flush().await?;
            state.file.sync_data().await?;
            Ok(())
        })
    }
}

/// Resolve strftime tokens in `template` against the current UTC time
/// and return the materialised path.
fn resolve_path(template: &Path) -> Result<PathBuf, SinkError> {
    let raw = template
        .to_str()
        .ok_or_else(|| SinkError::Path(format!("non-UTF8 path: {}", template.display())))?;
    let resolved = Utc::now().format(raw).to_string();
    if resolved.is_empty() {
        return Err(SinkError::Path("empty path after strftime".into()));
    }
    Ok(PathBuf::from(resolved))
}

/// Build the rotated filename: `<original>.<stamp>`. Preserves the
/// extension so log shippers can still match on `*.jsonl`.
fn rotated_name(active: &Path, stamp: &str) -> PathBuf {
    let mut s = active.as_os_str().to_owned();
    s.push(".");
    s.push(stamp);
    PathBuf::from(s)
}

#[cfg(unix)]
const fn devnull_path() -> &'static str {
    "/dev/null"
}

#[cfg(windows)]
const fn devnull_path() -> &'static str {
    "NUL"
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::fs;

    #[tokio::test]
    async fn appends_lines_and_creates_parents() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested/audit.jsonl");
        let sink = FileSink::open(FileSinkConfig::new(&path)).await.unwrap();
        sink.write(r#"{"kind":"a"}"#).await.unwrap();
        sink.write(r#"{"kind":"b"}"#).await.unwrap();
        sink.flush().await.unwrap();
        let body = fs::read_to_string(&path).await.unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines, vec![r#"{"kind":"a"}"#, r#"{"kind":"b"}"#]);
    }

    #[tokio::test]
    async fn rotates_when_threshold_crossed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let mut cfg = FileSinkConfig::new(&path);
        cfg.rotate_bytes = 30;
        cfg.fsync_each_write = false;
        let sink = FileSink::open(cfg).await.unwrap();
        // Each line is 13 bytes payload + newline = 14, so two lines
        // (28 B) fit; the third crosses 30 and triggers rotation.
        sink.write(r#"{"kind":"a"}"#).await.unwrap();
        sink.write(r#"{"kind":"b"}"#).await.unwrap();
        sink.write(r#"{"kind":"c"}"#).await.unwrap();
        sink.flush().await.unwrap();

        let mut entries = vec![];
        let mut rd = fs::read_dir(dir.path()).await.unwrap();
        while let Some(e) = rd.next_entry().await.unwrap() {
            entries.push(e.file_name().into_string().unwrap());
        }
        entries.sort();
        // One rotated file + the fresh active file.
        assert_eq!(entries.len(), 2, "rotation did not happen: {entries:?}");
        let active = fs::read_to_string(&path).await.unwrap();
        assert_eq!(active.trim(), r#"{"kind":"c"}"#);
    }

    #[tokio::test]
    async fn strftime_path_expansion() {
        let dir = tempdir().unwrap();
        let template = dir.path().join("audit-%Y.jsonl");
        let sink = FileSink::open(FileSinkConfig::new(&template))
            .await
            .unwrap();
        sink.write(r#"{"kind":"x"}"#).await.unwrap();
        sink.flush().await.unwrap();
        let year = Utc::now().format("%Y").to_string();
        let resolved = dir.path().join(format!("audit-{year}.jsonl"));
        assert!(resolved.exists(), "expected {resolved:?} to exist");
    }
}
