//! Read-only SQL guard + lightweight statement classification.
//!
//! Two related responsibilities live here:
//!
//! 1. [`guard_read_only`] — a syntactic allow-list that rejects anything
//!    that is not obviously a read (`SELECT`, `WITH`, `SHOW`, `EXPLAIN`,
//!    `DESCRIBE`, `DESC`, `PRAGMA`, `VALUES`, `TABLE`). Used by the MCP
//!    `run_query` tool as the first of its three safety layers (guard →
//!    `BEGIN`/`ROLLBACK` sandwich → row cap) and by the TUI when a
//!    connection is configured `read_only = true`.
//!
//! 2. [`classify_statement`] — bucket a statement into [`StatementKind`]
//!    so callers can branch on "is this a write?" without re-implementing
//!    keyword scanning. Used by the connection-level write-confirmation
//!    guard (`confirm_writes = true` in `connections.toml`).
//!
//! The implementation is deliberately syntactic, not a parser: we strip
//! leading comments + whitespace, strip the bodies of string literals
//! and double-quoted identifiers, and look at the first significant
//! token. That is enough to keep agents and humans honest while costing
//! the same order of magnitude as a `trim`.
//!
//! Originally lived inside `narwhal-mcp::tools::run_query` — moved here
//! in v1.1 so MCP and TUI share one denylist.

/// Bucket assigned by [`classify_statement`].
///
/// The four variants line up with the policy questions the UI cares about:
///
/// - `Read` — safe under a read-only connection, no confirmation needed.
/// - `Write` — DML (`INSERT`/`UPDATE`/`DELETE`/`MERGE`/`UPSERT`). Triggers
///   the `confirm_writes` modal.
/// - `Ddl` — schema mutations (`CREATE`/`DROP`/`ALTER`/`TRUNCATE`/`GRANT`/…).
///   Always confirmed in `confirm_writes` mode, regardless of severity.
/// - `Tx` — transaction control (`BEGIN`/`COMMIT`/`ROLLBACK`/`SAVEPOINT`).
///   Cheap to run, no confirmation; the UI uses this to update its
///   transaction badge.
/// - `Unknown` — empty input, comment-only input, or a first token we
///   don't recognise. Callers should treat this conservatively (i.e.
///   prompt the user) rather than guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StatementKind {
    Read,
    Write,
    Ddl,
    Tx,
    Unknown,
}

impl StatementKind {
    /// `true` for [`Self::Write`] and [`Self::Ddl`]. The `confirm_writes`
    /// connection flag uses this as the single predicate.
    #[must_use]
    pub const fn is_mutating(self) -> bool {
        matches!(self, Self::Write | Self::Ddl)
    }
}

/// Classify `sql` by its first significant keyword.
///
/// Comments and whitespace are skipped first; string literals and
/// quoted identifiers are *not* stripped because we only look at the
/// leading token, which by construction is not inside a quoted region.
///
/// This is intentionally cheap — O(leading whitespace + first word).
/// It is not a parser; a statement that opens with `WITH … INSERT …`
/// is classified as [`StatementKind::Read`] because the first keyword
/// is `WITH`. Callers that need to be strict about CTE-disguised
/// writes should run [`guard_read_only`] instead, which rejects the
/// statement at the `BEGIN`/`ROLLBACK` sandwich layer in MCP.
#[must_use]
pub fn classify_statement(sql: &str) -> StatementKind {
    let stripped = strip_leading_comments_and_whitespace(sql);
    let first = first_keyword(stripped);
    if first.is_empty() {
        return StatementKind::Unknown;
    }
    match first.as_str() {
        // Reads — also the `guard_read_only` allow-list.
        "SELECT" | "WITH" | "SHOW" | "EXPLAIN" | "DESCRIBE" | "DESC" | "PRAGMA" | "VALUES"
        | "TABLE" => StatementKind::Read,

        // DML — confirmation territory.
        "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "UPSERT" | "REPLACE" | "COPY" | "LOAD" => {
            StatementKind::Write
        }

        // DDL + permission changes — always confirmed.
        "CREATE" | "DROP" | "ALTER" | "TRUNCATE" | "RENAME" | "GRANT" | "REVOKE" | "COMMENT"
        | "VACUUM" | "ANALYZE" | "REINDEX" | "CLUSTER" | "REFRESH" | "ATTACH" | "DETACH"
        | "SET" => StatementKind::Ddl,

        // Transaction control.
        "BEGIN" | "START" | "COMMIT" | "END" | "ROLLBACK" | "SAVEPOINT" | "RELEASE" => {
            StatementKind::Tx
        }

        _ => StatementKind::Unknown,
    }
}

