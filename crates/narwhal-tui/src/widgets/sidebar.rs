//! Connection / schema browser shown in the sidebar.

use narwhal_core::{ConnectionConfig, Schema, Table};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use uuid::Uuid;

use crate::theme::Theme;

/// Pair of a schema with the tables it contains.
pub type SchemaListing = (Schema, Vec<Table>);

/// View model passed to [`render_sidebar`] each frame.
pub struct SidebarView<'a> {
    pub connections: &'a [ConnectionConfig],
    pub active_connection: Option<Uuid>,
    pub schemas: &'a [SchemaListing],
    pub selected_index: usize,
    pub focused: bool,
}

pub fn render_sidebar(frame: &mut Frame<'_>, area: Rect, view: &SidebarView<'_>, theme: &Theme) {
    let border_style = if view.focused {
        Style::default().fg(theme.accent)
    } else {
        Style::default().fg(theme.muted)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(" narwhal ", theme.sidebar_title()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if view.connections.is_empty() {
        let p = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  no connections",
                Style::default().fg(theme.muted),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  add one to connections.toml",
                Style::default().fg(theme.muted),
            )),
        ]);
        frame.render_widget(p, inner);
        return;
    }

    let mut lines: Vec<Line<'_>> = Vec::with_capacity(view.connections.len() * 3);
    for (idx, conn) in view.connections.iter().enumerate() {
        let active = Some(conn.id) == view.active_connection;
        let selected = idx == view.selected_index;
        let marker = if active { "●" } else { "○" };
        let cursor = if selected && view.focused { "▶" } else { " " };
        let mut style = Style::default().fg(theme.foreground);
        if active {
            style = style.add_modifier(Modifier::BOLD).fg(theme.accent);
        }
        let label = format!(" {cursor} {marker} {} ({}) ", conn.name, conn.driver);
        lines.push(Line::from(Span::styled(label, style)));

        if active && !view.schemas.is_empty() {
            for (schema, tables) in view.schemas {
                lines.push(Line::from(Span::styled(
                    format!("       {}", schema.name),
                    Style::default().fg(theme.muted),
                )));
                for table in tables {
                    let kind_glyph = match table.kind {
                        narwhal_core::TableKind::Table => "▢",
                        narwhal_core::TableKind::View => "▤",
                        narwhal_core::TableKind::MaterializedView => "▥",
                        narwhal_core::TableKind::SystemTable => "▣",
                    };
                    lines.push(Line::from(format!("         {kind_glyph} {}", table.name)));
                }
            }
        }
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}
