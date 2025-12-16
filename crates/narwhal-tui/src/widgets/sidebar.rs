//! Connection / schema browser shown in the sidebar.
//!
//! The browser receives a flat list of [`SidebarRow`]s so the consumer
//! controls exactly which entries are interactive and at which depth they
//! are rendered. This keeps key handling simple — a single `selected_index`
//! into a homogeneous slice — and makes it easy to add new row kinds.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::theme::Theme;
use crate::widgets::results::sanitize_for_display;

/// Backwards-compatible alias kept so existing callers can keep building
/// schema listings the way they did before the sidebar refactor.
pub type SchemaListing = (narwhal_core::Schema, Vec<narwhal_core::Table>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarRowKind {
    Connection,
    ActiveConnection,
    Schema,
    Table,
    View,
    MaterializedView,
    SystemTable,
}

impl SidebarRowKind {
    fn glyph(self) -> &'static str {
        match self {
            Self::Connection => "○",
            Self::ActiveConnection => "●",
            Self::Schema => "▾",
            Self::Table => "▢",
            Self::View => "▤",
            Self::MaterializedView => "▥",
            Self::SystemTable => "▣",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SidebarRow<'a> {
    pub depth: u8,
    pub kind: SidebarRowKind,
    pub label: &'a str,
}

pub struct SidebarView<'a> {
    pub items: &'a [SidebarRow<'a>],
    pub selected_index: usize,
    pub focused: bool,
}

pub fn render_sidebar(
    frame: &mut Frame<'_>,
    area: Rect,
    view: &SidebarView<'_>,
    theme: &Theme,
) -> Vec<(Rect, usize)> {
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

    if view.items.is_empty() {
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
        return Vec::new();
    }

    let lines: Vec<Line<'_>> = view
        .items
        .iter()
        .enumerate()
        .map(|(idx, row)| {
            let selected = idx == view.selected_index;
            let cursor = if selected && view.focused { "▶" } else { " " };
            let indent = "  ".repeat(row.depth as usize);
            let glyph = row.kind.glyph();
            let mut style = Style::default().fg(theme.foreground);
            match row.kind {
                SidebarRowKind::ActiveConnection => {
                    style = style.fg(theme.accent).add_modifier(Modifier::BOLD);
                }
                SidebarRowKind::Schema => {
                    style = style.fg(theme.muted);
                }
                _ => {}
            }
            Line::from(vec![
                Span::raw(format!(" {cursor} ")),
                Span::raw(indent),
                Span::raw(format!("{glyph} ")),
                Span::styled(sanitize_for_display(row.label).into_owned(), style),
            ])
        })
        .collect();

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);

    // Build hit-test rects for table entries. Each visible item occupies
    // one row in the inner area starting at y = inner.y.
    let mut table_rects = Vec::new();
    for (idx, row) in view.items.iter().enumerate() {
        if matches!(
            row.kind,
            SidebarRowKind::Table
                | SidebarRowKind::View
                | SidebarRowKind::MaterializedView
                | SidebarRowKind::SystemTable
        ) {
            let rect = Rect {
                x: inner.x,
                y: inner.y + idx as u16,
                width: inner.width,
                height: 1,
            };
            table_rects.push((rect, idx));
        }
    }
    table_rects
}
