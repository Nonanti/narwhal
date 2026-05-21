//! ClickHouse type system bridge and SQL literal rendering.
//!
//! ClickHouse returns column metadata in the
//! `TabSeparatedWithNamesAndTypes` wire format where each value is a
//! tab-delimited field and the first two rows carry column names and
//! native type strings respectively. This module:
//!
//! * maps ClickHouse type strings (`UInt32`, `Nullable(String)`,
//!   `Array(Int64)`, …) to [`narwhal_core::Value`] variants for display;
//! * renders [`narwhal_core::Value`] as safe SQL literals for the HTTP
//!   query parameter substitution path;
//! * parses TSV response bodies into rows of [`narwhal_core::Value`].
//!
//! # Byte-accurate TSV parsing
//!
//! ClickHouse's `String` type is byte-oriented — it stores any byte
//! sequence, not just UTF-8 text. The TSV parser therefore works on
//! `&[u8]` throughout the data path, decoding ClickHouse's TSV escape
//! sequences (`\b \f \n \r \t \0 \\ \'`) and routing invalid-UTF-8
//! payloads into [`Value::Bytes`] instead of silently replacing them
//! with `U+FFFD`. Only header lines (column names and type strings,
//! which are always ASCII identifiers) pass through UTF-8 conversion.

use narwhal_core::Value;

/// Classify a ClickHouse type string into the [`Value`] variant that
/// should hold it.
///
/// Composite and exotic types (Array, Map, Tuple, Nested, LowCardinality,
/// AggregateFunction, …) collapse to `Value::String` — the TSV wire
/// representation is already human-readable and round-tripping structured
/// types is out of scope for a TUI client.
pub(crate) fn classify_type(ch_type: &str) -> ValueKind {
    let ch_type = ch_type.trim();

    // Strip Nullable(…) and LowCardinality(…) wrappers — the inner type
    // determines the variant.
    let inner = strip_wrappers(ch_type);

    // 128/256-bit integers don't fit in i64 → String.
    if inner.starts_with("Int128")
        || inner.starts_with("Int256")
        || inner.starts_with("UInt128")
        || inner.starts_with("UInt256")
    {
        return ValueKind::String;
    }
    if inner.starts_with("Int8")
        || inner.starts_with("Int16")
        || inner.starts_with("Int32")
        || inner.starts_with("Int64")
        || inner.starts_with("UInt8")
        || inner.starts_with("UInt16")
        || inner.starts_with("UInt32")
        || inner.starts_with("UInt64")
    {
        return ValueKind::Int;
    }
    if inner == "Float32" || inner == "Float64" {
        return ValueKind::Float;
    }
    if inner == "String" || inner.starts_with("FixedString(") {
        return ValueKind::String;
    }
    if inner == "UUID" {
        return ValueKind::Uuid;
    }
    if inner == "Bool" {
        return ValueKind::Bool;
    }
    if inner == "Date"
        || inner == "Date32"
        || inner.starts_with("DateTime")
        || inner.starts_with("DateTime64")
    {
        return ValueKind::String;
    }
    // Decimal, Enum, IPv4, IPv6, Array, Map, Tuple, Nested, etc.
    ValueKind::String
}

/// Simplified classification result used to decide which [`Value`] variant
/// to produce when parsing a TSV field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValueKind {
    Int,
    Float,
    Bool,
    Uuid,
    String,
}

/// Strip `Nullable(…)` and `LowCardinality(…)` wrappers, returning the
/// innermost type string. If no wrapper is present, returns `ty` unchanged.
fn strip_wrappers(ty: &str) -> &str {
    let mut current = ty;
    loop {
        let stripped = if let Some(rest) = current.strip_prefix("Nullable(") {
            rest.strip_suffix(')').unwrap_or(rest)
        } else if let Some(rest) = current.strip_prefix("LowCardinality(") {
            rest.strip_suffix(')').unwrap_or(rest)
        } else {
            break;
        };
        current = stripped;
    }
    current
}

