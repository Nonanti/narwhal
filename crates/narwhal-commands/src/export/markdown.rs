//! GitHub-Flavoured Markdown table writer (T1-T4-B).
//!
//! GFM tables are intentionally simple: a header row, a separator
//! row that doubles as alignment metadata (`:---`, `:---:`, `---:`),
//! then one row per record. Cell content cannot contain literal `|`
//! or newlines, so we escape both. Column alignment is inferred from
//! the data type: numeric columns get right-aligned, everything else
//! left-aligned.
//!
//! Memory model: like the other writers in this module, the input is
//! a fully-materialised `&[Row]`. Streaming Markdown is not useful
//! (you can't render half a table) so there is no Tier 2 follow-up
//! planned.

use std::io::Write;

use narwhal_core::{ColumnHeader, Row, Value};

use super::error::ExportError;
use super::format::MarkdownOptions;

pub(super) fn write_markdown<W: Write>(
    writer: &mut W,
    columns: &[ColumnHeader],
    rows: &[Row],
    options: MarkdownOptions,
) -> Result<(), ExportError> {
    // The empty-columns case can come from a `:export markdown` issued
    // on a "no result" tab. Emit a single italic line so the file is
    // valid Markdown rather than zero bytes (which renders as nothing
    // and looks like a write failure).
    if columns.is_empty() {
        writer.write_all(b"_no result to export_\n")?;
        return Ok(());
    }

    let alignments: Vec<Alignment> = columns.iter().map(infer_alignment).collect();

    // Header row.
    writer.write_all(b"|")?;
    for column in columns {
        writer.write_all(b" ")?;
        write_escaped(writer, &column.name)?;
        writer.write_all(b" |")?;
    }
    writer.write_all(b"\n")?;

    // Separator + alignment row. GFM requires at least three dashes
    // per column; we always emit `---` (no padding) which renders
    // identically in every major renderer.
    writer.write_all(b"|")?;
    for align in &alignments {
        match align {
            Alignment::Left => writer.write_all(b" :--- |")?,
            Alignment::Right => writer.write_all(b" ---: |")?,
        }
    }
    writer.write_all(b"\n")?;

    let (visible, truncated) = match options.row_limit {
        Some(limit) if rows.len() > limit => (&rows[..limit], Some(rows.len() - limit)),
        _ => (rows, None),
    };

    for row in visible {
        writer.write_all(b"|")?;
        for value in &row.0 {
            writer.write_all(b" ")?;
            write_cell(writer, value)?;
            writer.write_all(b" |")?;
        }
        writer.write_all(b"\n")?;
    }

    if let Some(omitted) = truncated {
        // Trailing italic line matches the brief; the underscore is
        // GFM emphasis. `(s)` keeps the message grammatical for the
        // n == 1 edge case.
        writeln!(
            writer,
            "\n_…{omitted} more row{s} truncated_",
            s = if omitted == 1 { "" } else { "s" }
        )?;
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Alignment {
    Left,
    Right,
}

/// Numeric SQL types get right-aligned so columns of integers line up
/// like a spreadsheet. Everything else (including JSON, dates, uuid)
/// stays left-aligned because right-aligning multi-line / variable
/// content looks worse than the default.
fn infer_alignment(column: &ColumnHeader) -> Alignment {
    let lower = column.data_type.to_ascii_lowercase();
    // Crude but effective: the SQL standard numeric types all contain
    // one of these substrings. We intentionally do NOT right-align
    // boolean (TRUE/FALSE is text-ish) or timestamp (right-aligned
    // dates look weird next to a left-aligned uuid).
    const NUMERIC_HINTS: &[&str] = &[
        "int", "decimal", "numeric", "real", "float", "double", "money", "serial",
    ];
    if NUMERIC_HINTS.iter().any(|hint| lower.contains(hint)) {
        Alignment::Right
    } else {
        Alignment::Left
    }
}

fn write_cell<W: Write>(writer: &mut W, value: &Value) -> Result<(), ExportError> {
    match value {
        Value::Null => {
            // GFM has no first-class NULL; `(null)` is the convention
            // psql + DBeaver Markdown export agree on. Plain text so
            // it gets the same alignment as the surrounding column.
            writer.write_all(b"(null)")?;
        }
        Value::Bytes(b) => {
            // Render bytes as a hint rather than dumping garbage into
            // a Markdown cell where any 0x7c would break the table.
            write!(writer, "&lt;{} bytes&gt;", b.len())?;
        }
        other => write_escaped(writer, &other.render())?,
    }
    Ok(())
}

/// Escape every character that would break the GFM table parse:
///
/// - `|` → `\|` (column separator)
/// - `\n`, `\r` → `<br>` (rows are line-delimited)
/// - `\\` → `\\\\` so the backslash escape we just emitted is not
///   itself swallowed by a later parse pass
/// - leading whitespace is preserved (some renderers eat it, but the
///   round-trip cost of `&nbsp;`-ing every cell isn't worth it)
fn write_escaped<W: Write>(writer: &mut W, text: &str) -> Result<(), ExportError> {
    for ch in text.chars() {
        match ch {
            '\\' => writer.write_all(br"\\")?,
            '|' => writer.write_all(br"\|")?,
            '\n' | '\r' => writer.write_all(b"<br>")?,
            // Backticks survive: GFM treats them inside table cells as
            // inline code spans, which is what the user wants when a
            // cell contains literal code.
            other => {
                let mut buf = [0u8; 4];
                writer.write_all(other.encode_utf8(&mut buf).as_bytes())?;
            }
        }
    }
    Ok(())
}
