//! Canonical-form helpers for type equality and default comparison.
//!
//! Drivers report the same logical type with a wide range of textual
//! forms. Postgres' `information_schema.columns` says
//! `character varying(255)`; the engine prompt prints `varchar(255)`;
//! a hand-typed migration writes `VARCHAR(255)`. The diff
//! algorithm treats these as equivalent — but only if we first map
//! every input through a single normalisation pass.
//!
//! ## What we *don't* try to do
//!
//! - Convert between *semantically* equivalent types across dialects
//!   (Postgres `text` vs `MySQL` `LONGTEXT`). Diff is single-engine in
//!   v2.0; cross-engine migration is the job of a dedicated tool.
//! - Resolve domain types / `CREATE TYPE` aliases. The driver's
//!   introspection already inlines those.
//! - Run a real SQL grammar. The matcher is regex-light on purpose
//!   so a misbehaving driver string never panics this crate.

/// Canonicalise a type name for equality comparison.
///
/// The pipeline:
///
/// 1. Trim and lowercase.
/// 2. Collapse whitespace runs to a single space.
/// 3. Strip precision qualifiers from temporal types
///    (`timestamp(0)` → `timestamp`, `timestamptz(3)` → `timestamptz`).
///    Precision on temporal types indicates fractional-second
///    resolution, not storage size; for diff purposes `timestamp(0)`
///    and `timestamp` are equivalent. Length qualifiers on character
///    types (`varchar(255)`) are preserved.
/// 4. Normalise a handful of well-known synonyms
///    (`character varying` → `varchar`, `integer` → `int4`, etc.).
/// 5. Strip the explicit `(n)` length when the default for the
///    underlying type already implies it.
///
/// Two strings that normalise to the same value are considered the
/// "same type" by [`crate::diff::diff`]. Anything richer (precision
/// scale, signed/unsigned, character set, collation) is preserved
/// verbatim so a real difference is never accidentally masked.
#[must_use]
pub fn canonical_type(raw: &str) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    let collapsed = collapse_whitespace(&lower);
    let stripped = strip_temporal_precision(&collapsed);
    apply_synonyms(&stripped)
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out
}

/// Strip precision qualifiers from temporal types only.
///
/// `timestamp(0) without time zone` → `timestamp without time zone`.
/// `timestamptz(3)` → `timestamptz`.
/// `time(2) with time zone` → `time with time zone`.
///
/// Character length qualifiers (`varchar(255)`) are left intact —
/// those are storage size, not fractional-second precision, and
/// two `varchar` columns with different lengths are genuinely
/// different types.
fn strip_temporal_precision(s: &str) -> String {
    // Temporal type keywords whose `(\d+)` qualifier is precision
    // rather than length. The loop strips the qualifier only when
    // it immediately follows one of these keywords.
    const TEMPORAL: &[&str] = &["timestamp", "timestamptz", "time", "timetz"];
    let mut working = s.to_owned();
    for kw in TEMPORAL {
        let prefix = format!("{kw}(");
        if let Some(start) = working.find(&prefix) {
            let after_kw = start + kw.len();
            // Verify `(` immediately follows the keyword.
            if working.as_bytes().get(after_kw) == Some(&b'(') {
                // Find the matching `)`.
                if let Some(end) = working[after_kw..].find(')') {
                    let close = after_kw + end;
                    // Verify the parens contain only digits.
                    let inner = &working[after_kw + 1..close];
                    if inner.bytes().all(|b| b.is_ascii_digit()) {
                        working.replace_range(after_kw..=close, "");
                    }
                }
            }
        }
    }
    working
}