/// Decode ClickHouse TSV escape sequences in a string-typed field.
/// Returns the decoded bytes (which may not be valid UTF-8).
///
/// ClickHouse escapes `\b \f \n \r \t \0 \\ \'` in TSV string cells.
/// Any other byte is passed through unchanged — including bytes that
/// are not valid UTF-8.
fn decode_tsv_string_bytes(field: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        if field[i] == b'\\' && i + 1 < field.len() {
            let next = field[i + 1];
            let decoded = match next {
                b'b' => Some(0x08),
                b'f' => Some(0x0C),
                b'n' => Some(b'\n'),
                b'r' => Some(b'\r'),
                b't' => Some(b'\t'),
                b'0' => Some(0x00),
                b'\\' => Some(b'\\'),
                b'\'' => Some(b'\''),
                _ => None,
            };
            if let Some(byte) = decoded {
                out.push(byte);
                i += 2;
                continue;
            }
        }
        out.push(field[i]);
        i += 1;
    }
    out
}

/// Parse a single TSV field according to the ClickHouse type string.
///
/// `\\N` (literal backslash-N) is ClickHouse's NULL representation in
/// TSV format. Empty strings in non-Nullable columns are preserved as
/// `Value::String("")`.
///
/// Takes `&[u8]` because ClickHouse's `String` type is byte-oriented —
/// it may contain arbitrary bytes that are not valid UTF-8. String-typed
/// fields are decoded via [`decode_tsv_string_bytes`] and then routed to
/// [`Value::String`] if the decoded bytes are valid UTF-8, or
/// [`Value::Bytes`] otherwise. Numeric/Bool/Uuid fields use strict
/// `std::str::from_utf8` because those types are always ASCII on the
/// wire; invalid UTF-8 there produces [`Value::Unknown`].
pub(crate) fn parse_tsv_value(raw: &[u8], ch_type: &str) -> Value {
    // ClickHouse represents NULL as the two-byte sequence \N in TSV.
    // This check must happen before escape decoding because \N is not
    // an escaped byte — it is a literal backslash followed by 'N'.
    if raw == b"\\N" {
        return Value::Null;
    }

    match classify_type(ch_type) {
        ValueKind::Int => {
            if raw.is_empty() {
                return Value::Null;
            }
            match std::str::from_utf8(raw) {
                Ok(s) => match s.parse::<i64>() {
                    Ok(v) => Value::Int(v),
                    Err(_) => Value::Unknown(s.to_owned()),
                },
                Err(_) => Value::Unknown(String::from_utf8_lossy(raw).into_owned()),
            }
        }
        ValueKind::Float => {
            if raw.is_empty() {
                return Value::Null;
            }
            match std::str::from_utf8(raw) {
                Ok(s) => match s.parse::<f64>() {
                    Ok(v) => Value::Float(v),
                    Err(_) => Value::Unknown(s.to_owned()),
                },
                Err(_) => Value::Unknown(String::from_utf8_lossy(raw).into_owned()),
            }
        }
        ValueKind::Bool => match raw {
            b"1" | b"true" => Value::Bool(true),
            b"0" | b"false" => Value::Bool(false),
            b"" => Value::Null,
            other => match std::str::from_utf8(other) {
                Ok(s) => Value::Unknown(s.to_owned()),
                Err(_) => Value::Unknown(String::from_utf8_lossy(other).into_owned()),
            },
        },
        ValueKind::Uuid => {
            if raw.is_empty() {
                return Value::Null;
            }
            match std::str::from_utf8(raw) {
                Ok(s) => match s.parse::<uuid::Uuid>() {
                    Ok(u) => Value::Uuid(u),
                    Err(_) => Value::String(s.to_owned()),
                },
                Err(_) => Value::Unknown(String::from_utf8_lossy(raw).into_owned()),
            }
        }
        ValueKind::String => {
            if raw.is_empty() && is_nullable_type(ch_type) {
                Value::Null
            } else {
                let decoded = decode_tsv_string_bytes(raw);
                match String::from_utf8(decoded) {
                    Ok(s) => Value::String(s),
                    Err(e) => Value::Bytes(e.into_bytes()),
                }
            }
        }
    }
}

