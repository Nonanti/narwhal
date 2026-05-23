//! Schema-detail block painted into the result pane when a table
//! introspection is shown instead of a row set.
//!
//! Renders as a tab strip across the top (`Records / Columns /
//! Constraints / FKs / Indexes`) plus the content for the currently
//! selected tab beneath. The `Records` tab is special-cased at the
//! host level: choosing it swaps the entire result state to
//! [`super::ResultDisplay::Rows`] (a preview query), so this renderer
//! only ever sees the four detail tabs.

use narwhal_core::{Column, ForeignKey, Index, TableSchema, UniqueConstraint};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::model::MetaTab;
use crate::theme::Theme;

pub(super) fn draw_table_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    schema: &TableSchema,
    active_tab: MetaTab,
    theme: &Theme,
) {
    // Reserve one row for the tab strip, the rest for the body.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    draw_tab_strip(frame, chunks[0], active_tab, theme);

    let body_lines = match active_tab {
        // `Records` is intercepted by the host (it swaps the entire
        // result state). If we still get here, render an explainer so
        // the user is not staring at a blank pane.
        MetaTab::Records => vec![Line::from(Span::styled(
            "  press Enter or `1` to preview rows for this table",
            Style::default().fg(theme.muted),
        ))],
        MetaTab::Columns => render_columns(&schema.columns, theme),
        MetaTab::Constraints => {
            render_constraints(&schema.columns, &schema.unique_constraints, theme)
        }
        MetaTab::ForeignKeys => render_foreign_keys(&schema.foreign_keys, theme),
        MetaTab::Indexes => render_indexes(&schema.indexes, theme),
    };
    frame.render_widget(Paragraph::new(body_lines), chunks[1]);
}

fn draw_tab_strip(frame: &mut Frame<'_>, area: Rect, active: MetaTab, theme: &Theme) {
    let active_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD | Modifier::REVERSED);
    let inactive_style = Style::default().fg(theme.muted);

    let mut spans: Vec<Span<'_>> = Vec::with_capacity(MetaTab::all().len() * 2);
    spans.push(Span::raw("  "));
    for (i, tab) in MetaTab::all().iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", inactive_style));
        }
        let label = format!(" {} {} ", tab.index(), tab.label());
        let style = if *tab == active {
            active_style
        } else {
            inactive_style
        };
        spans.push(Span::styled(label, style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_columns(columns: &[Column], theme: &Theme) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(columns.len() + 2);
    let bold_accent = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    lines.push(Line::from(Span::styled(
        format!("  {} column(s)", columns.len()),
        bold_accent,
    )));
    if columns.is_empty() {
        lines.push(Line::from("    (no columns reported)".to_owned()));
        return lines;
    }
    for col in columns {
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
    lines
}

fn render_constraints(
    columns: &[Column],
    uniques: &[UniqueConstraint],
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let bold_accent = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);

    // Primary key: derived from the columns table (single source of
    // truth across all engines).
    let pk_cols: Vec<&Column> = columns.iter().filter(|c| c.primary_key).collect();
    lines.push(Line::from(Span::styled("  primary key", bold_accent)));
    if pk_cols.is_empty() {
        lines.push(Line::from(
            "    (none)  \u{2014} row-level edits and deletes are disabled".to_owned(),
        ));
    } else {
        let names: Vec<String> = pk_cols.iter().map(|c| c.name.clone()).collect();
        lines.push(Line::from(format!("    ({})", names.join(", "))));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  unique constraints",
        bold_accent,
    )));
    if uniques.is_empty() {
        lines.push(Line::from("    (none)".to_owned()));
    } else {
        for uq in uniques {
            lines.push(Line::from(format_unique_line(uq)));
        }
    }
    lines
}

fn render_foreign_keys(fks: &[ForeignKey], theme: &Theme) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let bold_accent = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    lines.push(Line::from(Span::styled(
        format!("  {} foreign key(s)", fks.len()),
        bold_accent,
    )));
    if fks.is_empty() {
        lines.push(Line::from("    (none)".to_owned()));
        return lines;
    }
    for fk in fks {
        lines.push(Line::from(format_foreign_key_line(fk)));
    }
    lines
}

fn render_indexes(indexes: &[Index], theme: &Theme) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let bold_accent = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    lines.push(Line::from(Span::styled(
        format!("  {} index(es)", indexes.len()),
        bold_accent,
    )));
    if indexes.is_empty() {
        lines.push(Line::from("    (none)".to_owned()));
        return lines;
    }
    for idx in indexes {
        lines.push(Line::from(format_index_line(idx)));
    }
    lines
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
