//! Connection / schema browser shown in the sidebar.
//!
//! The browser receives a flat list of [`SidebarRow`]s so the consumer
//! controls exactly which entries are interactive and at which depth they
//! are rendered. This keeps key handling simple — a single `selected_index`
//! into a homogeneous slice — and makes it easy to add new row kinds.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::theme::Theme;
use crate::widgets::results::sanitize_for_display;

/// Backwards-compatible alias kept so existing callers can keep building
/// schema listings the way they did before the sidebar refactor.
pub use narwhal_domain::SchemaListing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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
    const fn glyph(self) -> &'static str {
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
    /// First visible row index (L24). The renderer slices
    /// `items[scroll_offset..scroll_offset + visible_rows]`. Callers
    /// should clamp this in tandem with `selected_index` so the cursor
    /// is always within the visible window.
    pub scroll_offset: usize,
    pub focused: bool,
}

impl SidebarView<'_> {
    /// How many rows fit in `inner_height` cells (one row per item).
    pub const fn visible_rows(inner_height: u16) -> usize {
        inner_height as usize
    }

    /// Clamp `scroll_offset` so `selected_index` is within the visible
    /// window `[scroll, scroll + visible)`. Mirrors what callers should
    /// do before passing the view to [`render_sidebar`].
    pub fn clamp_scroll(selected: usize, scroll: usize, visible: usize, total: usize) -> usize {
        if visible == 0 || total == 0 {
            return 0;
        }
        let max_scroll = total.saturating_sub(visible);
        let mut s = scroll.min(max_scroll);
        if selected < s {
            s = selected;
        } else if selected >= s + visible {
            s = selected + 1 - visible;
        }
        s.min(max_scroll)
    }
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
        let accent = Style::default().fg(theme.accent);
        let muted = Style::default().fg(theme.muted);
        let p = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled("  no connections yet", muted)),
            Line::from(""),
            Line::from(vec![
                Span::styled("  press ", muted),
                Span::styled(":add", accent),
                Span::styled(" to create one", muted),
            ]),
            Line::from(vec![
                Span::styled("  or  ", muted),
                Span::styled(":url <dsn>", accent),
                Span::styled(" to paste a URL", muted),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("  press ", muted),
                Span::styled("?", accent),
                Span::styled(" for help", muted),
            ]),
        ]);
        frame.render_widget(p, inner);
        return Vec::new();
    }

    // L24: render only the slice that fits in the inner viewport.
    let visible = SidebarView::visible_rows(inner.height);
    let total = view.items.len();
    let scroll = SidebarView::clamp_scroll(view.selected_index, view.scroll_offset, visible, total);
    let end = (scroll + visible).min(total);

    let lines: Vec<Line<'_>> = view.items[scroll..end]
        .iter()
        .enumerate()
        .map(|(slice_idx, row)| {
            let idx = scroll + slice_idx;
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
    // one row in the inner area starting at y = inner.y. We map slice
    // positions back to absolute item indices so the host's click handler
    // doesn't need to know about scroll state.
    let mut table_rects = Vec::new();
    for (slice_idx, row) in view.items[scroll..end].iter().enumerate() {
        let idx = scroll + slice_idx;
        if matches!(
            row.kind,
            SidebarRowKind::Table
                | SidebarRowKind::View
                | SidebarRowKind::MaterializedView
                | SidebarRowKind::SystemTable
        ) {
            let rect = Rect {
                x: inner.x,
                y: inner.y + slice_idx as u16,
                width: inner.width,
                height: 1,
            };
            table_rects.push((rect, idx));
        }
    }
    table_rects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_scroll_keeps_selection_visible() {
        // Selection above the window pulls scroll up.
        assert_eq!(SidebarView::clamp_scroll(0, 10, 5, 20), 0);
        // Selection below the window pushes scroll down.
        assert_eq!(SidebarView::clamp_scroll(8, 0, 5, 20), 4);
        // Selection inside the window leaves scroll alone.
        assert_eq!(SidebarView::clamp_scroll(3, 2, 5, 20), 2);
        // Scroll never exceeds max_scroll.
        assert_eq!(SidebarView::clamp_scroll(19, 100, 5, 20), 15);
    }

    #[test]
    fn clamp_scroll_handles_degenerate_inputs() {
        assert_eq!(SidebarView::clamp_scroll(0, 0, 0, 20), 0);
        assert_eq!(SidebarView::clamp_scroll(0, 0, 5, 0), 0);
    }
}
