//! Padded ASCII table writer.

use std::io::Write;

use narwhal_core::{ColumnHeader, Row, Value};

use super::error::ExportError;

pub(super) fn write_table<W: Write>(
    writer: &mut W,
    columns: &[ColumnHeader],
    rows: &[Row],
) -> Result<(), ExportError> {
    if columns.is_empty() {
        return Ok(());
    }

    // Pre-render every cell once so we can compute the max width per
    // column without paying the `Display` cost twice. Memory cost is
    // (cells × average string width); for million-cell sets the user
    // should choose CSV/JSON/TSV instead.
    let mut rendered: Vec<Vec<String>> = Vec::with_capacity(rows.len());
    let mut widths: Vec<usize> = columns.iter().map(|c| c.name.chars().count()).collect();
    for row in rows {
        let mut cells = Vec::with_capacity(columns.len());
        for (i, value) in row.0.iter().enumerate() {
            let s = match value {
                Value::Null => String::new(),
                other => other.render(),
            };
            if i < widths.len() {
                widths[i] = widths[i].max(s.chars().count());
            }
            cells.push(s);
        }
        rendered.push(cells);
    }

    // Top border
    write_table_border(writer, &widths)?;
    // Header
    write_table_row(writer, &widths, columns.iter().map(|c| c.name.as_str()))?;
    // Header/data separator
    write_table_border(writer, &widths)?;
    // Data rows
    for cells in &rendered {
        write_table_row(writer, &widths, cells.iter().map(String::as_str))?;
    }
    // Bottom border
    write_table_border(writer, &widths)?;
    Ok(())
}

fn write_table_border<W: Write>(writer: &mut W, widths: &[usize]) -> Result<(), ExportError> {
    writer.write_all(b"+")?;
    for &w in widths {
        for _ in 0..(w + 2) {
            writer.write_all(b"-")?;
        }
        writer.write_all(b"+")?;
    }
    writer.write_all(b"\n")?;
    Ok(())
}

fn write_table_row<'a, W, I>(writer: &mut W, widths: &[usize], cells: I) -> Result<(), ExportError>
where
    W: Write,
    I: IntoIterator<Item = &'a str>,
{
    writer.write_all(b"|")?;
    for (i, cell) in cells.into_iter().enumerate() {
        let target = widths.get(i).copied().unwrap_or(0);
        let width = cell.chars().count();
        writer.write_all(b" ")?;
        writer.write_all(cell.as_bytes())?;
        for _ in width..target {
            writer.write_all(b" ")?;
        }
        writer.write_all(b" |")?;
    }
    writer.write_all(b"\n")?;
    Ok(())
}

