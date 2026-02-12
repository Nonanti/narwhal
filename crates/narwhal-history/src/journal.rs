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
    //
    // H9: pattern list expanded from the original 4 to cover:
    //  - PostgreSQL `ENCRYPTED PASSWORD`, `WITH PASSWORD`
    //  - Dollar-quoted function bodies (`$$…$$`, `$tag$…$tag$`) which
    //    frequently wrap credentials inside `CREATE FUNCTION` blocks.
    //  - Connection-string DSNs (`postgres://user:pw@host/db`, MySQL,
    //    ClickHouse, Redis, MongoDB) where the userinfo segment leaks
    //    the password verbatim if logged in an error message.
    //  - AWS-style key literals (`ACCESS_KEY_ID`, `SECRET_ACCESS_KEY`)
    //    and generic `TOKEN`/`AUTHORIZATION` keywords used by Snowflake,
    //    BigQuery, S3 `COPY`, etc.
    //  - `password=…` kv pairs (JDBC / connection-property syntax).
    vec![
        // CREATE/ALTER USER … [ENCRYPTED] PASSWORD '…'
        Regex::new(r"(?i)(\b(?:encrypted\s+)?password\s+)'(?:[^']|'')*'").unwrap(),
        // … WITH PASSWORD '…'
        Regex::new(r"(?i)(\bwith\s+password\s+)'(?:[^']|'')*'").unwrap(),
        // CREATE USER … IDENTIFIED [WITH …] BY '…'
        Regex::new(r"(?i)(\bidentified\s+(?:with\s+\S+\s+)?by\s+)'(?:[^']|'')*'").unwrap(),
        // COPY … CREDENTIALS '…'
        Regex::new(r"(?i)(\bcredentials\s+)'(?:[^']|'')*'").unwrap(),
        // SET PASSWORD = '…' / SET password = '…'
        Regex::new(r"(?i)(\bset\s+password\s*=\s+)'(?:[^']|'')*'").unwrap(),
        // AWS-style key literals (used by Redshift / Snowflake / BigQuery COPY).
        Regex::new(r"(?i)(\baccess_key_id\s*=?\s*)'(?:[^']|'')*'").unwrap(),
        Regex::new(r"(?i)(\bsecret_access_key\s*=?\s*)'(?:[^']|'')*'").unwrap(),
        Regex::new(r"(?i)(\baws_secret_access_key\s*=?\s*)'(?:[^']|'')*'").unwrap(),
        // Bearer tokens / OAuth client secrets / API keys.
        Regex::new(r"(?i)(\btoken\s+)'(?:[^']|'')*'").unwrap(),
        Regex::new(r"(?i)(\bauthorization\s+)'(?:[^']|'')*'").unwrap(),
        Regex::new(r"(?i)(\boauth_client_secret\s*=?\s*)'(?:[^']|'')*'").unwrap(),
        Regex::new(r"(?i)(\bapi_key\s*=?\s*)'(?:[^']|'')*'").unwrap(),
        Regex::new(r"(?i)(\bprivate_key\s*=?\s*)'(?:[^']|'')*'").unwrap(),
        // JDBC / connection-property kv pairs: `password=…` until
        // whitespace, `;`, `&`, or end-of-input. Used by Spark, Trino,
        // ODBC connection strings, etc.
        Regex::new(r#"(?i)(\bpassword\s*=\s*)[^\s;&'"]+"#).unwrap(),
        // (Dollar-quoted PG function bodies are handled by a separate
        // hand-rolled pass below — the `regex` crate does not support
        // the backreference needed to pair the opening/closing tag.)

        // DSN userinfo: `scheme://user:password@host`. Replacement
        // collapses the entire userinfo segment (`user:***@`). We list
        // the schemes we ship drivers for plus a couple of common ones
        // that show up in errors.
        Regex::new(
            r"(?i)\b(postgres(?:ql)?|mysql|clickhouse|redis|mongodb(?:\+srv)?|jdbc:[a-z0-9]+)://([^:@/\s]+):([^@/\s]+)@",
        )
        .unwrap(),
    ]
});

/// Bespoke replacement string per pattern.
///
/// Most patterns capture the keyword in group 1 and want the literal
/// replaced with `'***'`. A few (DSN userinfo, JDBC kv pairs) carry a
/// different shape and need their own template. The index here mirrors
/// the order of [`REDACT_PATTERNS`].
const fn redact_replacement(idx: usize) -> &'static str {
    match idx {
        // JDBC kv pair (`password=foo` — no quotes). Index must match
        // the position of the corresponding regex in REDACT_PATTERNS.
        13 => "$1***",
        // DSN userinfo — keep scheme + user, mask password.
        14 => "$1://$2:***@",
        // Every other pattern: replace the captured quoted literal.
        _ => "${1}'***'",
    }
}