/// Quick check: does the ClickHouse type string start with `Nullable(`?
fn is_nullable_type(ch_type: &str) -> bool {
    ch_type.trim().starts_with("Nullable(")
}

/// Escape a string for embedding in a single-quoted ClickHouse SQL literal.
///
/// ClickHouse honours backslash escapes inside string literals, so both
/// backslash and single-quote must be escaped. Backslash is doubled
/// (`\\` → `\\\\`) and single-quote is doubled (`'` → `''`).
pub(crate) fn escape_sql_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("''"),
            other => out.push(other),
        }
    }
    out
}

/// Render a [`Value`] as a SQL literal safe for embedding in a ClickHouse
/// HTTP query string.
///
/// **Security**: String values are single-quoted with interior quotes
/// escaped by doubling (`'` → `''`). Byte arrays are rendered as hex
/// literals. All other variants use their natural SQL representation.
/// This function must **never** produce an unescaped interpolation.
pub(crate) fn value_to_sql_literal(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_owned(),
        Value::Bool(b) => {
            if *b {
                "1".to_owned()
            } else {
                "0".to_owned()
            }
        }
        Value::Int(i) => i.to_string(),
        Value::Float(f) => {
            let s = f.to_string();
            // Ensure the float literal has a decimal point so ClickHouse
            // does not interpret it as an integer.
            if s.contains('.') || s.contains('e') || s.contains('E') {
                s
            } else {
                format!("{s}.0")
            }
        }
        Value::String(s) => format!("'{}'", escape_sql_string(s)),
        Value::Bytes(b) => {
            let hex: String = b.iter().map(|byte| format!("{byte:02x}")).collect();
            format!("unhex('{hex}')")
        }
        Value::Date(d) => format!("'{d}'"),
        Value::Time(t) => format!("'{t}'"),
        Value::DateTime(dt) => format!("'{dt}'"),
        Value::Timestamp(ts) => format!("'{}'", ts.to_rfc3339()),
        Value::Uuid(u) => format!("'{u}'"),
        Value::Json(v) => format!("'{}'", escape_sql_string(&v.to_string())),
        Value::Unknown(s) => format!("'{}'", escape_sql_string(s)),
    }
}

/// Split a byte body into lines, handling both `\n` and `\r\n` line
/// endings. Returns slices into the original body; no copying.
fn split_lines(body: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &b) in body.iter().enumerate() {
        if b == b'\n' {
            let mut end = i;
            if end > start && body[end - 1] == b'\r' {
                end -= 1;
            }
            out.push(&body[start..end]);
            start = i + 1;
        }
    }
    if start < body.len() {
        // Trailing line without LF.
        let mut end = body.len();
        if end > start && body[end - 1] == b'\r' {
            end -= 1;
        }
        out.push(&body[start..end]);
    }
    out
}