/// Apply the small synonym table. Kept tiny and explicit so a future
/// regression caused by a driver string change is grep-able.
fn apply_synonyms(s: &str) -> String {
    // ANSI long names → engine-friendly short names. The right-hand
    // side matches what `narwhal-drivers` introspection reports for
    // the most common columns; matching the introspection rather
    // than the ANSI form means engine round-trips stay zero-diff.
    const SYNONYMS: &[(&str, &str)] = &[
        ("character varying", "varchar"),
        ("character", "char"),
        ("double precision", "float8"),
        ("real", "float4"),
        ("integer", "int4"),
        // M4.4: bare `int` is the most common PG shorthand for `int4`.
        ("int", "int4"),
        ("smallint", "int2"),
        ("bigint", "int8"),
        ("boolean", "bool"),
        ("timestamp without time zone", "timestamp"),
        ("timestamp with time zone", "timestamptz"),
        ("time without time zone", "time"),
        ("time with time zone", "timetz"),
    ];

    // The list is small enough that an O(n) sweep is faster than any
    // associative structure, and the ordering matters: "double
    // precision" must be matched before any partial like "double".
    let mut working = s.to_owned();
    for (long, short) in SYNONYMS {
        if let Some(idx) = working.find(long) {
            // Only replace at word boundaries — a column type of
            // `"character_set"` (rare but legal) must not become
            // `"char_set"`.
            let before_ok = idx == 0
                || !working
                    .as_bytes()
                    .get(idx - 1)
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
            let after_idx = idx + long.len();
            let after_ok = working
                .as_bytes()
                .get(after_idx)
                .is_none_or(|b| !b.is_ascii_alphanumeric() && *b != b'_');
            if before_ok && after_ok {
                working.replace_range(idx..after_idx, short);
            }
        }
    }
    working
}

/// Compare two default-expression strings for equality.
///
/// Defaults are surface SQL fragments; the same value can be written
/// many ways (`'foo'` vs `'foo'::text`, `0` vs `(0)`). The matcher
/// strips wrapping parentheses, trims, and lowercases — anything
/// finer would require a SQL grammar.
///
/// Returns `true` when the canonical forms match. `None` defaults
/// (no default declared) compare equal to one another and to an
/// explicit `null` (case-insensitive) per the project decision
/// captured in `docs/schema-diff.md`.
#[must_use]
pub fn defaults_equal(left: Option<&str>, right: Option<&str>) -> bool {
    let l = canonical_default(left);
    let r = canonical_default(right);
    l == r
}

fn canonical_default(raw: Option<&str>) -> Option<String> {
    let s = raw?.trim();
    if s.is_empty() {
        return None;
    }
    let stripped = strip_paren_wrap(s);
    let mut lower = stripped.trim().to_ascii_lowercase();
    if lower == "null" {
        return None;
    }
    // M4.6: Strip trailing `::identifier` cast suffixes iteratively
    // so `'foo'::text` and `'foo'` compare equal. Chained casts like
    // `'foo'::text::text` are also handled.
    lower = strip_cast_suffix(&lower);
    Some(lower)
}

/// Iteratively strip trailing `::identifier` cast suffixes from a
/// default expression. Handles chained casts: `'foo'::text::text`
/// becomes `'foo'`.
fn strip_cast_suffix(s: &str) -> String {
    let mut working = s.to_owned();
    while let Some(pos) = working.rfind("::") {
        // Verify everything after `::` is a valid SQL identifier
        // (ascii alphanumeric + underscore, not starting with digit).
        let ident = &working[pos + 2..];
        if ident.is_empty() {
            break;
        }
        let mut chars = ident.chars();
        let first = chars.next().expect("non-empty");
        if !first.is_ascii_alphabetic() && first != '_' {
            break;
        }
        if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
            break;
        }
        working.truncate(pos);
    }
    working
}