/// Replace the body of every dollar-quoted PG block (`$$ … $$`,
/// `$tag$ … $tag$`) with `***`, preserving both fences. Implemented
/// by hand because the `regex` crate does not support backreferences
/// and pulling in `fancy-regex` for one pattern is overkill.
fn redact_dollar_quoted(sql: &str) -> Cow<'_, str> {
    let bytes = sql.as_bytes();
    let mut out: Option<String> = None;
    let mut i = 0usize;
    let mut last_copied = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        // Try to read a tag: `$` + identifier chars + `$`.
        let tag_start = i + 1;
        let mut j = tag_start;
        while j < bytes.len() {
            let b = bytes[j];
            if b.is_ascii_alphanumeric() || b == b'_' {
                j += 1;
            } else {
                break;
            }
        }
        if j >= bytes.len() || bytes[j] != b'$' {
            // Not a dollar-quote opener.
            i += 1;
            continue;
        }
        let tag = &bytes[tag_start..j];
        let body_start = j + 1;
        // Search for the matching closer.
        let closer_len = 2 + tag.len();
        let Some(rel_close) = find_subsequence(&bytes[body_start..], tag, closer_len) else {
            // Unterminated — stop scanning to avoid quadratic walks.
            break;
        };
        let close_start = body_start + rel_close;
        let close_end = close_start + closer_len;
        // Emit pending prefix + opener.
        let buf = out.get_or_insert_with(|| String::with_capacity(sql.len()));
        buf.push_str(&sql[last_copied..body_start]);
        buf.push_str("***");
        buf.push_str(&sql[close_start..close_end]);
        last_copied = close_end;
        i = close_end;
    }
    match out {
        Some(mut s) => {
            s.push_str(&sql[last_copied..]);
            Cow::Owned(s)
        }
        None => Cow::Borrowed(sql),
    }
}

