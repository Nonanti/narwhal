use narwhal_vim::Mode;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::theme::Theme;
use crate::widgets::{
    editor_cursor_anchor, render_completion_popup, render_editor, render_results, render_sidebar,
    CompletionPopupView, EditorBuffer, ResultDisplay, ResultView, SidebarView,
};

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
}

pub fn render_root(frame: &mut Frame<'_>, area: Rect, view: &mut RootLayout<'_>) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(34), Constraint::Min(1)])
        .split(outer[0]);

    render_sidebar(frame, body[0], &view.sidebar, view.theme);

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
    );

    render_results(
        frame,
        main[1],
        &view.result,
        view.result_view,
        view.theme,
        view.focus == Pane::Results,
    );

    render_status_bar(frame, outer[1], view);

    if let Some(popup) = view.completion.as_ref() {
        let mut popup = *popup;
        // Re-anchor the popup to the actual editor cursor coordinates so
        // the host app doesn't need to mirror our layout maths.
        popup.anchor = editor_cursor_anchor(editor_area, view.editor);
        render_completion_popup(frame, area, &popup, view.theme);
    }
}

fn render_status_bar(frame: &mut Frame<'_>, area: Rect, view: &RootLayout<'_>) {
    let mode_style = match view.mode {
        Mode::Insert => view.theme.mode_insert(),
        Mode::Command | Mode::Visual | Mode::VisualLine => view.theme.mode_command(),
        Mode::Normal => view.theme.mode_normal(),
    };

    let mode_label = format!(" {} ", view.mode.short_label());
    let _mode_width = mode_label.chars().count() as u16;

    let focus_label = view.focus.label();
    let left_text = format!(" {mode_label}{focus_label} ");
    let left_width = left_text.chars().count() as u16;

    let conn_text = match view.status_bar.connection {
        Some(c) => format!(" {c} "),
        None => " (no connection) ".to_owned(),
    };
    let conn_width = conn_text.chars().count() as u16;

    let txn_text = match view.status_bar.transaction {
        Some(t) => format!(" TX:{t} "),
        None => String::new(),
    };
    let txn_width = txn_text.chars().count() as u16;

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
