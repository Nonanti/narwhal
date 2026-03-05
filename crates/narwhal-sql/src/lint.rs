//! Lightweight SQL lint heuristics (v1.3 #9).
//!
//! Intentionally **not** a parser. Each lint is a textual heuristic
//! that runs on the post-comment-stripped source. The goal is to
//! catch the common foot-guns before they reach the network, with
//! O(n) cost so the editor can call into this on every keystroke.
//!
//! The list is conservative: false positives on legitimate code are
//! a much bigger problem than false negatives, because users will
//! disable the linter if it cries wolf. New rules should ship with
//! a deny-list of known idioms that look like the rule but aren't.
//!
//! For deeper checks (alias resolution, unused CTE, cross-table
//! identifiers) a real parser is required — deferred to v2.

/// Severity of a single lint finding. Drives the UI badge / colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LintSeverity {
    /// Stylistic / advisory. Yellow underline.
    Info,
    /// Likely bug or dangerous statement. Red underline.
    Warning,
}

/// One lint finding tied to a (1-based) line number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintFinding {
    /// Internal short code (`select-star`, `update-no-where`, …).
    /// Stable across releases so users can disable individual rules
    /// via `:lint <id> off` (v2).
    pub rule: &'static str,
    /// One-line human-readable message.
    pub message: String,
    /// 1-based line number within the input the lint fires on.
    pub line: usize,
    pub severity: LintSeverity,
}

/// Run every lint rule against `sql` and return the findings in
/// source order. Comments are stripped before destructive checks so
/// a `-- DELETE FROM users` doesn't trip the rule; the `select-star`
/// rule looks at the original source so its `-- lint:allow …`
/// pragma still works.
#[must_use]
pub fn lint(sql: &str) -> Vec<LintFinding> {
    let stripped = strip_comments(sql);
    let mut out = Vec::new();
    out.extend(check_select_star(sql));
    out.extend(check_destructive_no_where(&stripped));
    out.extend(check_cartesian_join(&stripped));
    out.sort_by_key(|f| f.line);
    out
}

/// Rule `select-star`. Suppressed by a trailing
/// `-- lint:allow select-star` on the same line.
fn check_select_star(sql: &str) -> Vec<LintFinding> {
    let mut out = Vec::new();
    for (i, line) in sql.lines().enumerate() {
        if line.contains("lint:allow select-star") {
            continue;
        }
        let upper = line.to_ascii_uppercase();
        if let Some(p) = upper.find("SELECT") {
            // Require the token to be flanked by non-identifier chars
            // so 'reselect' / 'SELECTED' don't trip.
            let before_ok = p == 0
                || !upper.as_bytes()[p - 1].is_ascii_alphanumeric()
                    && upper.as_bytes()[p - 1] != b'_';
            let after = &upper[p + 6..];
            let after_ok = after
                .as_bytes()
                .first()
                .copied()
                .map_or(true, |b| !b.is_ascii_alphanumeric() && b != b'_');
            if !before_ok || !after_ok {
                continue;
            }
            let first_non_ws = after.trim_start();
            if first_non_ws.starts_with('*') {
                out.push(LintFinding {
                    rule: "select-star",
                    message: "SELECT * — prefer an explicit column list in shared queries".into(),
                    line: i + 1,
                    severity: LintSeverity::Info,
                });
            }
        }
    }
    out
}

/// Rule `destructive-no-where`. `UPDATE` / `DELETE` / `TRUNCATE`
/// without a `WHERE` clause runs against every row.
fn check_destructive_no_where(sql: &str) -> Vec<LintFinding> {
    let mut out = Vec::new();
    let upper = sql.to_ascii_uppercase();
    let mut offset = 0;
    for stmt in upper.split(';') {
        let trimmed = stmt.trim_start();
        let starts_destructive = trimmed.starts_with("UPDATE ")
            || trimmed.starts_with("DELETE ")
            || trimmed.starts_with("DELETE FROM");
        let starts_truncate = trimmed.starts_with("TRUNCATE");
        if starts_destructive && !has_word(trimmed, "WHERE") {
            let line = upper[..offset].matches('\n').count() + 1;
            out.push(LintFinding {
                rule: "destructive-no-where",
                message: "UPDATE/DELETE without WHERE — will affect every row".into(),
                line,
                severity: LintSeverity::Warning,
            });
        }
        if starts_truncate {
            let line = upper[..offset].matches('\n').count() + 1;
            out.push(LintFinding {
                rule: "truncate",
                message: "TRUNCATE drops all rows and bypasses triggers".into(),
                line,
                severity: LintSeverity::Warning,
            });
        }
        offset += stmt.len() + 1;
    }
    out
}

