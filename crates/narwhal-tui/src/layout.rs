use narwhal_vim::Mode;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;
use crate::widgets::{
    editor_cursor_anchor, render_completion_popup, render_editor, render_results, render_sidebar,
    CompletionPopupView, EditorBuffer, EditorSearchHighlight, ResultDisplay, ResultView,
    SidebarView,
};

/// Hit-test regions computed during the last render. Stored on `AppCore`
/// so that a `MouseEvent` arriving on the next frame can determine which
/// element the pointer landed on.
#[derive(Debug, Default, Clone)]
pub struct LayoutRegions {
    pub sidebar: Rect,
    pub editor: Rect,
    pub results: Rect,
    pub status: Rect,
    pub completion: Option<Rect>,
    /// One `(Rect, sidebar_index)` per visible table entry in the sidebar.
    /// `sidebar_index` indexes into `AppCore::sidebar_items`.
    pub sidebar_tables: Vec<(Rect, usize)>,
    /// One `(Rect, column_index)` per rendered column header cell.
    pub result_headers: Vec<(Rect, usize)>,
    /// One `(Rect, row_index)` per rendered data row.
    pub result_rows: Vec<(Rect, usize)>,
    /// One `(Rect, result_index)` per rendered result tab in the strip.
    /// Empty when the bundle has only one result.
    pub result_tabs: Vec<(Rect, usize)>,
    /// One `(Rect, item_index)` per visible completion item.
    pub completion_items: Vec<(Rect, usize)>,
}

/// Indicates which pane currently owns keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Sidebar,
    Editor,
    Results,
}

impl Pane {
    pub fn cycle(self) -> Self {
        match self {
            Pane::Sidebar => Pane::Editor,
            Pane::Editor => Pane::Results,
            Pane::Results => Pane::Sidebar,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Pane::Sidebar => "sidebar",
            Pane::Editor => "editor",
            Pane::Results => "results",
        }
    }
}

/// Read-only view of the three-slot status bar passed from
/// [`narwhal_app::core::StatusBar`] into the render path.
#[derive(Debug, Clone, Default)]
pub struct StatusBarView<'a> {
    /// Center slot — connection name + driver (sticky).
    pub connection: Option<&'a str>,
    /// Right slot — last transient message.
    pub message: &'a str,
    /// Optional fourth slot — transaction isolation level.
    pub transaction: Option<&'a str>,
}

pub struct RootLayout<'a> {
    pub mode: Mode,
    pub focus: Pane,
    pub status_bar: StatusBarView<'a>,
    pub running: bool,
    pub theme: &'a Theme,
    pub sidebar: SidebarView<'a>,
    pub editor: &'a mut EditorBuffer,
    pub editor_title: &'a str,
    pub result_view: &'a mut ResultView,
    pub result: ResultDisplay<'a>,
    /// When `Some`, an overlay completion popup is rendered above the
    /// editor pane on top of the regular widgets.
    pub completion: Option<CompletionPopupView<'a>>,
    /// When `Some`, editor search matches are highlighted.
    pub editor_search: Option<EditorSearchHighlight<'a>>,
    /// Number of results in the bundle. >1 means the tab strip renders.
    pub result_count: usize,
    /// Index of the active result (0-based).
    pub active_result: usize,
}

