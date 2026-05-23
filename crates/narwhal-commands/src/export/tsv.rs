//! Tab-separated values writer.

use std::io::Write;

use narwhal_core::{ColumnHeader, Row, Value};

use super::error::ExportError;

pub(super) fn write_tsv<W: Write>(
    writer: &mut W,
    columns: &[ColumnHeader],
    rows: &[Row],
) -> Result<(), ExportError> {
    let mut first = true;
    for column in columns {
        if !first {
            writer.write_all(b"\t")?;
        }
        write_tsv_field(writer, &column.name)?;
        first = false;
    }
    writer.write_all(b"\n")?;

    for row in rows {
        let mut first = true;
        for value in &row.0 {
            if !first {
                writer.write_all(b"\t")?;
            }
            match value {
                Value::Null => {}
                other => write_tsv_field(writer, &other.render())?,
            }
            first = false;
        }
        writer.write_all(b"\n")?;
    }
    Ok(())
}

pub(super) fn write_tsv_field<W: Write>(writer: &mut W, field: &str) -> Result<(), ExportError> {
    // Replace the three characters that would corrupt TSV framing. Any
    // other Unicode passes through verbatim — TSV is bytes-in, bytes-out.
    for ch in field.chars() {
        let safe = match ch {
            '\t' | '\n' | '\r' => ' ',
            other => other,
        };
        let mut buf = [0u8; 4];
        writer.write_all(safe.encode_utf8(&mut buf).as_bytes())?;
    }
    Ok(())
}