/// Rule `cartesian-join`. `FROM a, b` with no joining `WHERE` is
/// almost always a bug.
fn check_cartesian_join(sql: &str) -> Vec<LintFinding> {
    let mut out = Vec::new();
    let upper = sql.to_ascii_uppercase();
    let Some(from_pos) = upper.find("FROM ") else {
        return out;
    };
    let after_from = &upper[from_pos + 5..];
    let end = after_from
        .find(" WHERE ")
        .or_else(|| after_from.find(" GROUP "))
        .or_else(|| after_from.find(" ORDER "))
        .or_else(|| after_from.find(" LIMIT "))
        .unwrap_or(after_from.len());
    let clause = &after_from[..end];
    let has_where = after_from.contains(" WHERE ");
    let has_join = clause.contains(" JOIN ");
    if !has_where && !has_join && clause.contains(',') {
        let line = upper[..from_pos].matches('\n').count() + 1;
        out.push(LintFinding {
            rule: "cartesian-join",
            message: "FROM a, b without a JOIN condition — likely a Cartesian product".into(),
            line,
            severity: LintSeverity::Warning,
        });
    }
    out
}

/// Strip `--` line and `/* … */` block comments. Preserves newlines
/// inside line comments so line numbers stay accurate.
fn strip_comments(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '-' && chars.peek() == Some(&'-') {
            chars.next();
            for next in chars.by_ref() {
                if next == '\n' {
                    out.push('\n');
                    break;
                }
            }
        } else if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut prev = ' ';
            for next in chars.by_ref() {
                if prev == '*' && next == '/' {
                    break;
                }
                if next == '\n' {
                    out.push('\n');
                }
                prev = next;
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Word-boundary substring match (shared with `guard::contains_word`
/// in spirit; duplicated here to keep `lint` independent).
fn has_word(haystack: &str, needle: &str) -> bool {
    let n = needle.len();
    if n == 0 || n > haystack.len() {
        return false;
    }
    let bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    let mut i = 0;
    while i + n <= bytes.len() {
        if &bytes[i..i + n] == needle_bytes {
            let before_ok = i == 0 || !is_ident(bytes[i - 1]);
            let after_ok = i + n == bytes.len() || !is_ident(bytes[i + n]);
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

const fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(findings: &[LintFinding]) -> Vec<&'static str> {
        findings.iter().map(|f| f.rule).collect()
    }

    #[test]
    fn select_star_warns() {
        let r = lint("SELECT * FROM users");
        assert!(rules(&r).contains(&"select-star"));
    }

    #[test]
    fn select_star_silenced_by_pragma_comment() {
        let r = lint("SELECT * FROM users -- lint:allow select-star");
        assert!(!rules(&r).contains(&"select-star"));
    }

    #[test]
    fn select_with_columns_does_not_warn() {
        let r = lint("SELECT id, name FROM users");
        assert!(!rules(&r).contains(&"select-star"));
    }

    #[test]
    fn update_without_where_warns() {
        let r = lint("UPDATE users SET active = false");
        assert!(rules(&r).contains(&"destructive-no-where"));
    }

    #[test]
    fn update_with_where_does_not_warn() {
        let r = lint("UPDATE users SET active = false WHERE id = 7");
        assert!(!rules(&r).contains(&"destructive-no-where"));
    }

    #[test]
    fn truncate_warns() {
        let r = lint("TRUNCATE users");
        assert!(rules(&r).contains(&"truncate"));
    }

    #[test]
    fn cartesian_warns() {
        let r = lint("SELECT * FROM a, b");
        assert!(rules(&r).contains(&"cartesian-join"));
    }

    #[test]
    fn cartesian_silenced_by_where() {
        let r = lint("SELECT * FROM a, b WHERE a.id = b.a_id");
        assert!(!rules(&r).contains(&"cartesian-join"));
    }

    #[test]
    fn cartesian_silenced_by_join() {
        let r = lint("SELECT * FROM a JOIN b ON a.id = b.a_id");
        assert!(!rules(&r).contains(&"cartesian-join"));
    }

    #[test]
    fn comments_are_stripped() {
        let r = lint("-- DELETE FROM users -- not real\nSELECT 1");
        assert!(!rules(&r).contains(&"destructive-no-where"));
    }
}
