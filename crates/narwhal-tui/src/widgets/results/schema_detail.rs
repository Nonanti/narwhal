//! Schema-detail block painted into the result pane when a table
//! introspection is shown instead of a row set.

use narwhal_core::{ForeignKey, Index, TableSchema, UniqueConstraint};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::theme::Theme;

pub(super) fn draw_table_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    schema: &TableSchema,
    theme: &Theme,
) {
    let mut lines: Vec<Line<'_>> = Vec::with_capacity(
        schema.columns.len() + schema.indexes.len() + schema.foreign_keys.len() + 8,
    );
    let bold_accent = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);

    lines.push(Line::from(Span::styled("  columns", bold_accent)));
    for col in &schema.columns {
        let mut tags = Vec::new();
        if col.primary_key {
            tags.push("PK");
        }
        if !col.nullable {
            tags.push("NOT NULL");
        }
        let tag_str = if tags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", tags.join(" "))
        };
        let default_str = match &col.default {
            Some(d) => format!("  default {d}"),
            None => String::new(),
        };
        lines.push(Line::from(format!(
            "    {:<24} {}{tag_str}{default_str}",
            col.name, col.data_type
        )));
    }

    if !schema.indexes.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("  indexes", bold_accent)));
        for idx in &schema.indexes {
            lines.push(Line::from(format_index_line(idx)));
        }
    }

    if !schema.foreign_keys.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("  foreign keys", bold_accent)));
        for fk in &schema.foreign_keys {
            lines.push(Line::from(format_foreign_key_line(fk)));
        }
    }

    if !schema.unique_constraints.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  unique constraints",
            bold_accent,
        )));
        for uq in &schema.unique_constraints {
            lines.push(Line::from(format_unique_line(uq)));
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn format_index_line(idx: &Index) -> String {
    let kind = if idx.primary {
        "PRIMARY"
    } else if idx.unique {
        "UNIQUE"
    } else {
        "INDEX"
    };
    format!("    {kind:<8} {}({})", idx.name, idx.columns.join(", "))
}

fn format_foreign_key_line(fk: &ForeignKey) -> String {
    let qualifier = match &fk.referenced_schema {
        Some(s) if !s.is_empty() => format!("{s}."),
        _ => String::new(),
    };
    let actions = {
        let mut bits = Vec::new();
        if let Some(a) = fk.on_update {
            bits.push(format!("ON UPDATE {}", a.as_sql()));
        }
        if let Some(a) = fk.on_delete {
            bits.push(format!("ON DELETE {}", a.as_sql()));
        }
        if bits.is_empty() {
            String::new()
        } else {
            format!("  {}", bits.join(" "))
        }
    };
    format!(
        "    {} ({}) -> {}{}({}){actions}",
        fk.name,
        fk.columns.join(", "),
        qualifier,
        fk.referenced_table,
        fk.referenced_columns.join(", ")
    )
}

fn format_unique_line(uq: &UniqueConstraint) -> String {
    format!("    {} ({})", uq.name, uq.columns.join(", "))
}
