use std::borrow::Cow;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use uuid::Uuid;

/// Statically-compiled regex patterns that match secret literals in SQL.
///
/// Each pattern captures the keyword prefix (group 1) and the quoted
/// secret value (group 2) so the replacement preserves the keyword and
/// only masks the secret. Patterns are compiled once at first use via
/// `once_cell::sync::Lazy` to avoid per-call compilation cost.
///
/// **Note:** Only *newly written* entries are redacted. Existing history
/// files with cleartext secrets are **not** automatically retrofitted —
/// users should delete or manually redact old files if they contain
/// sensitive data.
static REDACT_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    // Inner literal alternation `(?:[^']|'')*` matches the SQL standard
    // doubled-single-quote escape so passwords containing `'` aren't
    // cut short mid-string (which would leak the tail). Tested below.
    vec![
        // CREATE/ALTER USER ... PASSWORD '...'
        Regex::new(r"(?i)(\bpassword\s+)'(?:[^']|'')*'").unwrap(),
        // CREATE USER ... IDENTIFIED BY '...'
        Regex::new(r"(?i)(\bidentified\s+by\s+)'(?:[^']|'')*'").unwrap(),
        // COPY ... CREDENTIALS '...'
        Regex::new(r"(?i)(\bcredentials\s+)'(?:[^']|'')*'").unwrap(),
        // SET PASSWORD = '...'
        Regex::new(r"(?i)(\bset\s+password\s*=\s+)'(?:[^']|'')*'").unwrap(),
    ]
});

/// Redact known secret patterns from a SQL string.
///
/// Returns `Cow::Borrowed` when no patterns match (avoiding allocation),
/// or `Cow::Owned` with all secret values replaced by `'***'`.
///
/// `Regex::replace_all` already returns `Cow::Borrowed` when there's no
/// match, so chaining `replace_all` directly avoids the double scan a
/// separate `is_match` would do — the regex engine only walks the
/// string once per pattern in the common (no-secret) path.
fn redact_secrets(sql: &str) -> Cow<'_, str> {
    let mut result: Cow<'_, str> = Cow::Borrowed(sql);
    for re in REDACT_PATTERNS.iter() {
        match re.replace_all(&result, "${1}'***'") {
            // No replacement — keep the existing Cow (borrowed or owned).
            Cow::Borrowed(_) => {}
            Cow::Owned(s) => result = Cow::Owned(s),
        }
    }
    result
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HistoryError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialisation: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Outcome of a recorded statement execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Outcome {
    Success,
    Cancelled,
    Failed,
}

/// Borrowed serialisation view of [`HistoryEntry`] with an overridden
/// `sql` field. Used by [`Journal::append`] to write a redacted /
/// truncated entry without cloning the original — every other field
/// is passed by reference and serde renders them in place.
///
/// The struct field order must mirror [`HistoryEntry`] so the JSON
/// output is byte-for-byte compatible with the legacy clone-based
/// path.
#[derive(Serialize)]
struct HistoryEntryView<'a> {
    timestamp: &'a DateTime<Utc>,
    connection_id: &'a Option<Uuid>,
    connection_name: &'a Option<String>,
    driver: &'a Option<String>,
    sql: &'a str,
    elapsed_ms: u64,
    rows_affected: &'a Option<u64>,
    rows_returned: &'a Option<u64>,
    outcome: &'a Outcome,
    error: &'a Option<String>,
}

