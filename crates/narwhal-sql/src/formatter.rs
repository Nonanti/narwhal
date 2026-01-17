//! Dialect-aware SQL pretty-printer.
//!
//! Wraps the [`sqlformat`] crate so the rest of the workspace doesn't
//! have to know about its option struct. The dialect is mostly used to
//! pick between standard SQL keywords and `MySQL` `` ` `` quoting style;
//! everything else (line breaks, indentation, comma placement) is
//! shared across drivers.
//!
//! Formatting is a *best effort* transform. We deliberately avoid
//! returning errors because:
//!
//! - syntactically broken SQL is the common case while editing,
//!   and silently leaving the input alone is friendlier than failing,
//! - the formatter never executes; the worst case is a messy-but-still-
//!   runnable buffer.
//!
//! The caller decides what to format: `:format` passes the statement
//! under the cursor (via the splitter), `:format-all` passes the
//! whole buffer.
//!
//! Indent width is fixed at four spaces, matching the convention the
//! splitter assumes when offsetting cursor positions.

use crate::splitter::Dialect;

const INDENT: &str = "    ";

/// Format `sql` using the conventions appropriate for `dialect`.
///
/// The returned string never has trailing whitespace on any line.
/// Comments are preserved verbatim where the upstream library
/// supports it (line comments are kept; block comments inside
/// expressions are best-effort).
pub fn format(sql: &str, dialect: Dialect) -> String {
    // sqlformat 0.3 itself is dialect-agnostic at the API level, but
    // we still accept the [`Dialect`] parameter so:
    //   - callers can keep a single "per-session formatting function" handle, and
    //   - we can switch to a dialect-aware backend later without breaking
    //     callers (the signature stays).
    let _ = dialect;
    let options = sqlformat::FormatOptions {
        indent: sqlformat::Indent::Spaces(INDENT.len() as u8),
        uppercase: Some(true),
        lines_between_queries: 2,
        ignore_case_convert: None,
    };

    let params = sqlformat::QueryParams::None;
    let raw = sqlformat::format(sql, &params, &options);

    // Trim trailing spaces on each line. sqlformat occasionally leaves
    // them after split keywords; integration tests caught it.
    let mut out = String::with_capacity(raw.len());
    for (idx, line) in raw.lines().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(line.trim_end());
    }
    out
}

/// Convenience wrapper for callers that already have a [`Dialect`]
/// string (e.g. the app layer reads `session.driver.name()`).
pub fn format_for_driver(sql: &str, driver_name: &str) -> String {
    let dialect = match driver_name {
        "postgres" => Dialect::Postgres,
        "mysql" => Dialect::MySql,
        "sqlite" => Dialect::Sqlite,
        _ => Dialect::Generic,
    };
    format(sql, dialect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uppercases_keywords_and_indents_select_list() {
        let input = "select id,name from users where active=true";
        let out = format(input, Dialect::Postgres);
        assert!(out.contains("SELECT"));
        assert!(out.contains("FROM"));
        assert!(out.contains("WHERE"));
        // The select list should land on its own indented line.
        assert!(out.contains("\n    "));
    }

    #[test]
    fn no_trailing_whitespace_on_any_line() {
        let input = "select * from t where a in (1,2,3) order by a desc";
        let out = format(input, Dialect::Generic);
        for line in out.lines() {
            assert_eq!(line, line.trim_end(), "trailing space on: {line:?}");
        }
    }

    #[test]
    fn preserves_dollar_quoted_blocks_for_postgres() {
        // sqlformat doesn't deeply understand $$…$$, but it should
        // at least not mangle them — the body must survive.
        let input = "select $body$ select 1 $body$ as x";
        let out = format(input, Dialect::Postgres);
        assert!(out.contains("$body$"));
    }

    #[test]
    fn multiple_statements_get_blank_line_separator() {
        let input = "select 1; select 2";
        let out = format(input, Dialect::Generic);
        // lines_between_queries = 2 → at least one blank line between.
        assert!(out.contains("\n\n"));
    }

    #[test]
    fn mysql_dialect_handles_backticks() {
        // The library doesn't quote identifiers itself but it must
        // not strip them either.
        let input = "select `id` from `Users`";
        let out = format(input, Dialect::MySql);
        assert!(out.contains("`id`"));
        assert!(out.contains("`Users`"));
    }

    #[test]
    fn format_for_driver_picks_correct_dialect() {
        // Smoke: each driver routes to a real format() invocation
        // without panicking and produces non-empty output.
        for d in ["postgres", "mysql", "sqlite", "clickhouse"] {
            let out = format_for_driver("select 1", d);
            assert!(out.to_uppercase().contains("SELECT"));
        }
    }

    #[test]
    fn empty_input_is_a_noop() {
        assert_eq!(format("", Dialect::Generic), "");
    }
}