/// Reject `sql` unless its first significant token is on the read-only
/// allow-list **and** the statement does not contain a known
/// connection-holding / CPU-burning function call.
///
/// Returns `Ok(())` for a safe read, or `Err(reason)` with a
/// human-readable explanation suitable for surfacing to the user / agent.
///
/// # Allow-list
///
/// `SELECT`, `WITH`, `SHOW`, `EXPLAIN`, `DESCRIBE`, `DESC`, `PRAGMA`,
/// `VALUES`, `TABLE`.
///
/// # Deny-list (by word-boundary substring after literal stripping)
///
/// `PG_SLEEP`, `PG_SLEEP_FOR`, `PG_SLEEP_UNTIL`, `SLEEP`, `DBMS_LOCK`,
/// `BENCHMARK`, `WAITFOR`.
pub fn guard_read_only(sql: &str) -> Result<(), String> {
    let stripped = strip_leading_comments_and_whitespace(sql);
    let first_token = first_keyword(stripped);

    if first_token.is_empty() {
        return Err("empty statement".into());
    }

    const ALLOWED: &[&str] = &[
        "SELECT",   // ANSI
        "WITH",     // CTE
        "SHOW",     // PG/MySQL/CH metadata
        "EXPLAIN",  // every driver
        "DESCRIBE", // MySQL
        "DESC",     // MySQL shorthand
        "PRAGMA",   // SQLite (read forms only — write pragmas mutate session state)
        "VALUES",   // PG row-constructor SELECT
        "TABLE",    // PG `TABLE foo;` shorthand
    ];

    if !ALLOWED.contains(&first_token.as_str()) {
        return Err(format!(
            "first token `{first_token}` is not in the read-only allow-list \
             (SELECT/WITH/SHOW/EXPLAIN/DESCRIBE/DESC/PRAGMA/VALUES/TABLE)"
        ));
    }

    // Even when the first token is allowed, block known dangerous
    // function calls that can hold a connection or mutate state.
    //
    // Issue D (sprint 5): the previous substring match produced both
    // false positives (`sleeping_bags`, `asleep`, a column named
    // `"SLEEP"`) and false negatives (`pg_sleep_for`, `WAITFOR DELAY`,
    // `BENCHMARK(...)`). The new check
    //
    //   1. Strips string literals and quoted identifiers so a comment
    //      or `'pg_sleep'` cannot trip the guard, and
    //   2. Requires word-boundary matches so `sleeping_bags` is not
    //      mistaken for `SLEEP`.
    //
    // The denylist is also widened to cover the engines we ship
    // drivers for plus the documented dialect quirks (MSSQL
    // `WAITFOR`, MySQL `BENCHMARK` CPU bomb, PG `pg_sleep_for` /
    // `pg_sleep_until`).
    let sanitised = strip_sql_literals(stripped);
    let upper_sql = sanitised.to_ascii_uppercase();
    const BLOCKED_FUNCS: &[&str] = &[
        "PG_SLEEP",       // postgres: holds the connection
        "PG_SLEEP_FOR",   // postgres alt
        "PG_SLEEP_UNTIL", // postgres alt
        "SLEEP",          // mysql / mariadb
        "DBMS_LOCK",      // oracle (`DBMS_LOCK.SLEEP(…)`)
        "BENCHMARK",      // mysql cpu bomb
        "WAITFOR",        // mssql delay
    ];
    for pattern in BLOCKED_FUNCS {
        if contains_word(&upper_sql, pattern) {
            return Err(format!(
                "statement contains blocked function `{pattern}` which can \
                 hold a connection or burn CPU — pass `read_only=false` if intentional"
            ));
        }
    }

    Ok(())
}

