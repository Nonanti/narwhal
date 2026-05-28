//! `:diff <left> <right>` schema diff handler.
//!
//! Compares two tables (`schema.table` qualified) inside the active
//! connection and writes the generated `ALTER TABLE` statements to a
//! fresh editor tab so the user can review + execute. Failures land
//! on the status bar; the user's open tabs are never modified
//! mid-flight.

use narwhal_commands::schema_diff::render_alter_statements;

use super::AppCore;

impl AppCore {
    /// Resolve two `schema.table` strings against the active session
    /// and dump the migration SQL into a new tab.
    pub(super) async fn diff_schema_command(&mut self, left: String, right: String) {
        let Some((left_schema, left_table)) = split_qualified(&left) else {
            self.ui.status.message = format!("diff: '{left}' is not a qualified name");
            return;
        };
        let Some((right_schema, right_table)) = split_qualified(&right) else {
            self.ui.status.message = format!("diff: '{right}' is not a qualified name");
            return;
        };

        let Some(session) = self.session.active.as_mut() else {
            self.ui.status.message = "diff: no active session".into();
            return;
        };
        let dialect = session.dialect();
        // m-2: route both describes through the cached entry point.
        // Re-running `:diff a b` after editing one side and saving
        // (without `:refresh`) returns the cached snapshot —
        // intentional: the user's intent at that point is to inspect
        // their pending change against the snapshot they reasoned
        // about, and the explicit `:refresh` they'll run next clears
        // the cache for the live view.
        let before = match session
            .describe_table_cached(&left_schema, &left_table)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                self.ui.status.message = format!("diff: describe {left} failed: {e}");
                return;
            }
        };
        let after = match session
            .describe_table_cached(&right_schema, &right_table)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                self.ui.status.message = format!("diff: describe {right} failed: {e}");
                return;
            }
        };

        let stmts = render_alter_statements(&before, &after, dialect);
        if stmts.is_empty() {
            self.ui.status.message = format!("diff: {left} and {right} are identical");
            return;
        }

        let mut buf = String::with_capacity(stmts.iter().map(String::len).sum::<usize>() + 256);
        buf.push_str(&format!("-- diff: {left}  ->  {right}\n"));
        buf.push_str(&format!("-- {} change(s)\n\n", stmts.len()));
        for s in &stmts {
            buf.push_str(s);
            buf.push('\n');
        }

        // Open a new tab so the user can review/edit before running.
        self.new_tab().await;
        let tab = &mut self.ui.tabs[self.ui.active_tab];
        tab.editor.insert_str(&buf);
        self.ui.status.message = format!("diff: {} ALTER statement(s) in new tab", stmts.len());
    }
}

/// Split a `schema.table` argument into its two halves. Returns
/// `None` when the input doesn't look like a qualified name.
///
/// Supports the three forms users commonly type at the `:diff`
/// prompt:
/// - Plain: `public.users`
/// - Postgres-quoted: `"weird.schema".users` (dots inside the
/// double-quoted half are part of the identifier, not the
/// separator).
/// - MySQL/SQLite-quoted: `` `weird.schema`.users ``.
///
/// The quoting is stripped from the return value so the caller
/// (`describe_table`) gets the raw schema and table names. Escaped
/// inner quotes (`""`/```` ``  ````) collapse to a single quote, matching the
/// SQL identifier rules. Inputs with more than one top-level `.`
/// (`a.b.c`) or with empty halves are rejected.
///
/// The `narwhal-sql` splitter is intentionally not used here because
/// users type this argument by hand and we want a forgiving
/// exact-match — only enough parsing to honour the quoting
/// convention they would use everywhere else.
fn split_qualified(s: &str) -> Option<(String, String)> {
    let (left, rest) = parse_ident_segment(s)?;
    let rest = rest.strip_prefix('.')?;
    let (right, tail) = parse_ident_segment(rest)?;
    if !tail.is_empty() || left.is_empty() || right.is_empty() {
        return None;
    }
    Some((left, right))
}

