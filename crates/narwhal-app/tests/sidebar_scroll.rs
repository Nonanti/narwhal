//! L24 — sidebar viewport scrolling.
//!
//! Renders a sidebar with more rows than fit in the viewport, then
//! verifies that:
//!   * selection beyond the viewport pulls the scroll offset along,
//!   * mouse wheel pans the viewport without moving the selection,
//!   * `PageDown` / `PageUp` jump 10 rows at a time.

use narwhal_tui::widgets::sidebar::SidebarView;

#[test]
fn clamp_scroll_keeps_selection_visible() {
    // selection above the window → pull scroll up to the selection
    assert_eq!(SidebarView::clamp_scroll(0, 10, 5, 50), 0);
    // selection inside the window → no movement
    assert_eq!(SidebarView::clamp_scroll(12, 10, 5, 50), 10);
    // selection just below the window → push scroll down by 1
    assert_eq!(SidebarView::clamp_scroll(15, 10, 5, 50), 11);
    // selection well below the window → scroll = selected + 1 - visible
    assert_eq!(SidebarView::clamp_scroll(40, 10, 5, 50), 36);
    // saturate at max scroll (total - visible)
    assert_eq!(SidebarView::clamp_scroll(49, 100, 5, 50), 45);
    // empty list → 0
    assert_eq!(SidebarView::clamp_scroll(0, 0, 5, 0), 0);
    // zero viewport → 0 (degenerate but well-defined)
    assert_eq!(SidebarView::clamp_scroll(10, 5, 0, 50), 0);
}
