//! Editor right-click context menu overlay.
//!
//! Drawn on top of the editor pane whenever the user right-clicks
//! inside it. Each entry has a label, an action id (interpreted by
//! the host) and a disabled flag — disabled entries are rendered
//! greyed out but still visible so the menu width stays stable
//! between clicks.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::theme::Theme;

/// Borrowed view of the context-menu state. The host (`narwhal_app`)
/// builds this from its own `ContextMenuState` so the renderer stays
/// allocation-free.
pub struct ContextMenuView<'a> {
    /// Anchor cell where the menu's top-left should land. The
    /// renderer clamps to the screen bounds.
    pub anchor: (u16, u16),
    /// One entry per line.
    pub items: &'a [ContextMenuItemView<'a>],
    /// Highlighted entry index.
    pub selected: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct ContextMenuItemView<'a> {
    pub label: &'a str,
    pub disabled: bool,
}

/// Render the menu inside `screen`. The menu width is the longest
/// label plus 4 cells of padding; height is `items.len() + 2`.
pub fn render_context_menu(
    frame: &mut Frame<'_>,
    screen: Rect,
    view: &ContextMenuView<'_>,
    theme: &Theme,
) {
    if view.items.is_empty() {
        return;
    }

    // Bail out when the terminal is too small to show even a
    // one-line menu. `u16::clamp(min, max)` panics if min > max,
    // which happened when screen.width <= 13 (max = width - 2
    // <= 11 < 12 = min). Using min().max() avoids that, and the
    // early return prevents rendering a zero-size area.
    let max_width = screen.width.saturating_sub(2);
    let max_height = screen.height.saturating_sub(2);
    if max_width == 0 || max_height == 0 {
        return;
    }

    let widest = view
        .items
        .iter()
        .map(|i| i.label.chars().count())
        .max()
        .unwrap_or(8) as u16;
    let min_width: u16 = 12;
    let desired = widest.saturating_add(4);
    let width = desired.min(max_width).max(min_width.min(max_width));
    let height = (view.items.len() as u16).saturating_add(2).min(max_height);

    // Clamp the anchor so the menu stays on-screen.
    let max_x = screen.x + screen.width.saturating_sub(width);
    let max_y = screen.y + screen.height.saturating_sub(height);
    let x = view.anchor.0.min(max_x);
    let y = view.anchor.1.min(max_y);

    let area = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(" menu ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines: Vec<Line<'_>> = view
        .items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let mut style = Style::default();
            if item.disabled {
                style = style.fg(theme.muted);
            }
            if idx == view.selected && !item.disabled {
                style = style
                    .bg(theme.accent)
                    .fg(theme.background)
                    .add_modifier(Modifier::BOLD);
            }
            let pad = " ".repeat(2);
            Line::from(format!("{pad}{label}", pad = pad, label = item.label)).style(style)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}