/// Consume one identifier segment from the front of `s`. Recognises
/// double-quoted (`"…"`) and backtick-quoted (`` `…` ``) forms, with
/// `""`/```` `` ```` escaped quotes inside. Returns the unquoted ident
/// plus whatever remains after it (typically a `.` followed by the
/// next segment, or end-of-input).
fn parse_ident_segment(s: &str) -> Option<(String, &str)> {
    let bytes = s.as_bytes();
    if let Some(&first) = bytes.first() {
        if first == b'"' || first == b'`' {
            return parse_quoted_segment(s, first);
        }
    }
    // Bare segment: read up to the first `.` or end-of-input.
    let end = s.find('.').unwrap_or(s.len());
    Some((s[..end].to_owned(), &s[end..]))
}

fn parse_quoted_segment(s: &str, quote: u8) -> Option<(String, &str)> {
    debug_assert_eq!(s.as_bytes().first().copied(), Some(quote));
    let bytes = s.as_bytes();
    let mut out = String::new();
    let mut i = 1;
    while i < bytes.len() {
        if bytes[i] == quote {
            // Escaped quote (`""` or `` `` ``) — keep one, skip the
            // doubled second.
            if bytes.get(i + 1) == Some(&quote) {
                out.push(quote as char);
                i += 2;
                continue;
            }
            // Closing quote.
            return Some((out, &s[i + 1..]));
        }
        // Pass UTF-8 bytes through one at a time. Identifier
        // characters are restricted to ASCII in practice for the
        // engines narwhal targets, but a stray multi-byte sequence
        // is forwarded losslessly so the error path matches the
        // driver's later complaint.
        out.push(bytes[i] as char);
        i += 1;
    }
    None // unterminated quoted ident
}

#[cfg(test)]
mod tests {
    use super::split_qualified;

    #[test]
    fn split_qualified_accepts_schema_dot_table() {
        assert_eq!(
            split_qualified("public.users"),
            Some(("public".to_owned(), "users".to_owned()))
        );
    }

    #[test]
    fn split_qualified_rejects_unqualified() {
        assert!(split_qualified("users").is_none());
        assert!(split_qualified("").is_none());
        assert!(split_qualified(".users").is_none());
        assert!(split_qualified("public.").is_none());
        assert!(split_qualified("a.b.c").is_none());
    }

    /// m-5 regression: a PG identifier may legitimately contain a
    /// `.` when it is double-quoted. `:diff "weird.schema".users
    /// "weird.schema".orders` used to reject both arguments because
    /// the parser keyed on the first `.` it saw.
    #[test]
    fn split_qualified_handles_pg_quoted_dotted_schema() {
        assert_eq!(
            split_qualified("\"weird.schema\".users"),
            Some(("weird.schema".to_owned(), "users".to_owned()))
        );
    }

    /// `MySQL` / `SQLite` backtick form.
    #[test]
    fn split_qualified_handles_backtick_dotted_schema() {
        assert_eq!(
            split_qualified("`weird.schema`.users"),
            Some(("weird.schema".to_owned(), "users".to_owned()))
        );
    }

    /// Both halves can be quoted independently.
    #[test]
    fn split_qualified_both_quoted() {
        assert_eq!(
            split_qualified("\"a\".\"b\""),
            Some(("a".to_owned(), "b".to_owned()))
        );
        assert_eq!(
            split_qualified("`a`.`b`"),
            Some(("a".to_owned(), "b".to_owned()))
        );
    }

    /// Escaped inner quote collapses to a single character.
    #[test]
    fn split_qualified_unescapes_doubled_quotes() {
        assert_eq!(
            split_qualified("\"sch\"\"ema\".t"),
            Some(("sch\"ema".to_owned(), "t".to_owned()))
        );
    }

    /// Unterminated quoting is a syntax error — don't paper over it.
    #[test]
    fn split_qualified_rejects_unterminated_quote() {
        assert!(split_qualified("\"weird.schema.users").is_none());
        assert!(split_qualified("`weird.schema.users").is_none());
    }
}