/// Find the byte offset of `$<tag>$` (a closing dollar-quote) within
/// `haystack`, where `tag` is the bare identifier between the fences.
/// `closer_len` = `tag.len() + 2` (the two `$` bytes).
fn find_subsequence(haystack: &[u8], tag: &[u8], closer_len: usize) -> Option<usize> {
    if haystack.len() < closer_len {
        return None;
    }
    let limit = haystack.len() - closer_len + 1;
    let mut k = 0usize;
    while k < limit {
        if haystack[k] == b'$'
            && haystack[k + 1..k + 1 + tag.len()] == *tag
            && haystack[k + 1 + tag.len()] == b'$'
        {
            return Some(k);
        }
        k += 1;
    }
    None
}

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
    // Hand-rolled pass first so subsequent regexes don't see the
    // already-masked body and misfire on `***` content.
    if let Cow::Owned(s) = redact_dollar_quoted(&result) {
        result = Cow::Owned(s);
    }
    for (idx, re) in REDACT_PATTERNS.iter().enumerate() {
        let replacement = redact_replacement(idx);
        match re.replace_all(&result, replacement) {
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
    // H9: error is now a borrowed `Option<&str>` so the cold-path can
    // pass either the original message or a redacted variant without
    // cloning. JSON shape is unchanged (`null` vs. string).
    error: Option<&'a str>,
}

impl<'a> HistoryEntryView<'a> {
    const fn from(entry: &'a HistoryEntry, sql: &'a str, error: Option<&'a str>) -> Self {
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
            error,
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

        // H9: redact both `sql` AND `error` — driver error messages on
        // Postgres / MySQL frequently echo back the offending statement
        // (including the password literal) and were previously persisted
        // verbatim. We rewrite the `error` field in place on the cold
        // path so the hot path (no secrets, no truncation) stays
        // allocation-free.
        let redacted_sql = redact_secrets(&entry.sql);
        let redacted_error: Option<Cow<'_, str>> = entry.error.as_deref().map(redact_secrets);
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

        // Cold path triggers if EITHER sql was rewritten/truncated OR
        // the error field carried a secret.
        let error_changed = matches!(redacted_error, Some(Cow::Owned(_)));
        let mut line = if matches!(final_sql, Cow::Borrowed(_)) && !error_changed {
            serde_json::to_vec(entry)?
        } else {
            // Prefer the redacted error when produced, otherwise pass
            // through the original reference. Both paths borrow — no
            // clones either way.
            let err_ref: Option<&str> = redacted_error.as_deref().or(entry.error.as_deref());
            let view = HistoryEntryView::from(entry, &final_sql, err_ref);
            serde_json::to_vec(&view)?
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

    // ---- H9: expanded redaction coverage ----

    #[test]
    fn redact_encrypted_password() {
        let r = redact_secrets("CREATE USER bob ENCRYPTED PASSWORD 'h4sh'");
        assert_eq!(r, "CREATE USER bob ENCRYPTED PASSWORD '***'");
    }

    #[test]
    fn redact_identified_with_plugin_by() {
        let r = redact_secrets("CREATE USER x IDENTIFIED WITH mysql_native_password BY 'pw'");
        assert!(r.ends_with("'***'"), "got: {r}");
        assert!(!r.contains("pw'"));
    }

    #[test]
    fn redact_with_password() {
        let r = redact_secrets("CREATE ROLE app WITH PASSWORD 'sup3r-secret'");
        assert!(!r.contains("sup3r-secret"), "got: {r}");
    }

    #[test]
    fn redact_aws_access_keys() {
        let r = redact_secrets(
            "COPY t FROM 's3://b/k' ACCESS_KEY_ID 'AKIA123' SECRET_ACCESS_KEY 'wJalrXUt'",
        );
        assert!(!r.contains("AKIA123"), "got: {r}");
        assert!(!r.contains("wJalrXUt"), "got: {r}");
    }

    #[test]
    fn redact_bearer_token_and_authorization() {
        let r = redact_secrets("SET TOKEN 'eyJhbGciOi' AUTHORIZATION 'Bearer xyz'");
        assert!(!r.contains("eyJhbGciOi"), "got: {r}");
        assert!(!r.contains("Bearer xyz"), "got: {r}");
    }

    #[test]
    fn redact_jdbc_password_kv() {
        let r = redact_secrets("jdbc:postgresql://h/db?user=app&password=hunter2&sslmode=require");
        assert!(!r.contains("hunter2"), "got: {r}");
        assert!(r.contains("sslmode=require"), "got: {r}");
    }

    #[test]
    fn redact_dsn_userinfo() {
        let r = redact_secrets("failed to connect: postgres://app:hunter2@db.local:5432/orders");
        assert!(!r.contains("hunter2"), "got: {r}");
        assert!(r.contains("postgres://app:***@"), "got: {r}");
    }

    #[test]
    fn redact_mysql_and_clickhouse_dsn() {
        let r1 = redact_secrets("mysql://root:toor@db:3306/x");
        assert!(!r1.contains("toor"), "got: {r1}");
        let r2 = redact_secrets("clickhouse://user:p4ss@db:8123/d");
        assert!(!r2.contains("p4ss"), "got: {r2}");
    }

    #[test]
    fn redact_dollar_quoted_function_body() {
        let sql = "CREATE FUNCTION f() RETURNS void AS $$ SELECT 'leaked' $$ LANGUAGE sql";
        let r = redact_secrets(sql);
        assert!(!r.contains("leaked"), "got: {r}");
        assert!(r.contains("$$***$$"), "got: {r}");
    }

    #[test]
    fn redact_tagged_dollar_quoted_body() {
        let sql = "AS $body$ PASSWORD 's3cret' $body$";
        let r = redact_secrets(sql);
        assert!(!r.contains("s3cret"), "got: {r}");
        assert!(r.contains("$body$***$body$"), "got: {r}");
    }

    #[test]
    fn unterminated_dollar_quote_does_not_panic() {
        // Defensive: malformed input must not loop or panic.
        let sql = "weird $tag$ never closed";
        let r = redact_secrets(sql);
        // Unterminated bodies are left as-is (we can't safely mask them).
        assert_eq!(r, sql);
    }

    #[tokio::test]
    async fn error_field_is_redacted_on_write() {
        // H9: drivers (PG, MySQL) echo offending SQL into error
        // messages. Make sure that text is redacted on disk too.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("history.jsonl");
        let journal = Journal::open(&path).await.unwrap();
        let entry = HistoryEntry::success("SELECT 1")
            .with_failure("syntax error at or near \"CREATE USER x PASSWORD 's3cret'\"");
        journal.append(&entry).await.unwrap();
        drop(journal);

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("s3cret"), "raw secret leaked: {body}");
        assert!(body.contains("PASSWORD '***'"), "got: {body}");
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