/// Extract the leading ASCII-alphabetic keyword from `stripped`,
/// uppercased. Returns an empty string when no keyword is present.
///
/// Shared helper between [`guard_read_only`] and [`classify_statement`]
/// so both agree on what "the first token" means.
fn first_keyword(stripped: &str) -> String {
    stripped
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("")
        .to_ascii_uppercase()
}

/// Replace the contents of every SQL string literal (`'…'`),
/// double-quoted identifier (`"…"`), line comment (`-- …`), and
/// block comment (`/* … */`) with spaces so that subsequent keyword
/// scans see only structural tokens. Handles the SQL standard
/// doubled-quote escapes (`''` inside a single-quoted literal, `""`
/// inside a double-quoted identifier). Backslash escapes are *not*
/// honoured — the goal is keyword stripping, not full lexing.
///
/// Comment masking was added in v2.2.0 (B7) because a trailing
/// `-- benchmark candidate, sleep(1)` or `/* TODO: drop pg_sleep */`
/// was tripping the denylist even though the keywords were not
/// executable code.
///
/// Sprint 11 follow-up: an earlier draft added `MySQL` backticks to
/// the strip set on the assumption that ``SELECT `SLEEP`(10)``
/// bypassed the scanner. The opposite is true — backticks are
/// **not** `is_ident_byte`, so `SLEEP` between them is already
/// flanked by word boundaries and the denylist catches it. Masking
/// the body would *create* the bypass by hiding the function name
/// from the scanner. The regression test
/// `guard_rejects_backtick_identifier_bypass` pins this.
fn strip_sql_literals(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut iter = sql.chars().peekable();
    while let Some(c) = iter.next() {
        match c {
            // Line comment: keep `--` delimiter, mask body until newline.
            '-' if iter.peek() == Some(&'-') => {
                iter.next();
                out.push('-');
                out.push('-');
                while let Some(&next) = iter.peek() {
                    iter.next();
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                    out.push(' ');
                }
            }
            // Block comment: keep `/* */` delimiters, mask body.
            '/' if iter.peek() == Some(&'*') => {
                iter.next();
                out.push('/');
                out.push('*');
                loop {
                    match iter.next() {
                        Some('*') if iter.peek() == Some(&'/') => {
                            iter.next();
                            out.push('*');
                            out.push('/');
                            break;
                        }
                        Some('\n') => out.push('\n'),
                        Some(_) => out.push(' '),
                        None => break,
                    }
                }
            }
            '\'' | '"' => {
                let quote = c;
                out.push(c);
                while let Some(&next) = iter.peek() {
                    iter.next();
                    if next == quote {
                        if iter.peek() == Some(&quote) {
                            // Doubled-quote escape: stay inside.
                            iter.next();
                            out.push(' ');
                            out.push(' ');
                            continue;
                        }
                        out.push(next);
                        break;
                    }
                    // Mask the body so a keyword inside the literal
                    // (e.g. `'pg_sleep'`) doesn't reach the scanner.
                    out.push(if next == '\n' { '\n' } else { ' ' });
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// Word-boundary substring match. `needle` is searched in `haystack`
/// with the requirement that the character immediately before and
/// after the match is *not* an identifier character (ASCII alphanumeric
/// or underscore).
fn contains_word(haystack: &str, needle: &str) -> bool {
    let n = needle.len();
    if n == 0 || n > haystack.len() {
        return false;
    }
    let bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    let mut i = 0;
    while i + n <= bytes.len() {
        if &bytes[i..i + n] == needle_bytes {
            let before_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
            let after_ok = i + n == bytes.len() || !is_ident_byte(bytes[i + n]);
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

const fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Skip leading whitespace and SQL comments. Returns the remainder.
fn strip_leading_comments_and_whitespace(sql: &str) -> &str {
    let mut s = sql.trim_start();
    loop {
        if let Some(rest) = s.strip_prefix("--") {
            // line comment: skip until newline or EOF
            s = match rest.find('\n') {
                Some(i) => &rest[i + 1..],
                None => "",
            };
            s = s.trim_start();
            continue;
        }
        if let Some(rest) = s.strip_prefix("/*") {
            // block comment: skip until `*/` (or EOF if malformed)
            s = match rest.find("*/") {
                Some(i) => &rest[i + 2..],
                None => "",
            };
            s = s.trim_start();
            continue;
        }
        break;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // guard_read_only — moved verbatim from narwhal-mcp::tools::run_query
    // -----------------------------------------------------------------

    #[test]
    fn guard_accepts_obvious_reads() {
        for sql in [
            "SELECT 1",
            "select * from t",
            "  WITH cte AS (SELECT 1) SELECT * FROM cte",
            "EXPLAIN SELECT 1",
            "PRAGMA table_info(users)",
            "VALUES (1, 2), (3, 4)",
            "-- a comment\nSELECT 1",
            "/* block */ SELECT 1",
            "/* one */ -- two \n SELECT 1",
        ] {
            assert!(guard_read_only(sql).is_ok(), "must accept: {sql:?}");
        }
    }

    #[test]
    fn guard_rejects_writes() {
        for sql in [
            "INSERT INTO t VALUES (1)",
            "UPDATE t SET x = 1",
            "DELETE FROM t",
            "DROP TABLE t",
            "CREATE TABLE t(id INT)",
            "ALTER TABLE t ADD COLUMN x INT",
            "TRUNCATE t",
            "GRANT SELECT ON t TO alice",
            "",
            "   ",
            "-- only comment",
        ] {
            assert!(guard_read_only(sql).is_err(), "must reject: {sql:?}");
        }
    }

    /// Sprint 11 (Opus M4): the `MySQL` backtick identifier form is
    /// not a legitimate way to call a denied function. Previously
    /// `SELECT * FROM `SLEEP`(10)` bypassed the scanner because
    /// backticks aren't `is_ident_byte`, so `SLEEP` looked like a
    /// stand-alone word. After stripping backtick identifiers, the
    /// guard catches this case along with the obvious unquoted call.
    #[test]
    fn guard_rejects_backtick_identifier_bypass() {
        for sql in [
            "SELECT * FROM `SLEEP`(10)",
            "SELECT `pg_sleep`(1)",
            "SELECT * FROM `dbms_lock`.`SLEEP`(1)",
            // Doubled-backtick escape: still inside an identifier.
            "SELECT 1 FROM `tbl``with``SLEEP`(1)",
        ] {
            assert!(
                guard_read_only(sql).is_err(),
                "backtick-wrapped denied call must be rejected: {sql:?}"
            );
        }
    }

    /// Sanity: stripping a backtick body should not break the
    /// trailing keyword scan. `FROM users` after a stripped table
    /// identifier is still a normal read.
    #[test]
    fn strip_preserves_structural_keywords() {
        let sql = "SELECT * FROM `users` WHERE id = 1";
        assert!(
            guard_read_only(sql).is_ok(),
            "non-malicious backtick identifier must still pass: {sql:?}"
        );
    }

    // -----------------------------------------------------------------
    // classify_statement — new in v1.1
    // -----------------------------------------------------------------

    #[test]
    fn classify_reads() {
        for sql in [
            "SELECT 1",
            "  select * from t",
            "WITH cte AS (SELECT 1) SELECT * FROM cte",
            "EXPLAIN SELECT 1",
            "SHOW TABLES",
            "DESCRIBE users",
            "DESC users",
            "PRAGMA table_info(users)",
            "VALUES (1, 2)",
            "TABLE users",
            "-- comment\nSELECT 1",
        ] {
            assert_eq!(classify_statement(sql), StatementKind::Read, "{sql:?}");
        }
    }

    #[test]
    fn classify_writes() {
        for sql in [
            "INSERT INTO t VALUES (1)",
            "UPDATE t SET x = 1",
            "DELETE FROM t",
            "MERGE INTO t USING s ON ...",
            "REPLACE INTO t VALUES (1)",
            "COPY t FROM STDIN",
        ] {
            let k = classify_statement(sql);
            assert_eq!(k, StatementKind::Write, "{sql:?} → {k:?}");
            assert!(k.is_mutating());
        }
    }

    #[test]
    fn classify_ddl() {
        for sql in [
            "CREATE TABLE t(id INT)",
            "DROP TABLE t",
            "ALTER TABLE t ADD COLUMN x INT",
            "TRUNCATE t",
            "GRANT SELECT ON t TO alice",
            "REVOKE ALL ON t FROM alice",
            "VACUUM ANALYZE t",
            "REINDEX t",
            "SET search_path = public",
        ] {
            let k = classify_statement(sql);
            assert_eq!(k, StatementKind::Ddl, "{sql:?} → {k:?}");
            assert!(k.is_mutating());
        }
    }

    #[test]
    fn classify_tx() {
        for sql in [
            "BEGIN",
            "START TRANSACTION",
            "COMMIT",
            "ROLLBACK",
            "SAVEPOINT s1",
            "RELEASE SAVEPOINT s1",
            "END",
        ] {
            let k = classify_statement(sql);
            assert_eq!(k, StatementKind::Tx, "{sql:?} → {k:?}");
            assert!(!k.is_mutating());
        }
    }

    #[test]
    fn classify_unknown() {
        for sql in ["", "   ", "-- only comment", "/* block */", ";"] {
            assert_eq!(classify_statement(sql), StatementKind::Unknown, "{sql:?}");
        }
    }

    /// Read first, even if a write follows. Documented behaviour —
    /// `guard_read_only` is the strict layer that still catches
    /// `WITH … INSERT`-style smuggling because the BEGIN/ROLLBACK
    /// sandwich unwinds it.
    #[test]
    fn classify_with_cte_is_read() {
        assert_eq!(
            classify_statement("WITH cte AS (SELECT 1) INSERT INTO t SELECT * FROM cte"),
            StatementKind::Read,
        );
    }

    // -----------------------------------------------------------------
    // B7: comment-masking — pg_sleep / sleep / waitfor in comments
    // must NOT trip the denylist.
    // -----------------------------------------------------------------

    #[test]
    fn guard_allows_pg_sleep_in_trailing_line_comment() {
        let sql = "SELECT * FROM users -- benchmark candidate, sleep(1)";
        assert!(guard_read_only(sql).is_ok(), "must accept: {sql:?}");
    }

    #[test]
    fn guard_allows_sleep_in_block_comment() {
        let sql = "SELECT 1 /* uses SLEEP for soak */";
        assert!(guard_read_only(sql).is_ok(), "must accept: {sql:?}");
    }

    #[test]
    fn guard_allows_waitfor_in_mid_statement_block_comment() {
        let sql = "SELECT id /* TODO: drop pg_sleep usage */ FROM t";
        assert!(guard_read_only(sql).is_ok(), "must accept: {sql:?}");
    }

    /// Regression: actual executable blocked functions must still be rejected.
    #[test]
    fn guard_still_rejects_pg_sleep_in_actual_code() {
        for sql in [
            "SELECT pg_sleep(1)",
            "SELECT * FROM t WHERE SLEEP(5) = 0",
            "SELECT BENCHMARK(1000000, SHA1('test'))",
            "SELECT 1; WAITFOR DELAY '00:00:05'",
        ] {
            assert!(guard_read_only(sql).is_err(), "must reject: {sql:?}");
        }
    }

    #[test]
    fn guard_handles_block_comment_with_newlines() {
        let sql = "SELECT id\n/* multi-line\n   WAITFOR DELAY workaround\n*/\nFROM t";
        assert!(guard_read_only(sql).is_ok(), "must accept: {sql:?}");
    }
}