impl<'a> HistoryEntryView<'a> {
    const fn from(entry: &'a HistoryEntry, sql: &'a str) -> Self {
        Self {
            timestamp: &entry.timestamp,
            connection_id: &entry.connection_id,
            connection_name: &entry.connection_name,
            driver: &entry.driver,
            sql,
            elapsed_ms: entry.elapsed_ms,
            rows_affected: &entry.rows_affected,
            rows_returned: &entry.rows_returned,
            outcome: &entry.outcome,
            error: &entry.error,
        }
    }
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
    /// Who produced this entry. Free-form tag — `"tui"` (default for
    /// existing on-disk entries via `#[serde(default)]`) or `"mcp"` for
    /// statements that came through the MCP server. Future runtimes
    /// (CLI `-e`, plugin, …) tag themselves the same way.
    #[serde(default)]
    pub source: Option<String>,
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
            source: None,
        }
    }

    /// Tag the entry with a free-form source identifier.
    ///
    /// Used by the MCP server to mark statements as `"mcp"` so an
    /// auditor can separate agent-issued traffic from interactive TUI
    /// usage.
    #[must_use]
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
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
    pub const fn with_timing(mut self, elapsed_ms: u64) -> Self {
        self.elapsed_ms = elapsed_ms;
        self
    }

    #[must_use]
    pub const fn with_rows_affected(mut self, count: u64) -> Self {
        self.rows_affected = Some(count);
        self
    }

    #[must_use]
    pub const fn with_rows_returned(mut self, count: u64) -> Self {
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
    pub const fn with_cancellation(mut self) -> Self {
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

        #[cfg(unix)]
        let file = {
            OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(&path)
                .await?
        };

        #[cfg(not(unix))]
        let file = {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await?
        };

        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Serialise `entry` to a single line and flush to disk.
    ///
    /// Secret patterns in the `sql` field (e.g. `PASSWORD '...'`,
    /// `IDENTIFIED BY '...'`) are automatically redacted to `'***'`
    /// before writing. Only *newly appended* entries are redacted;
    /// pre-existing entries in the history file are left untouched.
    pub async fn append(&self, entry: &HistoryEntry) -> Result<(), HistoryError> {
        // Path A (hot): no secrets and no truncation. Serialize the
        // borrowed entry as-is — zero allocation beyond the final JSON
        // line buffer.
        //
        // Path B (cold): redaction or truncation applied. Build a
        // borrowed view (`HistoryEntryView`) with a substituted `sql`
        // field so we never clone the entire `HistoryEntry` (the
        // timestamp/uuid/string fields used to be cloned twice).
        const SQL_MAX_BYTES: usize = 64 * 1024;

        let redacted_sql = redact_secrets(&entry.sql);
        // Apply truncation on top of the redaction result. Both are
        // expressed as a single owned `String` when either fires; we
        // pay one allocation, not two.
        let final_sql: Cow<'_, str> = if redacted_sql.len() > SQL_MAX_BYTES {
            let mut owned = redacted_sql.into_owned();
            let mut end = SQL_MAX_BYTES;
            while end > 0 && !owned.is_char_boundary(end) {
                end -= 1;
            }
            let dropped = owned.len() - end;
            owned.truncate(end);
            // Avoid the intermediate `format!` allocation — push the
            // marker pieces straight onto the existing buffer.
            owned.push_str("… (truncated ");
            owned.push_str(&dropped.to_string());
            owned.push_str(" bytes)");
            Cow::Owned(owned)
        } else {
            redacted_sql
        };

        let mut line = if matches!(final_sql, Cow::Borrowed(_)) {
            serde_json::to_vec(entry)?
        } else {
            serde_json::to_vec(&HistoryEntryView::from(entry, &final_sql))?
        };
        line.push(b'\n');

        let mut guard = self.file.lock().await;
        guard.write_all(&line).await?;
        guard.flush().await?;
        Ok(())
    }

    /// Return up to `n` most-recent entries in chronological order
    /// (oldest entry first, newest last).
    ///
    /// Uses `rev_lines` to read from the end of the file (avoiding a
    /// full-file parse) and `spawn_blocking` to keep the async runtime
    /// responsive. Corrupt lines are logged with `tracing::warn` rather
    /// than silently swallowed.
    pub async fn recent(&self, n: usize) -> Result<Vec<HistoryEntry>, HistoryError> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let file = File::open(&path)?;
            let reader = BufReader::new(file);
            let mut rev = rev_lines::RevLines::new(reader);
            let mut out = Vec::with_capacity(n);
            for line in rev.by_ref() {
                if out.len() >= n {
                    break;
                }
                let line = line.map_err(|e| std::io::Error::other(e.to_string()))?;
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<HistoryEntry>(trimmed) {
                    Ok(e) => out.push(e),
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            line = %trimmed,
                            "journal parse failed"
                        );
                    }
                }
            }
            // rev_lines reads newest-first; reverse to get chronological
            // order (oldest of the batch first, newest last).
            out.reverse();
            Ok(out)
        })
        .await
        .map_err(|e| HistoryError::Io(std::io::Error::other(e.to_string())))?
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
    async fn recent_returns_last_n_in_chronological_order() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("history.jsonl");
        let journal = Journal::open(&path).await.unwrap();
        for i in 0..5 {
            journal
                .append(&HistoryEntry::success(format!("SELECT {i}")))
                .await
                .unwrap();
        }

        let recent = journal.recent(3).await.unwrap();
        assert_eq!(recent.len(), 3);
        // Chronological: oldest of the batch first
        assert_eq!(recent[0].sql, "SELECT 2");
        assert_eq!(recent[1].sql, "SELECT 3");
        assert_eq!(recent[2].sql, "SELECT 4");
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

        let recent = journal.recent(200).await.unwrap();
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

    // ---- redact_secrets unit tests ----

    #[test]
    fn redact_password_literal() {
        assert_eq!(
            redact_secrets("CREATE USER x PASSWORD 'secret'"),
            "CREATE USER x PASSWORD '***'"
        );
    }

    #[test]
    fn redact_identified_by() {
        assert_eq!(
            redact_secrets("CREATE USER x IDENTIFIED BY 'pw'"),
            "CREATE USER x IDENTIFIED BY '***'"
        );
    }

    #[test]
    fn redact_no_match_returns_borrowed() {
        let sql = "SELECT 1";
        let result = redact_secrets(sql);
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    /// Round 1 bugfix: the old pattern `'[^']*'` matched only up to
    /// the first `'`, so a password containing the standard SQL
    /// double-single-quote escape would be cut and its tail leaked
    /// into the journal.
    #[test]
    fn redact_password_with_escaped_single_quote() {
        let redacted = redact_secrets("ALTER USER x PASSWORD 'it''s s3cret'");
        assert_eq!(redacted, "ALTER USER x PASSWORD '***'");
        // The leaky tail must not survive anywhere in the output.
        assert!(!redacted.contains("s3cret"));
    }

    /// M13: `recent` warns on corrupt lines rather than silently swallowing.
    #[tokio::test]
    async fn recent_warns_on_corrupt_line() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("history.jsonl");

        // Write one valid and one corrupt entry directly to the file.
        let valid = HistoryEntry::success("SELECT 1");
        let mut line = serde_json::to_vec(&valid).unwrap();
        line.push(b'\n');
        std::fs::write(&path, &line).unwrap();

        // Append a corrupt line
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"THIS IS NOT JSON\n").unwrap();
        f.write_all(b"\n").unwrap(); // blank line
        drop(f);

        let journal = Journal::open(&path).await.unwrap();
        let recent = journal.recent(10).await.unwrap();
        // Only the valid entry should be returned; the corrupt line is
        // logged as a warning and skipped.
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].sql, "SELECT 1");
    }
}
