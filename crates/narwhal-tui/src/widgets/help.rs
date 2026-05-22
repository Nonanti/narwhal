//! Help-panel modal renderer and static cheatsheet data.
//!
//! The cheatsheet is a compile-time constant — no introspection from the
//! keymap struct in v1. When bindings change, update this file by hand so
//! the docs stay in sync. The snapshot test (`snapshot_help_modal`) will
//! catch accidental drift.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::theme::Theme;

/// One row in the cheatsheet table.
pub struct CheatsheetEntry {
    pub keys: &'static str,
    pub description: &'static str,
}

/// One section of the cheatsheet (e.g. "Global", "Editor").
pub struct CheatsheetSection {
    pub title: &'static str,
    pub entries: &'static [CheatsheetEntry],
}

/// All sections, in display order.
///
/// Bindings listed here are verified against the actual key-handling code.
/// When a new binding is added to `AppCore::handle_global_key`,
/// `handle_editor_key`, or `handle_results_key`, update the matching
/// section below and re-run the snapshot test.
pub const CHEATSHEET: &[CheatsheetSection] = &[
    CheatsheetSection {
        title: "Global",
        entries: &[
            CheatsheetEntry {
                keys: "F5 / Alt-Enter / Ctrl-;",
                description: "run statement under cursor",
            },
            CheatsheetEntry {
                keys: "F6",
                description: "run whole buffer",
            },
            CheatsheetEntry {
                keys: "F7",
                description: "stream cursor statement",
            },
            CheatsheetEntry {
                keys: "F4 / Ctrl-C",
                description: "cancel running query",
            },
            CheatsheetEntry {
                keys: "Ctrl-W",
                description: "cycle pane focus",
            },
            CheatsheetEntry {
                keys: ":",
                description: "command palette (any pane)",
            },
            CheatsheetEntry {
                keys: "Ctrl-T",
                description: "new editor tab",
            },
            CheatsheetEntry {
                keys: "Ctrl-Tab / Ctrl-Shift-Tab",
                description: "cycle tabs",
            },
            CheatsheetEntry {
                keys: "? / F1",
                description: "this help",
            },
            CheatsheetEntry {
                keys: ":q",
                description: "quit",
            },
            CheatsheetEntry {
                keys: ":refresh",
                description: "re-fetch schema tree for active connection",
            },
            CheatsheetEntry {
                keys: ":format / :fmt",
                description: "pretty-print the statement under the cursor",
            },
            CheatsheetEntry {
                keys: ":format-all / :fmtall",
                description: "pretty-print every statement in the buffer",
            },
        ],
    },
    CheatsheetSection {
        title: "Editor",
        entries: &[
            CheatsheetEntry {
                keys: "i / a",
                description: "enter insert mode",
            },
            CheatsheetEntry {
                keys: "Esc",
                description: "back to normal mode",
            },
            CheatsheetEntry {
                keys: "Tab / Ctrl-Space",
                description: "completion",
            },
            CheatsheetEntry {
                keys: "↑ ↓ / Shift-Tab",
                description: "cycle popup items",
            },
            CheatsheetEntry {
                keys: "Enter / Tab (in popup)",
                description: "accept completion",
            },
            CheatsheetEntry {
                keys: "h j k l / arrows",
                description: "move cursor",
            },
            CheatsheetEntry {
                keys: "w / b",
                description: "word forward / backward",
            },
            CheatsheetEntry {
                keys: "0 / $",
                description: "line start / end",
            },
            CheatsheetEntry {
                keys: "v / V",
                description: "visual / visual-line mode",
            },
        ],
    },
    CheatsheetSection {
        title: "Sidebar",
        entries: &[
            CheatsheetEntry {
                keys: "j / k / ↑ / ↓",
                description: "navigate",
            },
            CheatsheetEntry {
                keys: "Enter",
                description: "describe table",
            },
            CheatsheetEntry {
                keys: "o",
                description: "preview table data",
            },
            CheatsheetEntry {
                keys: "d",
                description: "inject DDL into editor",
            },
        ],
    },
    CheatsheetSection {
        title: "Results",
        entries: &[
            CheatsheetEntry {
                keys: "h j k l / arrows",
                description: "move selection",
            },
            CheatsheetEntry {
                keys: "Enter",
                description: "open cell popup",
            },
            CheatsheetEntry {
                keys: "e",
                description: "edit cell value",
            },
            CheatsheetEntry {
                keys: "y / Y",
                description: "yank cell / row to clipboard",
            },
            CheatsheetEntry {
                keys: "/",
                description: "filter rows",
            },
            CheatsheetEntry {
                keys: "n / N",
                description: "next / prev search match",
            },
            CheatsheetEntry {
                keys: "g / G",
                description: "jump to first / last row",
            },
            CheatsheetEntry {
                keys: ":next / :prev",
                description: "page through results",
            },
        ],
    },
    CheatsheetSection {
        title: "Connections",
        entries: &[
            CheatsheetEntry {
                keys: ":add",
                description: "open the connection wizard (empty form)",
            },
            CheatsheetEntry {
                keys: ":url <dsn>",
                description: "prefill the wizard from a connection URL",
            },
            CheatsheetEntry {
                keys: ":test [name|url]",
                description: "dry-run a connection without opening a session",
            },
            CheatsheetEntry {
                keys: ":edit <name>",
                description: "edit a saved connection in the wizard",
            },
            CheatsheetEntry {
                keys: ":open <name|url>",
                description: "connect to a saved entry or an ad-hoc URL",
            },
            CheatsheetEntry {
                keys: ":remove <name>",
                description: "delete a saved connection (also :rm)",
            },
            CheatsheetEntry {
                keys: "ssh tunnel",
                description: "fill ssh_host + ssh_user in :add (or ?ssh_host=… in :url)",
            },
            CheatsheetEntry {
                keys: "pgpass / env",
                description: "PGPASSWORD / MYSQL_PWD / ~/.pgpass picked up automatically",
            },
            CheatsheetEntry {
                keys: "Tab on path field",
                description: "filesystem completion in the wizard",
            },
        ],
    },
    CheatsheetSection {
        title: "Snippets",
        entries: &[
            CheatsheetEntry {
                keys: ":save <name>",
                description: "save editor buffer as a named snippet",
            },
            CheatsheetEntry {
                keys: ":load <name>",
                description: "load a snippet into a new tab",
            },
            CheatsheetEntry {
                keys: ":rm-snippet <name>",
                description: "delete a saved snippet",
            },
            CheatsheetEntry {
                keys: ":snippets",
                description: "browse saved snippets",
            },
        ],
    },
];

/// Render the help modal on top of the current frame.
///
/// The modal occupies a centred rectangle (max 60×24, otherwise 70% of
/// available space) and displays each cheatsheet section as a labelled
/// two-column table (shortcut → description).
pub fn render_help_modal(frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
    let (max_width, max_height) = crate::constants::HELP_MODAL_MAX;
    let width = (area.width * 8 / 10).min(max_width);
    let height = (area.height * 9 / 10).min(max_height);
    if width < 30 || height < 8 {
        return;
    }
    let popup = centred(area, width, height);
    frame.render_widget(Clear, popup);

    let title = " help · esc closes ";
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines: Vec<Line<'_>> = Vec::new();
    let key_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(theme.foreground);
    let heading_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);

    for section in CHEATSHEET {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            format!(" {} ", section.title),
            heading_style,
        )));
        for entry in section.entries {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<28}", entry.keys), key_style),
                Span::styled(entry.description, desc_style),
            ]));
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

pub(crate) use super::centred_rect as centred;