pub fn render_root(frame: &mut Frame<'_>, area: Rect, view: &mut RootLayout<'_>) -> LayoutRegions {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(34), Constraint::Min(1)])
        .split(outer[0]);

    let sidebar_table_indices = render_sidebar(frame, body[0], &view.sidebar, view.theme);

    let main = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(body[1]);

    let editor_area = main[0];
    render_editor(
        frame,
        editor_area,
        view.editor,
        view.theme,
        view.focus == Pane::Editor,
        view.editor_title,
        view.editor_search.as_ref(),
    );

    let result_regions = render_results(
        frame,
        main[1],
        &view.result,
        view.result_view,
        view.theme,
        view.focus == Pane::Results,
        view.result_count,
        view.active_result,
    );

    render_status_bar(frame, outer[1], view);

    let completion_regions = if let Some(popup) = view.completion.as_ref() {
        let mut popup = *popup;
        // Re-anchor the popup to the actual editor cursor coordinates so
        // the host app doesn't need to mirror our layout maths.
        popup.anchor = editor_cursor_anchor(editor_area, view.editor);
        let regions = render_completion_popup(frame, area, &popup, view.theme);
        Some(regions)
    } else {
        None
    };

    // Build LayoutRegions from the captured rects.
    let sidebar_tables = sidebar_table_indices;

    LayoutRegions {
        sidebar: body[0],
        editor: editor_area,
        results: main[1],
        status: outer[1],
        completion: completion_regions.as_ref().and_then(|r| r.popup_rect),
        sidebar_tables,
        result_headers: result_regions.headers,
        result_rows: result_regions.rows,
        result_tabs: result_regions.tabs,
        completion_items: completion_regions
            .map(|regions| regions.items)
            .unwrap_or_default(),
    }
}

fn render_status_bar(frame: &mut Frame<'_>, area: Rect, view: &RootLayout<'_>) {
    let mode_style = match view.mode {
        Mode::Insert => view.theme.mode_insert(),
        Mode::Command | Mode::Visual | Mode::VisualLine => view.theme.mode_command(),
        Mode::Normal => view.theme.mode_normal(),
    };

    let mode_label = format!(" {} ", view.mode.short_label());
    let _mode_width = mode_label.width() as u16;

    let focus_label = view.focus.label();
    let left_text = format!(" {mode_label}{focus_label} ");
    let left_width = left_text.width() as u16;

    let conn_text = match view.status_bar.connection {
        Some(c) => format!(" {c} "),
        None => " (no connection) ".to_owned(),
    };
    let conn_width = conn_text.width() as u16;

    let txn_text = match view.status_bar.transaction {
        Some(t) => format!(" TX:{t} "),
        None => String::new(),
    };
    let txn_width = txn_text.width() as u16;

    let running_prefix = if view.running { "⏳ " } else { "" };
    let msg_text = format!(" {}{}", running_prefix, view.status_bar.message);
    let msg_width: u16 = 20; // minimum for the right slot

    let parts = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(left_width),
            Constraint::Length(conn_width),
            Constraint::Length(txn_width),
            Constraint::Min(msg_width),
        ])
        .split(area);

    // Left slot: mode + focus pane
    frame.render_widget(Paragraph::new(left_text).style(mode_style), parts[0]);

    // Center slot: connection (sticky)
    frame.render_widget(
        Paragraph::new(conn_text).style(view.theme.status_bar()),
        parts[1],
    );

    // Optional fourth slot: transaction badge (yellow text)
    if !txn_text.is_empty() {
        frame.render_widget(
            Paragraph::new(txn_text).style(view.theme.transaction_badge()),
            parts[2],
        );
    }

    // Right slot: message
    frame.render_widget(
        Paragraph::new(msg_text).style(view.theme.status_bar()),
        parts[3],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_bar_width_handles_wide_chars() {
        // The ⏳ (hourglass) emoji has display width 2 in most terminals.
        // CJK character '中' has display width 2.
        // With chars().count(), "⏳" would be 1 cell; with width() it's 2.
        let text = "⏳ running";
        assert_eq!(text.width(), 10, "⏳ should count as 2 display cells");
        // Verify the old chars().count() gives the wrong answer:
        assert_ne!(text.chars().count(), text.width());
    }

    #[test]
    fn status_bar_width_cjk_connection() {
        let conn = " 中文数据库 ";
        // Each CJK char = 2 display cells, plus 2 spaces = 2 + 4*2 + 2 = 12
        assert_eq!(conn.width(), 12);
    }
}