/// Parse a complete TSV response body in `TabSeparatedWithNamesAndTypes`
/// format.
///
/// Returns `(headers, type_strings, rows)` where `headers` has the column
/// names, `type_strings` has the ClickHouse native types, and `rows` has
/// the parsed [`Value`] rows.
///
/// Takes `&[u8]` because data cells may contain arbitrary bytes (the
/// ClickHouse `String` type is byte-oriented). Header lines (column
/// names and type strings) are always ASCII identifiers on the wire, so
/// they are converted to `String` via `from_utf8_lossy` defensively.
pub(crate) fn parse_tsv_body(body: &[u8]) -> (Vec<String>, Vec<String>, Vec<Vec<Value>>) {
    let lines = split_lines(body);
    let mut lines_iter = lines.iter().peekable();

    // First line: column names (tab-separated).
    let header_line = match lines_iter.next() {
        Some(l) => *l,
        None => return (Vec::new(), Vec::new(), Vec::new()),
    };
    let headers: Vec<String> = header_line
        .split(|&b| b == b'\t')
        .map(|field| String::from_utf8_lossy(field).into_owned())
        .collect();

    // Second line: type strings.
    let type_line = match lines_iter.next() {
        Some(l) => *l,
        None => return (headers, Vec::new(), Vec::new()),
    };
    let type_strings: Vec<String> = type_line
        .split(|&b| b == b'\t')
        .map(|field| String::from_utf8_lossy(field).into_owned())
        .collect();

    let mut rows = Vec::new();
    for line in lines_iter {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&[u8]> = line.split(|&b| b == b'\t').collect();
        let mut row = Vec::with_capacity(headers.len());
        for (i, field) in fields.iter().enumerate() {
            let ch_type = type_strings.get(i).map(String::as_str).unwrap_or("String");
            row.push(parse_tsv_value(field, ch_type));
        }
        // Pad missing fields with Null.
        while row.len() < headers.len() {
            row.push(Value::Null);
        }
        rows.push(row);
    }

    (headers, type_strings, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- classify_type ----

    #[test]
    fn classify_integer_types() {
        assert_eq!(classify_type("UInt8"), ValueKind::Int);
        assert_eq!(classify_type("UInt16"), ValueKind::Int);
        assert_eq!(classify_type("UInt32"), ValueKind::Int);
        assert_eq!(classify_type("UInt64"), ValueKind::Int);
        assert_eq!(classify_type("Int8"), ValueKind::Int);
        assert_eq!(classify_type("Int16"), ValueKind::Int);
        assert_eq!(classify_type("Int32"), ValueKind::Int);
        assert_eq!(classify_type("Int64"), ValueKind::Int);
    }

    #[test]
    fn classify_oversized_ints_are_strings() {
        assert_eq!(classify_type("UInt128"), ValueKind::String);
        assert_eq!(classify_type("UInt256"), ValueKind::String);
        assert_eq!(classify_type("Int128"), ValueKind::String);
        assert_eq!(classify_type("Int256"), ValueKind::String);
    }

    #[test]
    fn classify_float_types() {
        assert_eq!(classify_type("Float32"), ValueKind::Float);
        assert_eq!(classify_type("Float64"), ValueKind::Float);
    }

    #[test]
    fn classify_string_types() {
        assert_eq!(classify_type("String"), ValueKind::String);
        assert_eq!(classify_type("FixedString(16)"), ValueKind::String);
    }

    #[test]
    fn classify_uuid() {
        assert_eq!(classify_type("UUID"), ValueKind::Uuid);
        assert_eq!(classify_type("Nullable(UUID)"), ValueKind::Uuid);
    }

    #[test]
    fn classify_bool() {
        assert_eq!(classify_type("Bool"), ValueKind::Bool);
    }

    #[test]
    fn classify_datetime() {
        assert_eq!(classify_type("DateTime('UTC')"), ValueKind::String);
        assert_eq!(classify_type("DateTime64(3)"), ValueKind::String);
        assert_eq!(classify_type("Date"), ValueKind::String);
        assert_eq!(classify_type("Date32"), ValueKind::String);
    }

    #[test]
    fn classify_nullable_and_lowcardinality() {
        assert_eq!(classify_type("Nullable(String)"), ValueKind::String);
        assert_eq!(classify_type("Nullable(Int32)"), ValueKind::Int);
        assert_eq!(classify_type("LowCardinality(String)"), ValueKind::String);
        assert_eq!(
            classify_type("Nullable(LowCardinality(String))"),
            ValueKind::String
        );
    }

    #[test]
    fn classify_complex_types() {
        assert_eq!(classify_type("Array(Int64)"), ValueKind::String);
        assert_eq!(classify_type("Map(String, Int64)"), ValueKind::String);
        assert_eq!(
            classify_type("Tuple(String, Int64, Float64)"),
            ValueKind::String
        );
        assert_eq!(classify_type("Decimal(18, 3)"), ValueKind::String);
        assert_eq!(classify_type("IPv4"), ValueKind::String);
        assert_eq!(classify_type("IPv6"), ValueKind::String);
    }

    // ---- parse_tsv_value ----

    #[test]
    fn parse_null_value() {
        assert!(matches!(
            parse_tsv_value(b"\\N", "Nullable(Int32)"),
            Value::Null
        ));
    }

    #[test]
    fn parse_int_value() {
        let v = parse_tsv_value(b"42", "UInt32");
        assert!(matches!(v, Value::Int(42)));
        assert_eq!(v.render(), "42");

        let v2 = parse_tsv_value(b"-7", "Int32");
        assert!(matches!(v2, Value::Int(-7)));
    }

    #[test]
    fn parse_float_value() {
        let v = parse_tsv_value(b"3.14", "Float64");
        assert!(matches!(v, Value::Float(_)));
        assert_eq!(v.render(), "3.14");
    }

    #[test]
    fn parse_bool_value() {
        assert!(matches!(parse_tsv_value(b"1", "Bool"), Value::Bool(true)));
        assert!(matches!(parse_tsv_value(b"0", "Bool"), Value::Bool(false)));
    }

    #[test]
    fn parse_uuid_value() {
        let uuid_str = b"550e8400-e29b-41d4-a716-446655440000";
        let parsed = parse_tsv_value(uuid_str, "UUID");
        assert!(matches!(parsed, Value::Uuid(_)));
    }

    #[test]
    fn parse_uuid_fallback_to_string() {
        let parsed = parse_tsv_value(b"not-a-uuid", "UUID");
        assert!(matches!(parsed, Value::String(_)));
    }

    #[test]
    fn parse_string_value() {
        let v = parse_tsv_value(b"hello world", "String");
        assert!(matches!(v, Value::String(_)));
        assert_eq!(v.render(), "hello world");
    }

    // ---- value_to_sql_literal ----

    #[test]
    fn sql_literal_string_escapes_quotes() {
        assert_eq!(
            value_to_sql_literal(&Value::String("it's here".into())),
            "'it''s here'"
        );
    }

    #[test]
    fn sql_literal_string_escapes_backslash() {
        assert_eq!(
            value_to_sql_literal(&Value::String(r"C:\Users".into())),
            r"'C:\\Users'"
        );
    }

    #[test]
    fn sql_literal_string_escapes_trailing_backslash() {
        assert_eq!(
            value_to_sql_literal(&Value::String(r"path\".into())),
            r"'path\\'"
        );
    }

    #[test]
    fn sql_literal_unknown_escapes_backslash() {
        assert_eq!(
            value_to_sql_literal(&Value::Unknown(r"x\y".into())),
            r"'x\\y'"
        );
    }

    #[test]
    fn sql_literal_null() {
        assert_eq!(value_to_sql_literal(&Value::Null), "NULL");
    }

    #[test]
    fn sql_literal_bool() {
        assert_eq!(value_to_sql_literal(&Value::Bool(true)), "1");
        assert_eq!(value_to_sql_literal(&Value::Bool(false)), "0");
    }

    #[test]
    fn sql_literal_int() {
        assert_eq!(value_to_sql_literal(&Value::Int(42)), "42");
    }

    #[test]
    fn sql_literal_float_ensures_decimal() {
        let result = value_to_sql_literal(&Value::Float(3.0));
        assert!(
            result.contains('.') || result.contains('e') || result.contains('E'),
            "float literal must contain a decimal point or exponent: got {result}"
        );
    }

    #[test]
    fn sql_literal_float_nan_renders_as_function() {
        let result = value_to_sql_literal(&Value::Float(f64::NAN));
        assert_eq!(result, "nan()");
    }

    #[test]
    fn sql_literal_float_inf_renders_as_function() {
        let result = value_to_sql_literal(&Value::Float(f64::INFINITY));
        assert_eq!(result, "inf()");
    }

    #[test]
    fn sql_literal_float_neg_inf_renders_with_sign() {
        let result = value_to_sql_literal(&Value::Float(f64::NEG_INFINITY));
        assert_eq!(result, "-inf()");
    }

    #[test]
    fn sql_literal_bytes_hex() {
        let result = value_to_sql_literal(&Value::Bytes(vec![0xDE, 0xAD]));
        assert!(result.starts_with("unhex('"));
        assert!(result.contains("dead"));
    }

    // ---- parse_tsv_body ----

    #[test]
    fn parse_full_tsv_body() {
        let body = b"id\tname\tactive\nUInt32\tString\tBool\n1\talice\t1\n2\tbob\t0";
        let (headers, types, rows) = parse_tsv_body(body);
        assert_eq!(headers, vec!["id", "name", "active"]);
        assert_eq!(types, vec!["UInt32", "String", "Bool"]);
        assert_eq!(rows.len(), 2);
        assert!(matches!(rows[0][0], Value::Int(1)));
        assert!(matches!(rows[0][1], Value::String(_)));
        assert!(matches!(rows[0][2], Value::Bool(true)));
        assert!(matches!(rows[1][0], Value::Int(2)));
        assert!(matches!(rows[1][2], Value::Bool(false)));
    }

    #[test]
    fn parse_tsv_body_with_null() {
        let body = b"id\tname\nUInt32\tNullable(String)\n1\t\\N";
        let (_, _, rows) = parse_tsv_body(body);
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0][0], Value::Int(1)));
        assert!(matches!(rows[0][1], Value::Null));
    }

    // ---- TSV escape decoding and byte preservation ----

    #[test]
    fn parse_tsv_escape_decoded_string() {
        // Payload contains `line1\\nline2` (six characters on the wire),
        // should decode to "line1\nline2" (two lines, 11 bytes).
        let v = parse_tsv_value(b"line1\\nline2", "String");
        match &v {
            Value::String(s) => assert_eq!(s, "line1\nline2"),
            other => panic!("expected Value::String, got {other:?}"),
        }
    }

    #[test]
    fn parse_tsv_string_preserves_invalid_utf8() {
        // A single byte 0xFF is not valid UTF-8; should become Value::Bytes.
        let v = parse_tsv_value(&[0xFF], "String");
        match &v {
            Value::Bytes(b) => assert_eq!(b, &vec![0xFF]),
            other => panic!("expected Value::Bytes, got {other:?}"),
        }
    }

    #[test]
    fn parse_tsv_string_decodes_all_known_escapes() {
        // All known ClickHouse TSV escapes in one field.
        let input = b"\\b\\f\\n\\r\\t\\0\\\\\\'";
        let v = parse_tsv_value(input, "String");
        // The decoded bytes are [0x08, 0x0C, 0x0A, 0x0D, 0x09, 0x00, 0x5C, 0x27].
        // NUL (0x00) is valid UTF-8 (U+0000), so String::from_utf8
        // succeeds and we get Value::String.
        match &v {
            Value::String(s) => {
                assert_eq!(
                    s.as_bytes(),
                    &[0x08, 0x0C, 0x0A, 0x0D, 0x09, 0x00, 0x5C, 0x27]
                );
            }
            other => panic!("expected Value::String, got {other:?}"),
        }
    }

    #[test]
    fn parse_tsv_string_preserves_unknown_backslash_sequences() {
        // `\x` is not a known escape; both bytes pass through unchanged.
        let v = parse_tsv_value(b"\\x", "String");
        match &v {
            Value::String(s) => assert_eq!(s, "\\x"),
            other => panic!("expected Value::String, got {other:?}"),
        }
    }

    // ---- statement_returns_rows (in lib.rs) ----

    #[test]
    fn row_returning_keywords() {
        assert!(crate::statement_returns_rows("SELECT 1"));
        assert!(crate::statement_returns_rows(
            "  with cte as (select 1) select * from cte"
        ));
        assert!(crate::statement_returns_rows("SHOW TABLES"));
        assert!(crate::statement_returns_rows("DESCRIBE TABLE t"));
        assert!(crate::statement_returns_rows("EXPLAIN SELECT 1"));
    }

    #[test]
    fn non_row_returning_keywords() {
        assert!(!crate::statement_returns_rows("INSERT INTO t VALUES (1)"));
        assert!(!crate::statement_returns_rows("CREATE TABLE t (id Int32)"));
        assert!(!crate::statement_returns_rows("DROP TABLE t"));
        assert!(!crate::statement_returns_rows(
            "ALTER TABLE t ADD COLUMN x String"
        ));
    }
}
