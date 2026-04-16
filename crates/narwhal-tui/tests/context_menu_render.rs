//! Regression tests for context-menu rendering on tiny terminals.
//!
//! Before the v2.1.1 fix, `render_context_menu` called
//! `u16::clamp(12, screen.width - 2)`. When `screen.width <= 13`,
//! the lower bound exceeded the upper bound and `clamp` panicked,
//! crashing the app on right-click. These tests verify the renderer
//! no longer panics on any terminal size.

use narwhal_tui::{ContextMenuItemView, ContextMenuView, Theme, render_context_menu};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn render_at(w: u16, h: u16) {
    let backend = TestBackend::new(w, h);
    let mut term = Terminal::new(backend).unwrap();
    let items = [
        ContextMenuItemView {
            label: "Copy",
            disabled: false,
        },
        ContextMenuItemView {
            label: "Paste",
            disabled: true,
        },
    ];
    term.draw(|f| {
        let view = ContextMenuView {
            anchor: (0, 0),
            items: &items,
            selected: 0,
        };
        render_context_menu(f, f.area(), &view, &Theme::DARK);
    })
    .unwrap();
}

#[test]
fn tiny_4x4() {
    render_at(4, 4);
}

#[test]
fn tiny_8x4() {
    render_at(8, 4);
}

#[test]
fn tiny_13x10() {
    // Boundary: width = 13 → max_width = 11 < min_width 12.
    // This is the exact case that used to panic.
    render_at(13, 10);
}

#[test]
fn normal_80x24() {
    render_at(80, 24);
}

#[test]
fn minimal_viable_14x4() {
    // Width 14 is the smallest where max_width (12) >= min_width.
    render_at(14, 4);
}

#[test]
fn very_narrow_1x1() {
    render_at(1, 1);
}

#[test]
fn empty_items_does_not_panic() {
    let backend = TestBackend::new(4, 4);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| {
        let view = ContextMenuView {
            anchor: (0, 0),
            items: &[],
            selected: 0,
        };
        render_context_menu(f, f.area(), &view, &Theme::DARK);
    })
    .unwrap();
}
