//! Regression tests for H7 — history JSONL secret redaction + file mode 0o600.
//!
//! History entries are written as cleartext JSONL. Without redaction, SQL
//! statements like `CREATE USER x PASSWORD 'secret'` leak the password into
//! the history file. The journal now redacts known secret patterns before
//! writing, and on Unix the file is created with mode 0o600 (owner-only
//! read/write) to prevent other users from reading the history.

use std::fs;

use narwhal_history::{HistoryEntry, Journal};

/// Helper: create a temp journal, append an entry, return the path.
async fn journal_with_entry(entry: &HistoryEntry) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("history.jsonl");
    let journal = Journal::open(&path).await.unwrap();
    journal.append(entry).await.unwrap();
    drop(journal);
    tmp
}

/// Helper: read the raw content of the journal file.
fn read_journal_raw(tmp: &tempfile::TempDir) -> String {
    let path = tmp.path().join("history.jsonl");
    fs::read_to_string(path).unwrap()
}

#[tokio::test]
async fn redacts_create_user_password() {
    let entry = HistoryEntry::success("CREATE USER alice PASSWORD 's3cret'");
    let tmp = journal_with_entry(&entry).await;
    let raw = read_journal_raw(&tmp);
    assert!(!raw.contains("s3cret"), "password leaked in journal: {raw}");
    assert!(raw.contains("***"), "redaction marker not found: {raw}");
}

#[tokio::test]
async fn redacts_alter_user_password() {
    let entry = HistoryEntry::success("ALTER USER bob WITH PASSWORD 'hunter2'");
    let tmp = journal_with_entry(&entry).await;
    let raw = read_journal_raw(&tmp);
    assert!(
        !raw.contains("hunter2"),
        "password leaked in journal: {raw}"
    );
}

#[tokio::test]
async fn redacts_identified_by() {
    let entry = HistoryEntry::success("CREATE USER carol IDENTIFIED BY 'p4ss'");
    let tmp = journal_with_entry(&entry).await;
    let raw = read_journal_raw(&tmp);
    assert!(!raw.contains("p4ss"), "password leaked in journal: {raw}");
}

#[tokio::test]
async fn redacts_credentials() {
    let entry = HistoryEntry::success("COPY t TO 'file' CREDENTIALS 'ak:sk'");
    let tmp = journal_with_entry(&entry).await;
    let raw = read_journal_raw(&tmp);
    assert!(
        !raw.contains("ak:sk"),
        "credentials leaked in journal: {raw}"
    );
}

#[tokio::test]
async fn redacts_set_password() {
    let entry = HistoryEntry::success("SET PASSWORD = 'mysecret'");
    let tmp = journal_with_entry(&entry).await;
    let raw = read_journal_raw(&tmp);
    assert!(
        !raw.contains("mysecret"),
        "password leaked in journal: {raw}"
    );
}

#[tokio::test]
async fn redacts_password_with_equals() {
    // e.g. connection strings or config: password=secret
    let entry = HistoryEntry::success("CREATE SERVER srv OPTIONS (password 'pw123')");
    let tmp = journal_with_entry(&entry).await;
    let raw = read_journal_raw(&tmp);
    assert!(!raw.contains("pw123"), "password leaked in journal: {raw}");
}

#[tokio::test]
async fn does_not_redact_arbitrary_string() {
    // A normal query should not be modified.
    let sql = "SELECT * FROM users WHERE name = 'alice'";
    let entry = HistoryEntry::success(sql);
    let tmp = journal_with_entry(&entry).await;
    let raw = read_journal_raw(&tmp);
    assert!(
        raw.contains("alice"),
        "non-secret string was wrongly redacted: {raw}"
    );
    assert!(
        !raw.contains("***"),
        "redaction applied to non-secret: {raw}"
    );
}

#[tokio::test]
async fn does_not_redact_password_in_column_name() {
    // `password_hash` is a column name, not a secret literal.
    let sql = "SELECT password_hash FROM users";
    let entry = HistoryEntry::success(sql);
    let tmp = journal_with_entry(&entry).await;
    let raw = read_journal_raw(&tmp);
    assert!(
        raw.contains("password_hash"),
        "column name was wrongly redacted: {raw}"
    );
}

#[tokio::test]
async fn redacts_case_insensitive() {
    let entry = HistoryEntry::success("CREATE USER dave PASSWORD 'Secret'");
    let tmp = journal_with_entry(&entry).await;
    let raw = read_journal_raw(&tmp);
    assert!(
        !raw.contains("Secret"),
        "case-insensitive password leaked: {raw}"
    );
}

#[tokio::test]
async fn redacts_multiple_secrets_in_one_statement() {
    let sql = "ALTER USER eve IDENTIFIED BY 'first'; ALTER USER eve PASSWORD 'second'";
    let entry = HistoryEntry::success(sql);
    let tmp = journal_with_entry(&entry).await;
    let raw = read_journal_raw(&tmp);
    assert!(!raw.contains("first"), "first password leaked: {raw}");
    assert!(!raw.contains("second"), "second password leaked: {raw}");
}

#[cfg(unix)]
#[tokio::test]
async fn history_file_mode_is_0600() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("history.jsonl");
    let journal = Journal::open(&path).await.unwrap();
    journal
        .append(&HistoryEntry::success("SELECT 1"))
        .await
        .unwrap();
    drop(journal);

    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "history file mode is {mode:o}, expected 0o600");
}
