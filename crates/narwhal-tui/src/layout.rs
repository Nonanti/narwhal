use narwhal_vim::Mode;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;
use crate::widgets::{
    ChartPlaceholder, ChartView, CompletionPopupView, EditorBuffer, EditorSearchHighlight,
    PivotPlaceholder, PivotTableView, ResultDisplay, ResultView, SidebarView, editor_cursor_anchor,
    render_chart, render_chart_placeholder, render_completion_popup, render_editor, render_pivot,
    render_pivot_placeholder, render_results, render_sidebar,
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
#[non_exhaustive]
pub enum Pane {
    Sidebar,
    Editor,
    Results,
}

impl Pane {
    pub const fn cycle(self) -> Self {
        // Same-crate enum is exhaustively matched; the `#[non_exhaustive]`
        // attribute only forces wildcards on downstream consumers.
        match self {
            Self::Sidebar => Self::Editor,
            Self::Editor => Self::Results,
            Self::Results => Self::Sidebar,
        }
    }

    /// Reverse-cycle counterpart to [`Pane::cycle`] (L27). Used by
    /// `Shift-Ctrl+W` to walk the focus chain backwards.
    pub const fn cycle_back(self) -> Self {
        match self {
            Self::Sidebar => Self::Results,
            Self::Editor => Self::Sidebar,
            Self::Results => Self::Editor,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Sidebar => "sidebar",
            Self::Editor => "editor",
            Self::Results => "results",
        }
    }
}

/// Read-only view of the three-slot status bar passed from
/// `narwhal_app::core::StatusBar` into the render path.
#[derive(Debug, Clone, Default)]
pub struct StatusBarView<'a> {
    /// Center slot — connection name + driver (sticky).
    pub connection: Option<&'a str>,
    /// Right slot — last transient message.
    pub message: &'a str,
    /// Optional fourth slot — transaction isolation level.
    pub transaction: Option<&'a str>,
    /// Optional badge — number of staged mutations awaiting commit
    /// (L36). Rendered next to the connection slot so the user is
    /// always aware that there are uncommitted changes to commit or
    /// discard.
    pub pending: Option<usize>,
    /// L36 #11: `true` when the process was launched with
    /// `--read-only`. Renders a high-contrast `[RO]` badge so the
    /// user can tell at a glance that mutations will be refused.
    pub read_only: bool,
}

pub struct RootLayout<'a> {
    pub mode: Mode,
    /// When `Some`, replaces the vim mode short label with the
    /// provided text — used by basic / emacs editor modes to render
    /// `BASIC` / `EMACS` instead of `NOR`/`INS`. When `None`, the
    /// `mode` field above drives the label as before.
    pub mode_label_override: Option<&'a str>,
    /// When `false`, the mode indicator segment is hidden entirely.
    /// Mapped from `[editor].show_mode_indicator` in `settings.toml`.
    pub show_mode_indicator: bool,
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
    /// when `Some`, treesitter SQL highlight spans for the
    /// editor buffer are overlaid on the visible rows. The host app
    /// keeps the underlying [`narwhal_sql::treesitter::Parser`] per
    /// tab and refreshes the slice each render tick.
    pub editor_sql_highlights: Option<&'a [narwhal_sql::treesitter::HighlightSpan]>,
    /// when `Some(Ok(view))`, an inline ASCII chart is rendered
    /// in the top half of the result pane; `Some(Err(placeholder))`
    /// shows the error message inside an otherwise-empty chart block;
    /// `None` leaves the chart pane hidden (full-table layout).
    pub chart: Option<Result<ChartView<'a>, ChartPlaceholder<'a>>>,
    /// when `Some(Ok(view))`, an inline pivot table is
    /// rendered in the top half of the result pane;
    /// `Some(Err(placeholder))` shows the error message inside an
    /// otherwise-empty pivot block; `None` leaves the pivot pane
    /// hidden (full-table layout).
    pub pivot: Option<Result<PivotTableView<'a>, PivotPlaceholder<'a>>>,
    /// Number of results in the bundle. >1 means the tab strip renders.
    pub result_count: usize,
    /// Index of the active result (0-based).
    pub active_result: usize,
    /// optional accent colour pulled from the active
    /// connection's `color = "…"` field. When `Some`, the connection
    /// slot in the status bar is tinted to give the user a constant
    /// peripheral cue about which database they're on (`red` = prod
    /// is the canonical use).
    pub accent_color: Option<ratatui::style::Color>,
}

