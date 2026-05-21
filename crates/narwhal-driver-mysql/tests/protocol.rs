//! Regression tests for `uses_text_protocol` (bug H4).
//!
//! Without this guard `execute` chose the protocol on `params.is_empty()`
//! alone: any parameterless SELECT/INSERT/UPDATE went through the *text*
//! protocol, where MySQL returns every column as `MyValue::Bytes`. After
//! UTF-8 decoding the application then saw 'SELECT 1' as
//! 'Value::String("1")' rather than 'Value::Int(1)' — sort order and
//! aggregation broke for any non-string column whenever the caller
//! happened to omit parameters.
//!
//! The fix narrows the text-protocol branch to a small whitelist of
//! statements that MySQL refuses to prepare (transaction control,
//! session state, catalogue introspection, lock management, bulk load).
//! Everything else uses the binary prepared-statement protocol — with
//! `Params::Empty` for parameterless calls — so column types travel
//! intact.

use narwhal_driver_mysql::__test_only::uses_text_protocol;

// ----- Statements that MUST stay on the text protocol -----

#[test]
fn savepoint_uses_text_protocol() {
    assert!(uses_text_protocol("SAVEPOINT sp1"));
}

#[test]
fn release_savepoint_uses_text_protocol() {
    assert!(uses_text_protocol("RELEASE SAVEPOINT sp1"));
}

#[test]
fn rollback_to_savepoint_uses_text_protocol() {
    assert!(uses_text_protocol("ROLLBACK TO SAVEPOINT sp1"));
}

#[test]
fn start_transaction_uses_text_protocol() {
    assert!(uses_text_protocol("START TRANSACTION"));
}

#[test]
fn begin_uses_text_protocol() {
    assert!(uses_text_protocol("BEGIN"));
}

#[test]
fn begin_work_uses_text_protocol() {
    assert!(uses_text_protocol("BEGIN WORK"));
}

#[test]
fn commit_uses_text_protocol() {
    assert!(uses_text_protocol("COMMIT"));
}

#[test]
fn rollback_uses_text_protocol() {
    assert!(uses_text_protocol("ROLLBACK"));
}

#[test]
fn use_uses_text_protocol() {
    assert!(uses_text_protocol("USE my_db"));
}

#[test]
fn set_uses_text_protocol() {
    assert!(uses_text_protocol("SET TRANSACTION ISOLATION LEVEL READ COMMITTED"));
}

#[test]
fn set_session_uses_text_protocol() {
    assert!(uses_text_protocol("SET SESSION sql_mode = ''"));
}

#[test]
fn show_uses_text_protocol() {
    assert!(uses_text_protocol("SHOW TABLES"));
}

#[test]
fn describe_uses_text_protocol() {
    assert!(uses_text_protocol("DESCRIBE my_table"));
}

#[test]
fn lock_tables_uses_text_protocol() {
    assert!(uses_text_protocol("LOCK TABLES t WRITE"));
}

#[test]
fn unlock_tables_uses_text_protocol() {
    assert!(uses_text_protocol("UNLOCK TABLES"));
}

#[test]
fn case_insensitive_whitelist() {
    assert!(uses_text_protocol("savepoint sp1"));
    assert!(uses_text_protocol("Begin Work"));
    assert!(uses_text_protocol("show variables"));
}

#[test]
fn leading_whitespace_does_not_confuse_lookup() {
    assert!(uses_text_protocol("   \n\t COMMIT"));
}

#[test]
fn leading_block_comment_is_skipped() {
    assert!(uses_text_protocol("/* tag */ BEGIN"));
}

#[test]
fn leading_line_comment_is_skipped() {
    assert!(uses_text_protocol("-- explain\nCOMMIT"));
}

// ----- Statements that MUST go through the binary protocol -----

#[test]
fn select_uses_binary_protocol() {
    assert!(!uses_text_protocol("SELECT 1"));
}

#[test]
fn select_with_columns_uses_binary_protocol() {
    assert!(!uses_text_protocol("SELECT id, name FROM users"));
}

#[test]
fn insert_uses_binary_protocol() {
    assert!(!uses_text_protocol("INSERT INTO t VALUES (1)"));
}

#[test]
fn update_uses_binary_protocol() {
    assert!(!uses_text_protocol("UPDATE t SET x = 1"));
}

#[test]
fn delete_uses_binary_protocol() {
    assert!(!uses_text_protocol("DELETE FROM t WHERE id = 1"));
}

#[test]
fn create_table_uses_binary_protocol() {
    assert!(!uses_text_protocol("CREATE TABLE t (id INT)"));
}

#[test]
fn alter_table_uses_binary_protocol() {
    assert!(!uses_text_protocol("ALTER TABLE t ADD COLUMN c INT"));
}

#[test]
fn drop_table_uses_binary_protocol() {
    assert!(!uses_text_protocol("DROP TABLE t"));
}

#[test]
fn empty_input_falls_back_to_binary() {
    // No leading keyword to whitelist; binary path is the safer default
    // and will surface the protocol error from the server.
    assert!(!uses_text_protocol(""));
    assert!(!uses_text_protocol("   \n"));
}

#[test]
fn select_after_comment_uses_binary_protocol() {
    assert!(!uses_text_protocol("/* hint */ SELECT 1"));
    assert!(!uses_text_protocol("-- hint\nSELECT 1"));
}
