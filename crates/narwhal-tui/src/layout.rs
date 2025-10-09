use narwhal_vim::Mode;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::theme::Theme;
use crate::widgets::{
    render_editor, render_results, render_sidebar, EditorBuffer, ResultDisplay, ResultView,
    SidebarView,
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

pub struct RootLayout<'a> {
    pub mode: Mode,
    pub focus: Pane,
    pub connection_label: &'a str,
    pub status_message: &'a str,
    pub running: bool,
    /// `Some` when a transaction is open; the inner str is a short tag
    /// such as "TX" or "TX·sp:2" that the status bar renders verbatim.
    pub transaction_badge: Option<&'a str>,
    pub theme: &'a Theme,
    pub sidebar: SidebarView<'a>,
    pub editor: &'a mut EditorBuffer,
    pub editor_title: &'a str,
    pub result_view: &'a mut ResultView,
    pub result: ResultDisplay<'a>,
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

    render_editor(
        frame,
        main[0],
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
}

fn render_status_bar(frame: &mut Frame<'_>, area: Rect, view: &RootLayout<'_>) {
    let parts = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(area);

    let mode_label = format!(" {} ", view.mode.short_label());
    frame.render_widget(
        Paragraph::new(mode_label).style(view.theme.mode_indicator()),
        parts[0],
    );

    frame.render_widget(Paragraph::new(" ").style(view.theme.status_bar()), parts[1]);

    let running_indicator = if view.running { "⏳ " } else { "" };
    let txn = match view.transaction_badge {
        Some(tag) => format!("│ {tag} "),
        None => String::new(),
    };
    let body = Line::from(vec![Span::raw(format!(
        " {} │ {} {}│ {}{} ",
        view.focus.label(),
        view.connection_label,
        txn,
        running_indicator,
        view.status_message
    ))]);
    frame.render_widget(
        Paragraph::new(body).style(view.theme.status_bar()),
        parts[2],
    );
}