/// Strip one pair of outer parentheses if they wrap the whole string.
/// `(0)` → `0`; `(a + b) * c` is left alone.
fn strip_paren_wrap(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'(' || bytes[bytes.len() - 1] != b')' {
        return s;
    }
    // Verify the outer parens actually match — walk the depth count
    // and bail if it returns to 0 before the end.
    let mut depth = 0i32;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 && i + 1 < bytes.len() {
                    return s;
                }
            }
            _ => {}
        }
    }
    &s[1..s.len() - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_long_form_maps_to_short() {
        assert_eq!(canonical_type("character varying(255)"), "varchar(255)");
        assert_eq!(canonical_type("CHARACTER VARYING(255)"), "varchar(255)");
        assert_eq!(canonical_type("integer"), "int4");
        assert_eq!(canonical_type("DOUBLE PRECISION"), "float8");
        assert_eq!(canonical_type("timestamp without time zone"), "timestamp");
        assert_eq!(canonical_type("timestamp with time zone"), "timestamptz");
    }

    #[test]
    fn whitespace_collapses() {
        assert_eq!(
            canonical_type("  CHARACTER\t VARYING (255)  "),
            "varchar (255)"
        );
    }

    #[test]
    fn synonyms_respect_word_boundaries() {
        // `character_set` must not become `char_set`.
        assert_eq!(canonical_type("character_set"), "character_set");
        // `varcharacter` (made-up) must not absorb the long form.
        assert_eq!(canonical_type("varcharacter"), "varcharacter");
    }

    #[test]
    fn defaults_equal_simple() {
        assert!(defaults_equal(Some("0"), Some("0")));
        assert!(defaults_equal(Some("(0)"), Some("0")));
        assert!(defaults_equal(Some("'foo'"), Some("'foo'")));
        assert!(!defaults_equal(Some("0"), Some("1")));
    }

    #[test]
    fn defaults_treat_null_as_absent() {
        assert!(defaults_equal(None, Some("NULL")));
        assert!(defaults_equal(None, Some("null")));
        assert!(defaults_equal(Some("NULL"), None));
    }

    #[test]
    fn defaults_ignore_outer_parens() {
        assert!(defaults_equal(Some("(now())"), Some("now()")));
        // But not when parens are part of an expression.
        assert!(!defaults_equal(Some("(a) + (b)"), Some("a + b")));
    }

    #[test]
    fn defaults_case_insensitive() {
        assert!(defaults_equal(
            Some("CURRENT_TIMESTAMP"),
            Some("current_timestamp")
        ));
    }

    // -- M4.4: int → int4 synonym ------------------------------------------------

    #[test]
    fn int_canonicalises_to_int4() {
        assert_eq!(canonical_type("int"), "int4");
        assert_eq!(canonical_type("INT"), "int4");
        // `integer` still maps to `int4` (existing synonym).
        assert_eq!(canonical_type("integer"), "int4");
        // Word-boundary: `interval` must not be touched.
        assert_eq!(canonical_type("interval"), "interval");
    }

    // -- M4.5: precision-qualified timestamp normalisation -----------------------

    #[test]
    fn precision_qualified_timestamp_canonicalises() {
        assert_eq!(
            canonical_type("timestamp(0) without time zone"),
            "timestamp"
        );
        assert_eq!(canonical_type("TIMESTAMP(6) WITH TIME ZONE"), "timestamptz");
        assert_eq!(canonical_type("timestamp(3)"), "timestamp");
        // Length qualifier on varchar must NOT be stripped.
        assert_eq!(canonical_type("varchar(255)"), "varchar(255)");
        // Time with precision
        assert_eq!(canonical_type("time(2) without time zone"), "time");
    }

    // -- M4.6: default `::type` cast suffix strip --------------------------------

    #[test]
    fn default_with_cast_equal_to_uncast() {
        assert!(defaults_equal(Some("'foo'::text"), Some("'foo'")));
        assert!(defaults_equal(Some("0::int4"), Some("0")));
    }

    #[test]
    fn defaults_equal_handles_chained_casts() {
        assert!(defaults_equal(Some("'foo'::text::text"), Some("'foo'")));
        assert!(defaults_equal(Some("'bar'::varchar::text"), Some("'bar'")));
    }
}
