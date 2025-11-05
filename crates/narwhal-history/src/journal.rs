use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialisation: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Outcome of a recorded statement execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Cancelled,
    Failed,
}

/// One record in the history journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub timestamp: DateTime<Utc>,
    pub connection_id: Option<Uuid>,
    pub connection_name: Option<String>,
    pub driver: Option<String>,
    pub sql: String,
    pub elapsed_ms: u64,
    pub rows_affected: Option<u64>,
    pub rows_returned: Option<u64>,
    pub outcome: Outcome,
    pub error: Option<String>,
}

impl HistoryEntry {
    pub fn success(sql: impl Into<String>) -> Self {
        Self {
            timestamp: Utc::now(),
            connection_id: None,
            connection_name: None,
            driver: None,
            sql: sql.into(),
            elapsed_ms: 0,
            rows_affected: None,
            rows_returned: None,
            outcome: Outcome::Success,
            error: None,
        }
    }

    #[must_use]
    pub fn with_connection(mut self, id: Uuid, name: impl Into<String>) -> Self {
        self.connection_id = Some(id);
        self.connection_name = Some(name.into());
        self
    }

    #[must_use]
    pub fn with_driver(mut self, driver: impl Into<String>) -> Self {
        self.driver = Some(driver.into());
        self
    }

    #[must_use]
    pub fn with_timing(mut self, elapsed_ms: u64) -> Self {
        self.elapsed_ms = elapsed_ms;
        self
    }

    #[must_use]
    pub fn with_rows_affected(mut self, count: u64) -> Self {
        self.rows_affected = Some(count);
        self
    }

    #[must_use]
    pub fn with_rows_returned(mut self, count: u64) -> Self {
        self.rows_returned = Some(count);
        self
    }

    #[must_use]
    pub fn with_failure(mut self, message: impl Into<String>) -> Self {
        self.outcome = Outcome::Failed;
        self.error = Some(message.into());
        self
    }

    #[must_use]
    pub fn with_cancellation(mut self) -> Self {
        self.outcome = Outcome::Cancelled;
        self
    }
}

/// Append-only writer for [`HistoryEntry`].
///
/// A single [`Journal`] is intended to be shared between tasks; the internal
/// file handle is protected by a mutex so writes interleave at line
/// boundaries.
pub struct Journal {
    path: PathBuf,
    file: Mutex<tokio::fs::File>,
}

impl Journal {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, HistoryError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Serialise `entry` to a single line and flush to disk.
    pub async fn append(&self, entry: &HistoryEntry) -> Result<(), HistoryError> {
        let mut line = serde_json::to_vec(entry)?;
        line.push(b'\n');
        let mut guard = self.file.lock().await;
        guard.write_all(&line).await?;
        guard.flush().await?;
        Ok(())
    }

    /// Return up to `n` most-recent entries, newest first.
    ///
    /// Reads the JSONL file from disk (synchronous I/O) and returns the
    /// last `n` lines in reverse order so the most recent entry comes
    /// first. Malformed lines are silently skipped.
    pub fn recent(&self, n: usize) -> Result<Vec<HistoryEntry>, HistoryError> {
        let reader = JournalReader::open(&self.path)?;
        let all: Vec<HistoryEntry> = reader.filter_map(|r| r.ok()).collect();
        let start = all.len().saturating_sub(n);
        let mut slice = all[start..].to_vec();
        slice.reverse();
        Ok(slice)
    }
}

/// Synchronous iterator over journal entries. Reading is intentionally
/// blocking because callers typically dump history in a UI thread that is
/// already off the hot path.
pub struct JournalReader {
    reader: BufReader<File>,
}

impl JournalReader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, HistoryError> {
        let file = File::open(path)?;
        Ok(Self {
            reader: BufReader::new(file),
        })
    }
}

impl Iterator for JournalReader {
    type Item = Result<HistoryEntry, HistoryError>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut line = String::new();
        loop {
            line.clear();
            match self.reader.read_line(&mut line) {
                Ok(0) => return None,
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    return Some(serde_json::from_str(trimmed).map_err(Into::into));
                }
                Err(e) => return Some(Err(e.into())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip_single_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("history.jsonl");

        let journal = Journal::open(&path).await.unwrap();
        let entry = HistoryEntry::success("SELECT 1")
            .with_driver("sqlite")
            .with_timing(3)
            .with_rows_returned(1);
        journal.append(&entry).await.unwrap();
        drop(journal);

        let mut reader = JournalReader::open(&path).unwrap();
        let first = reader.next().unwrap().unwrap();
        assert_eq!(first.sql, "SELECT 1");
        assert_eq!(first.driver.as_deref(), Some("sqlite"));
        assert_eq!(first.elapsed_ms, 3);
        assert_eq!(first.rows_returned, Some(1));
        assert!(reader.next().is_none());
    }

    #[tokio::test]
    async fn appends_across_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("history.jsonl");

        {
            let journal = Journal::open(&path).await.unwrap();
            journal
                .append(&HistoryEntry::success("SELECT 1"))
                .await
                .unwrap();
        }
        {
            let journal = Journal::open(&path).await.unwrap();
            journal
                .append(&HistoryEntry::success("SELECT 2"))
                .await
                .unwrap();
        }

        let reader = JournalReader::open(&path).unwrap();
        let lines: Vec<_> = reader.collect::<Result<_, _>>().unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].sql, "SELECT 1");
        assert_eq!(lines[1].sql, "SELECT 2");
    }

    #[tokio::test]
    async fn concurrent_writes_interleave_at_line_boundaries() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("history.jsonl");
        let journal = std::sync::Arc::new(Journal::open(&path).await.unwrap());

        let mut handles = Vec::new();
        for i in 0..16 {
            let j = std::sync::Arc::clone(&journal);
            handles.push(tokio::spawn(async move {
                j.append(&HistoryEntry::success(format!("SELECT {i}")))
                    .await
                    .unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        drop(journal);

        let reader = JournalReader::open(&path).unwrap();
        let entries: Vec<_> = reader.collect::<Result<_, _>>().unwrap();
        assert_eq!(entries.len(), 16);
    }

    #[tokio::test]
    async fn recent_returns_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("history.jsonl");
        let journal = Journal::open(&path).await.unwrap();
        for i in 0..5 {
            journal
                .append(&HistoryEntry::success(format!("SELECT {i}")))
                .await
                .unwrap();
        }

        let recent = journal.recent(3).unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].sql, "SELECT 4");
        assert_eq!(recent[1].sql, "SELECT 3");
        assert_eq!(recent[2].sql, "SELECT 2");
    }

    #[tokio::test]
    async fn recent_clamps_to_available() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("history.jsonl");
        let journal = Journal::open(&path).await.unwrap();
        journal
            .append(&HistoryEntry::success("SELECT 1"))
            .await
            .unwrap();

        let recent = journal.recent(200).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].sql, "SELECT 1");
    }

    #[tokio::test]
    async fn captures_failure_and_cancellation() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("history.jsonl");
        let journal = Journal::open(&path).await.unwrap();

        journal
            .append(&HistoryEntry::success("SELECT 1").with_failure("boom"))
            .await
            .unwrap();
        journal
            .append(&HistoryEntry::success("SELECT 2").with_cancellation())
            .await
            .unwrap();

        let entries: Vec<_> = JournalReader::open(&path)
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(entries[0].outcome, Outcome::Failed);
        assert_eq!(entries[0].error.as_deref(), Some("boom"));
        assert_eq!(entries[1].outcome, Outcome::Cancelled);
    }
}
