//! `:diff <left> <right>` schema diff handler (v1.2 #8).
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
        let mut conn = match session.pool.acquire().await {
            Ok(c) => c,
            Err(e) => {
                self.ui.status.message = format!("diff: pool acquire failed: {e}");
                return;
            }
        };
        let before = match conn.describe_table(left_schema, left_table).await {
            Ok(s) => s,
            Err(e) => {
                self.ui.status.message = format!("diff: describe {left} failed: {e}");
                return;
            }
        };
        let after = match conn.describe_table(right_schema, right_table).await {
            Ok(s) => s,
            Err(e) => {
                self.ui.status.message = format!("diff: describe {right} failed: {e}");
                return;
            }
        };
        drop(conn);

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

/// Split `"schema.table"` into `("schema", "table")`. Returns `None`
/// when the input doesn't contain exactly one `.`. The `narwhal-sql`
/// splitter is intentionally not used here because users type this
/// argument by hand and we want a forgiving exact-match.
fn split_qualified(s: &str) -> Option<(&str, &str)> {
    let (a, b) = s.split_once('.')?;
    if a.is_empty() || b.is_empty() || b.contains('.') {
        return None;
    }
    Some((a, b))
}

#[cfg(test)]
mod tests {
    use super::split_qualified;

    #[test]
    fn split_qualified_accepts_schema_dot_table() {
        assert_eq!(split_qualified("public.users"), Some(("public", "users")));
    }

    #[test]
    fn split_qualified_rejects_unqualified() {
        assert!(split_qualified("users").is_none());
        assert!(split_qualified("").is_none());
        assert!(split_qualified(".users").is_none());
        assert!(split_qualified("public.").is_none());
        assert!(split_qualified("a.b.c").is_none());
    }
}
