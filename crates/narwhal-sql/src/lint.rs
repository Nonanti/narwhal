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

/// Run every lint rule against `sql` with the conservative default
/// dialect ([`crate::splitter::Dialect::Generic`]). Comments are
/// stripped before destructive checks so a `-- DELETE FROM users`
/// doesn't trip the rule; the `select-star` rule looks at the
/// original source so its `-- lint:allow …` pragma still works.
///
/// Most call sites should prefer [`lint_with_dialect`] so PG
/// dollar-quoted strings and `MySQL` backtick identifiers aren't
/// mis-parsed. `lint` exists for back-compat callers that don't
/// know their dialect.
#[must_use]
pub fn lint(sql: &str) -> Vec<LintFinding> {
    lint_with_dialect(sql, crate::splitter::Dialect::Generic)
}

/// Dialect-aware lint pass. The dialect is threaded into the
/// statement splitter so `MySQL` backtick identifiers, `PostgreSQL`
/// dollar-quoted strings and engine-specific string literal
/// conventions don't fragment statements at false boundaries.
///
/// M-4 / M-5: both [`check_destructive_no_where`] and
/// [`check_cartesian_join`] now share the splitter and operate on the
/// original source so byte offsets line up with the user's view of
/// the file. `check_select_star` keeps its line-by-line scan because
/// the `-- lint:allow select-star` pragma has to remain visible.
#[must_use]
pub fn lint_with_dialect(sql: &str, dialect: crate::splitter::Dialect) -> Vec<LintFinding> {
    let mut out = Vec::new();
    out.extend(check_select_star(sql));
    out.extend(check_destructive_no_where(sql, dialect));
    out.extend(check_cartesian_join(sql, dialect));
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
///
/// M-2 / M-4: uses [`crate::splitter::split_with`] instead of a
/// naive `;` split so a string literal containing `;`
/// (`UPDATE x SET name='a;b'`) doesn't get fragmented and trick the
/// heuristic. The caller-supplied dialect lets the splitter
/// recognise `PostgreSQL` dollar-quoting and `MySQL` backtick
/// identifiers; under [`crate::splitter::Dialect::Generic`] only
/// standard SQL single-quoted strings and `--` / `/* */` comments
/// are tracked.
fn check_destructive_no_where(sql: &str, dialect: crate::splitter::Dialect) -> Vec<LintFinding> {
    let mut out = Vec::new();
    for stmt in crate::splitter::split_with(sql, dialect) {
        let upper = stmt.text.to_ascii_uppercase();
        // M-A: a leading CTE (`WITH cte AS (...) DELETE FROM ...`) is
        // valid in PostgreSQL and SQLite and used commonly to scope a
        // destructive statement. The previous version only inspected
        // the first token of the statement and silently passed any
        // CTE-prefixed DELETE/UPDATE through the linter —
        // ironically the most dangerous-looking form. Strip the CTE
        // prefix before classifying.
        let main = skip_cte_prefix(&upper);
        let trimmed = main.trim_start();
        let starts_destructive = trimmed.starts_with("UPDATE ")
            || trimmed.starts_with("DELETE ")
            || trimmed.starts_with("DELETE FROM");
        let starts_truncate = trimmed.starts_with("TRUNCATE");
        if !starts_destructive && !starts_truncate {
            continue;
        }
        // Line number from the statement's offset in the original
        // source. `Statement::start` is a byte offset that the
        // splitter guarantees lands on the first non-whitespace
        // byte of the statement, so counting newlines up to it is
        // accurate even across `;`-separated multi-line scripts.
        // We anchor on the main-statement keyword (post-CTE) so the
        // squiggle lands on `DELETE`, not on the leading `WITH`.
        let main_offset = upper.len() - trimmed.len();
        let line = sql[..stmt.start + main_offset].matches('\n').count() + 1;
        if starts_destructive && find_top_level(trimmed.as_bytes(), b" WHERE ").is_none() {
            // Top-level WHERE only — a WHERE inside a subquery on
            // the right-hand side of an `IN (...)` or `EXISTS (...)`
            // does not constrain the destructive statement.
            out.push(LintFinding {
                rule: "destructive-no-where",
                message: "UPDATE/DELETE without WHERE — will affect every row".into(),
                line,
                severity: LintSeverity::Warning,
            });
        }
        if starts_truncate {
            out.push(LintFinding {
                rule: "truncate",
                message: "TRUNCATE drops all rows and bypasses triggers".into(),
                line,
                severity: LintSeverity::Warning,
            });
        }
    }
    out
}

/// Rule `cartesian-join`. `FROM a, b` with no joining `WHERE` /
/// JOIN clause is almost always a bug.
///
/// M-5: walks the dialect-aware splitter just like
/// [`check_destructive_no_where`] and reports findings per
/// statement. The earlier implementation operated on a
/// comment-stripped copy of the entire script and consequently
/// (a) missed Cartesian joins in any non-first statement and
/// (b) reported the wrong line in multi-statement scripts.
fn check_cartesian_join(sql: &str, dialect: crate::splitter::Dialect) -> Vec<LintFinding> {
    let mut out = Vec::new();
    for stmt in crate::splitter::split_with(sql, dialect) {
        // The cartesian check only makes sense on a SELECT. A
        // leading TRUNCATE/UPDATE/DELETE never fires this rule, so
        // skip early to keep the false-positive rate low.
        //
        // M-A: strip a leading CTE so a `WITH … SELECT * FROM a, b`
        // is analysed the same as a bare SELECT.
        let upper = stmt.text.to_ascii_uppercase();
        let main = skip_cte_prefix(&upper);
        let trimmed = main.trim_start();
        if !trimmed.starts_with("SELECT ") {
            continue;
        }
        let main_offset = upper.len() - trimmed.len();
        // M-C: walk top-level (paren-depth-0) only so a subquery
        // inside the FROM list cannot mask the outer cartesian. The
        // earlier `upper.find("FROM ")` could match a `FROM` inside
        // a derived-table subquery; likewise a subquery WHERE used
        // to silence the rule on a real outer cartesian.
        let trimmed_bytes = trimmed.as_bytes();
        let Some(from_pos) = find_top_level(trimmed_bytes, b"FROM ") else {
            continue;
        };
        let after_from = &trimmed[from_pos + 5..];
        let after_bytes = after_from.as_bytes();
        let end = [
            find_top_level(after_bytes, b" WHERE "),
            find_top_level(after_bytes, b" GROUP "),
            find_top_level(after_bytes, b" ORDER "),
            find_top_level(after_bytes, b" LIMIT "),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(after_bytes.len());
        let clause = &after_from[..end];
        let clause_bytes = clause.as_bytes();
        let has_top_where = find_top_level(after_bytes, b" WHERE ").is_some();
        let has_top_join = find_top_level(clause_bytes, b" JOIN ").is_some();
        let has_top_comma = contains_top_level_byte(clause_bytes, b',');
        if !has_top_where && !has_top_join && has_top_comma {
            // Translate the in-statement `FROM` position back to a
            // line in the original source.
            let line = sql[..stmt.start + main_offset + from_pos]
                .matches('\n')
                .count()
                + 1;
            out.push(LintFinding {
                rule: "cartesian-join",
                message: "FROM a, b without a JOIN condition — likely a Cartesian product".into(),
                line,
                severity: LintSeverity::Warning,
            });
        }
    }
    out
}

/// Skip a leading `WITH … (…)[, … (…)]` CTE prefix on an uppercase
/// statement. Returns a suffix that starts with the main statement's
/// first non-whitespace character (`DELETE`, `UPDATE`, `SELECT`,
/// …). When the input doesn't start with `WITH`, the whole
/// (trimmed) input is returned unchanged.
///
/// Recognises:
/// - `WITH [RECURSIVE] name AS (…) main_stmt`
/// - `WITH name (cols) AS (…) main_stmt`
/// - `WITH name AS [NOT] MATERIALIZED (…) main_stmt`
/// - Multiple comma-separated CTEs before the main statement.
///
/// Heuristic: column-list / body parens are tracked with depth so a
/// `(a, b)` column list and a body that contains `(...)` subqueries
/// don't confuse the scanner. Returns the original (trimmed) input
/// on any malformed prefix so the downstream linter never gets a
/// shorter view than the user typed.
fn skip_cte_prefix(upper: &str) -> &str {
    let s = upper.trim_start();
    if !begins_with_keyword(s, "WITH") {
        return s;
    }
    let bytes = s.as_bytes();
    let mut i = "WITH".len();
    i = skip_ws(bytes, i);
    // Optional RECURSIVE
    if begins_with_keyword(&s[i..], "RECURSIVE") {
        i += "RECURSIVE".len();
        i = skip_ws(bytes, i);
    }
    loop {
        // CTE name (identifier characters until ws / `(`)
        let name_start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'(' {
            i += 1;
        }
        if i == name_start {
            return &s[i..];
        }
        i = skip_ws(bytes, i);
        // Optional column list: `(a, b, c)`
        if bytes.get(i) == Some(&b'(') {
            i = skip_balanced_parens(bytes, i);
            i = skip_ws(bytes, i);
        }
        // `AS` is mandatory
        if !begins_with_keyword(&s[i..], "AS") {
            return &s[i..];
        }
        i += "AS".len();
        i = skip_ws(bytes, i);
        // Optional [NOT] MATERIALIZED
        if begins_with_keyword(&s[i..], "NOT") {
            i += "NOT".len();
            i = skip_ws(bytes, i);
        }
        if begins_with_keyword(&s[i..], "MATERIALIZED") {
            i += "MATERIALIZED".len();
            i = skip_ws(bytes, i);
        }
        // Body parentheses
        if bytes.get(i) != Some(&b'(') {
            return &s[i..];
        }
        i = skip_balanced_parens(bytes, i);
        i = skip_ws(bytes, i);
        // More CTEs (`, name AS (…)`) or main statement
        if bytes.get(i) == Some(&b',') {
            i += 1;
            i = skip_ws(bytes, i);
            continue;
        }
        return &s[i..];
    }
}

/// True if `s` begins with the uppercase keyword `kw` followed by a
/// non-identifier byte (whitespace, `(`, or end-of-input). Assumes
/// `s` is already uppercase ASCII for the keyword bytes.
fn begins_with_keyword(s: &str, kw: &str) -> bool {
    s.as_bytes().starts_with(kw.as_bytes())
        && s.as_bytes()
            .get(kw.len())
            .map_or(true, |b| !b.is_ascii_alphanumeric() && *b != b'_')
}

/// Advance past ASCII whitespace.
fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Caller guarantees `b[i] == b'('`. Returns the byte index one past
/// the matching `)`. If no match is found, returns `b.len()` (the
/// downstream linter then trips its `Some(…)` guards on the result
/// and bails out cleanly).
fn skip_balanced_parens(b: &[u8], mut i: usize) -> usize {
    debug_assert_eq!(b.get(i), Some(&b'('));
    let mut depth: i32 = 1;
    i += 1;
    while i < b.len() && depth > 0 {
        match b[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    i
}

/// Find the first occurrence of `needle` in `haystack` outside any
/// `(` `)` group. Used to distinguish a top-level `WHERE` / `JOIN`
/// from one buried inside a subquery in the FROM list (M-C).
fn find_top_level(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let mut depth: i32 = 0;
    let mut i = 0;
    while i + needle.len() <= haystack.len() {
        match haystack[i] {
            b'(' => {
                depth += 1;
                i += 1;
                continue;
            }
            b')' => {
                if depth > 0 {
                    depth -= 1;
                }
                i += 1;
                continue;
            }
            _ => {}
        }
        if depth == 0 && haystack[i..i + needle.len()] == *needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// True if `haystack` contains `needle` at top level (paren depth 0).
fn contains_top_level_byte(haystack: &[u8], needle: u8) -> bool {
    let mut depth: i32 = 0;
    for &b in haystack {
        match b {
            b'(' => depth += 1,
            b')' => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            _ if depth == 0 && b == needle => return true,
            _ => {}
        }
    }
    false
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

    #[test]
    fn semicolon_inside_string_literal_does_not_split_statement() {
        // M2 regression: a naive `;` split would have produced two
        // chunks here. The second — " UPDATE" — starts with the
        // word UPDATE and has no WHERE, so the linter used to fire a
        // false-positive destructive-no-where warning. The splitter
        // knows the `;` is inside a single-quoted literal and keeps
        // the statement whole, so the WHERE-suffixed UPDATE no
        // longer trips the rule.
        let sql = "UPDATE users SET note = 'a;b; UPDATE c' WHERE id = 1";
        let r = lint(sql);
        assert!(
            !rules(&r).contains(&"destructive-no-where"),
            "unexpected findings: {:?}",
            rules(&r)
        );
    }

    #[test]
    fn destructive_in_second_statement_still_caught() {
        let sql = "SELECT 1;\nDELETE FROM users";
        let r = lint(sql);
        assert!(rules(&r).contains(&"destructive-no-where"));
        // Line number should point at the DELETE, not the SELECT.
        let finding = r.iter().find(|f| f.rule == "destructive-no-where").unwrap();
        assert_eq!(finding.line, 2);
    }

    #[test]
    fn cartesian_join_caught_in_second_statement() {
        // M-5 regression: prior implementation only inspected the
        // first FROM in the script.
        let sql = "SELECT 1;\nSELECT * FROM a, b";
        let r = lint(sql);
        assert!(rules(&r).contains(&"cartesian-join"));
        let finding = r.iter().find(|f| f.rule == "cartesian-join").unwrap();
        assert_eq!(finding.line, 2);
    }

    #[test]
    fn cartesian_join_does_not_fire_on_update() {
        // M-5: the rule used to scan FROM anywhere in the source
        // and could fire on UPDATE … FROM joins.
        let sql = "UPDATE x SET v = 1 FROM a, b WHERE x.id = a.id AND a.id = b.id";
        let r = lint(sql);
        assert!(!rules(&r).contains(&"cartesian-join"));
    }

    #[test]
    fn cte_delete_caught() {
        // M-A regression: previously `WITH … DELETE` was silently
        // skipped because the destructive-no-where rule only
        // inspected the first token of the statement.
        let r = lint("WITH old AS (SELECT id FROM users) DELETE FROM users");
        assert!(
            rules(&r).contains(&"destructive-no-where"),
            "unexpected findings: {:?}",
            rules(&r)
        );
    }

    #[test]
    fn cte_update_caught() {
        let r = lint("WITH x AS (SELECT 1) UPDATE users SET banned = true");
        assert!(rules(&r).contains(&"destructive-no-where"));
    }

    #[test]
    fn cte_update_with_where_does_not_warn() {
        let r = lint("WITH x AS (SELECT 1) UPDATE users SET banned = true WHERE id = 7");
        assert!(
            !rules(&r).contains(&"destructive-no-where"),
            "unexpected findings: {:?}",
            rules(&r)
        );
    }

    #[test]
    fn cte_with_column_list_handled() {
        let r = lint("WITH x(a, b) AS (SELECT 1, 2) DELETE FROM users");
        assert!(rules(&r).contains(&"destructive-no-where"));
    }

    #[test]
    fn cte_recursive_handled() {
        let r = lint("WITH RECURSIVE t AS (SELECT 1) DELETE FROM users");
        assert!(rules(&r).contains(&"destructive-no-where"));
    }

    #[test]
    fn multi_cte_handled() {
        let r = lint("WITH a AS (SELECT 1), b AS (SELECT 2) DELETE FROM users");
        assert!(rules(&r).contains(&"destructive-no-where"));
    }

    #[test]
    fn cte_materialized_keyword_handled() {
        let r = lint("WITH x AS NOT MATERIALIZED (SELECT 1) DELETE FROM users");
        assert!(rules(&r).contains(&"destructive-no-where"));
    }

    #[test]
    fn cte_select_star_cartesian_still_caught() {
        // M-A: a CTE-prefixed SELECT with a top-level Cartesian
        // FROM clause should still trip the cartesian-join rule.
        let r = lint("WITH x AS (SELECT 1) SELECT * FROM a, b");
        assert!(
            rules(&r).contains(&"cartesian-join"),
            "unexpected findings: {:?}",
            rules(&r)
        );
    }

    #[test]
    fn subquery_where_does_not_silence_cartesian() {
        // M-C regression: the previous `after_from.contains(" WHERE ")`
        // substring test was fooled by a WHERE buried inside a
        // FROM-list subquery. With paren-aware top-level scanning,
        // the outer `a, b` cartesian is now reported even when an
        // inner subquery has its own WHERE.
        let r = lint("SELECT * FROM (SELECT id FROM x WHERE x.flag = 1) a, b");
        assert!(
            rules(&r).contains(&"cartesian-join"),
            "unexpected findings: {:?}",
            rules(&r)
        );
    }

    #[test]
    fn subquery_where_in_select_list_does_not_silence_destructive() {
        // M-C / M-A interaction: a `WHERE` inside an `EXISTS(…)`
        // subquery on the right-hand side of `SET` (UPDATE) or on
        // the right-hand side of an `IN (…)` (DELETE) does not
        // constrain the outer destructive statement. The top-level
        // scan keeps the rule honest.
        let r = lint(
            "DELETE FROM users WHERE id IN (SELECT user_id FROM banned WHERE banned.kind = 1)",
        );
        // This *does* have a top-level WHERE so no warning.
        assert!(!rules(&r).contains(&"destructive-no-where"));

        let r = lint(
            "DELETE FROM users a USING banned b WHERE a.id IN (SELECT id FROM x WHERE x.f = 1)",
        );
        assert!(!rules(&r).contains(&"destructive-no-where"));

        // No top-level WHERE; the only WHERE lives inside an
        // EXISTS subquery on the SET expression.
        let r = lint("UPDATE users SET banned = EXISTS (SELECT 1 FROM x WHERE x.id = users.id)");
        assert!(
            rules(&r).contains(&"destructive-no-where"),
            "unexpected findings: {:?}",
            rules(&r)
        );
    }

    #[test]
    fn postgres_dollar_quoted_semicolon_does_not_split() {
        // M-4: with the Postgres dialect, $$ ... $$ delimits a
        // dollar-quoted string and embedded `;` characters are part
        // of the literal, not statement terminators. Under Generic
        // dialect the splitter would treat the `;` as a separator
        // and the rule would mis-fire on the second "chunk".
        let sql = "CREATE FUNCTION f() RETURNS void AS $$ DELETE FROM users; $$ LANGUAGE sql";
        let r = lint_with_dialect(sql, crate::splitter::Dialect::Postgres);
        assert!(
            !rules(&r).contains(&"destructive-no-where"),
            "unexpected findings: {:?}",
            rules(&r)
        );
    }
}
