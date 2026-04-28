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
use chrono::format::{Item, StrftimeItems};
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
    /// Open append handle. `Option` so [`FileSink::rotate`] can
    /// `take()` it during the rename window (review fix M2). Outside
    /// rotation the handle is always `Some`.
    file: Option<File>,
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
                file: Some(file),
                bytes,
            }),
        })
    }

    /// Review fix M2: drop the outgoing handle via `Option::take`
    /// instead of swapping in a placeholder `/dev/null` file opened
    /// with sync `std::fs`. The previous placeholder pattern blocked
    /// the tokio executor on the `open()` syscall while holding the
    /// state mutex.
    ///
    /// Review fix M3: stamp is millisecond-granularity and probes
    /// for an existing rotated file, falling back to a monotonic
    /// suffix. This protects high-throughput audit deployments from
    /// silently losing a rotated file when two rotations happen
    /// inside the same second.
    async fn rotate(&self, state: &mut State) -> Result<(), SinkError> {
        // Sync + drop the outgoing handle so the rename below sees a
        // closed file on Windows (which forbids renaming open files)
        // and operators can ship the rotated artefact safely.
        if let Some(outgoing) = state.file.take() {
            outgoing.sync_data().await?;
            drop(outgoing);
        }
        // MR-M1: rename + create-target atomically inside the same
        // probe loop. Each iteration tries `OpenOptions::create_new`
        // on the candidate path; only if that succeeds do we
        // `rename(active -> candidate)`. The transient empty file
        // is removed immediately on success. This closes the TOCTOU
        // hole the previous `metadata().is_err() -> rename` pattern
        // had against concurrent writers.
        let rotated = self.pick_rotated_name_atomic(&state.active_path).await?;
        // R3-N1: clean up the placeholder if the rename fails so a
        // permission / disk-full failure doesn't leak an empty
        // file named like a real rotated artefact (which would
        // confuse log shippers and inflate next-stamp suffix probes).
        if let Err(e) = tokio::fs::rename(&state.active_path, &rotated).await {
            let _ = tokio::fs::remove_file(&rotated).await;
            return Err(SinkError::Io(e));
        }

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
        state.file = Some(fresh);
        state.bytes = 0;
        Ok(())
    }

    /// MR-M1: pick a free rotated path *atomically*. Tries the
    /// millisecond stamp first (common case), then numeric suffixes,
    /// then a nanosecond stamp as a final hail-Mary; gives up after
    /// [`MAX_ROTATION_ATTEMPTS`] with [`SinkError::Path`] so the
    /// caller sees a real error instead of silently overwriting a
    /// previous artefact.
    async fn pick_rotated_name_atomic(&self, active: &Path) -> Result<PathBuf, SinkError> {
        let stamp = Utc::now().format("%Y%m%dT%H%M%S%3fZ").to_string();

        async fn try_claim(candidate: &Path) -> std::io::Result<bool> {
            match tokio::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(candidate)
                .await
            {
                Ok(file) => {
                    drop(file);
                    // We only needed proof we could create the
                    // path; the imminent `rename(active -> here)`
                    // will replace this empty placeholder.
                    Ok(true)
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                Err(e) => Err(e),
            }
        }

        let primary = rotated_name(active, &stamp);
        if try_claim(&primary).await? {
            return Ok(primary);
        }
        for n in 1u32..MAX_ROTATION_ATTEMPTS {
            let candidate = rotated_name(active, &format!("{stamp}-{n}"));
            if try_claim(&candidate).await? {
                return Ok(candidate);
            }
        }
        // Last resort: nanosecond stamp. Still bounded — if even
        // this fails we bail with a real error rather than
        // overwrite.
        let nano = Utc::now().format("%Y%m%dT%H%M%S%9fZ").to_string();
        let candidate = rotated_name(active, &nano);
        if try_claim(&candidate).await? {
            return Ok(candidate);
        }
        Err(SinkError::Path(format!(
            "audit log rotation suffix exhausted after {MAX_ROTATION_ATTEMPTS} attempts; \
             stamp={stamp}, active={}",
            active.display()
        )))
    }
}

/// Cap on how many `.<stamp>-<n>` suffixes we'll probe before bailing
/// out. 1024 is generous — a process would have to perform that many
/// rotations inside one millisecond to reach it.
const MAX_ROTATION_ATTEMPTS: u32 = 1024;

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
            let file = state
                .file
                .as_mut()
                .ok_or_else(|| SinkError::Path("file handle missing post-rotation".into()))?;
            file.write_all(line.as_bytes()).await?;
            file.write_all(b"\n").await?;
            state.bytes += line_bytes;
            if self.cfg.fsync_each_write {
                if let Some(file) = state.file.as_mut() {
                    file.sync_data().await?;
                }
            }
            Ok(())
        })
    }

    fn flush<'a>(
        &'a self,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), SinkError>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            if let Some(file) = state.file.as_mut() {
                file.flush().await?;
                file.sync_data().await?;
            }
            Ok(())
        })
    }
}

/// Resolve strftime tokens in `template` against the current UTC time
/// and return the materialised path.
///
/// Validates the template up front by parsing it through
/// [`chrono::format::StrftimeItems`]. If the template contains any
/// invalid strftime token (e.g. `%K`), returns [`SinkError::Path`]
/// instead of letting chrono panic inside `DelayedFormat::to_string`.
fn resolve_path(template: &Path) -> Result<PathBuf, SinkError> {
    let raw = template
        .to_str()
        .ok_or_else(|| SinkError::Path(format!("non-UTF8 path: {}", template.display())))?;
    let items: Vec<Item<'_>> = StrftimeItems::new(raw).collect();
    if items.iter().any(|i| matches!(i, Item::Error)) {
        return Err(SinkError::Path(format!(
            "invalid strftime template in audit path: {raw:?}"
        )));
    }
    let resolved = Utc::now().format_with_items(items.into_iter()).to_string();
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

    #[test]
    fn resolve_path_rejects_invalid_strftime_token() {
        let path = PathBuf::from("audit-%K-full.jsonl");
        let result = resolve_path(&path);
        assert!(
            result.is_err(),
            "expected Err for invalid token %K, got {result:?}"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("invalid strftime"),
            "error message should mention invalid strftime: {msg}"
        );
    }

    #[test]
    fn resolve_path_accepts_literal_percent() {
        // chrono treats %% as a literal % character
        let path = PathBuf::from("audit-100%%-full.jsonl");
        let result = resolve_path(&path);
        assert!(result.is_ok(), "expected Ok for %% literal, got {result:?}");
        let resolved = result.unwrap();
        let name = resolved.file_name().unwrap().to_str().unwrap();
        assert_eq!(name, "audit-100%-full.jsonl");
    }

    #[test]
    fn resolve_path_substitutes_valid_tokens() {
        let path = PathBuf::from("audit-%Y-%m-%d.jsonl");
        let result = resolve_path(&path);
        assert!(
            result.is_ok(),
            "expected Ok for valid tokens, got {result:?}"
        );
        let resolved = result.unwrap();
        let name = resolved.file_name().unwrap().to_str().unwrap();
        assert!(
            !name.contains("%Y") && !name.contains("%m") && !name.contains("%d"),
            "tokens should have been substituted: {name}"
        );
        assert!(!name.is_empty(), "resolved filename should not be empty");
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
