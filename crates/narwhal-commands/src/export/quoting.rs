//! Identifier quoting helpers shared by INSERT and source extraction.

use std::io::Write;

use super::error::ExportError;

pub(super) fn write_quoted_sql_string<W: Write>(writer: &mut W, s: &str) -> Result<(), ExportError> {
    writer.write_all(b"'")?;
    for ch in s.chars() {
        if ch == '\'' {
            writer.write_all(b"''")?;
        } else {
            let mut buf = [0u8; 4];
            writer.write_all(ch.encode_utf8(&mut buf).as_bytes())?;
        }
    }
    writer.write_all(b"'")?;
    Ok(())
}

/// Double-quote a SQL identifier, escaping embedded double quotes by
/// doubling them (`"` → `""`). Always quotes unconditionally so that
/// reserved words like `order` or `from` are safe.
pub(super) fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Strip surrounding double quotes from a SQL identifier and unescape
/// doubled quotes (`""` → `"`).
pub(super) fn unquote_ident(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1].replace("\"\"", "\"")
    } else {
        s.to_owned()
    }
}