pub fn render_root(frame: &mut Frame<'_>, area: Rect, view: &mut RootLayout<'_>) -> LayoutRegions {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(crate::constants::SIDEBAR_WIDTH),
            Constraint::Min(1),
        ])
        .split(outer[0]);

    let sidebar_table_indices = render_sidebar(frame, body[0], &view.sidebar, view.theme);

    let main = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(crate::constants::EDITOR_RESULTS_SPLIT_PCT.0),
            Constraint::Percentage(crate::constants::EDITOR_RESULTS_SPLIT_PCT.1),
        ])
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
        view.editor_sql_highlights,
    );

    // /: when chart or pivot is active, split the
    // result pane area horizontally so the overlay panes stack above
    // the regular table. Chart takes priority when both are active
    // (the more frequent case in demos); pivot takes the same slot
    // when chart is hidden. Allocating both simultaneously is left to
    // a v2.1 follow-up.
    let (chart_area, pivot_area, results_area) = if main[1].height >= 8 {
        if view.chart.is_some() {
            let parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
                .split(main[1]);
            (Some(parts[0]), None, parts[1])
        } else if view.pivot.is_some() {
            let parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
                .split(main[1]);
            (None, Some(parts[0]), parts[1])
        } else {
            (None, None, main[1])
        }
    } else {
        (None, None, main[1])
    };

    if let (Some(area), Some(chart)) = (chart_area, view.chart.as_ref()) {
        match chart {
            Ok(chart_view) => render_chart(frame, area, chart_view, view.theme),
            Err(placeholder) => render_chart_placeholder(frame, area, placeholder, view.theme),
        }
    }
    if let (Some(area), Some(pivot)) = (pivot_area, view.pivot.as_ref()) {
        match pivot {
            Ok(pivot_view) => render_pivot(frame, area, pivot_view, view.theme),
            Err(placeholder) => render_pivot_placeholder(frame, area, placeholder, view.theme),
        }
    }

    let result_regions = render_results(
        frame,
        results_area,
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
        results: results_area,
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
        Mode::OperatorPending(_) => view.theme.mode_command(),
        Mode::Normal => view.theme.mode_normal(),
        // Future vim modes fall back to the normal style until styled explicitly.
        _ => view.theme.mode_normal(),
    };

    let mode_label = if view.show_mode_indicator {
        let label = view
            .mode_label_override
            .map_or_else(|| view.mode.short_label().to_owned(), str::to_owned);
        format!(" {label} ")
    } else {
        String::new()
    };
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

    let pending_text = match view.status_bar.pending {
        Some(n) if n > 0 => format!(" ⏳{n} pending "),
        _ => String::new(),
    };
    let pending_width = pending_text.width() as u16;

    let ro_text = if view.status_bar.read_only {
        " [RO] ".to_owned()
    } else {
        String::new()
    };
    let ro_width = ro_text.width() as u16;

    let running_prefix = if view.running { "⏳ " } else { "" };
    let msg_text = format!(" {}{}", running_prefix, view.status_bar.message);
    let msg_width: u16 = 20; // minimum for the right slot

    let parts = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(left_width),
            Constraint::Length(conn_width),
            Constraint::Length(ro_width),
            Constraint::Length(txn_width),
            Constraint::Length(pending_width),
            Constraint::Min(msg_width),
        ])
        .split(area);

    // Left slot: mode + focus pane
    frame.render_widget(Paragraph::new(left_text).style(mode_style), parts[0]);

    // Center slot: connection (sticky). When the connection declared
    // an accent colour, tint the slot so the user has a
    // constant peripheral cue about which database they're on.
    let conn_style = match view.accent_color {
        Some(c) => view
            .theme
            .status_bar()
            .bg(c)
            .fg(ratatui::style::Color::Black)
            .add_modifier(ratatui::style::Modifier::BOLD),
        None => view.theme.status_bar(),
    };
    frame.render_widget(Paragraph::new(conn_text).style(conn_style), parts[1]);

    // L36 #11: read-only badge sits between the connection and the
    // transaction slot so it's the very first thing the eye picks up
    // after the connection name — mirrors the visual priority of a
    // production banner.
    if !ro_text.is_empty() {
        frame.render_widget(
            Paragraph::new(ro_text).style(view.theme.transaction_badge()),
            parts[2],
        );
    }

    // Optional fourth slot: transaction badge (yellow text)
    if !txn_text.is_empty() {
        frame.render_widget(
            Paragraph::new(txn_text).style(view.theme.transaction_badge()),
            parts[3],
        );
    }

    // Optional fifth slot: pending mutations badge (L36). Reuses the
    // transaction-badge style so both "uncommitted state" cues read
    // the same way to the user.
    if !pending_text.is_empty() {
        frame.render_widget(
            Paragraph::new(pending_text).style(view.theme.transaction_badge()),
            parts[4],
        );
    }

    // Right slot: message
    frame.render_widget(
        Paragraph::new(msg_text).style(view.theme.status_bar()),
        parts[5],
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
