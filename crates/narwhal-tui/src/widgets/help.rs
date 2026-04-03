//! Help-panel modal renderer and static cheatsheet data.
//!
//! The cheatsheet is a compile-time constant — no introspection from the
//! keymap struct in v1. When bindings change, update this file by hand so
//! the docs stay in sync. The snapshot test (`snapshot_help_modal`) will
//! catch accidental drift.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

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
        title: "Editor (vim)",
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
            // ─── L36: row CRUD + pending changes ──────────────
            CheatsheetEntry {
                keys: "o / O",
                description: "queue INSERT (empty / duplicate row)",
            },
            CheatsheetEntry {
                keys: "d",
                description: "queue DELETE for the focused row",
            },
            CheatsheetEntry {
                keys: "Ctrl-S",
                description: "commit every staged mutation in a txn",
            },
            CheatsheetEntry {
                keys: "Ctrl-X",
                description: "discard the staged-mutation queue",
            },
            CheatsheetEntry {
                keys: "Ctrl-P",
                description: "toggle the pending-changes preview modal",
            },
            // ─── L36: metadata tabs ────────────────────────────
            CheatsheetEntry {
                keys: "1 / 2 / 3 / 4 / 5",
                description: "switch metadata tab: Records / Columns / Constraints / FKs / Indexes",
            },
            // ─── L36: JSON viewer ──────────────────────────────
            CheatsheetEntry {
                keys: "z / Z",
                description: "open JSON viewer (cell / whole row)",
            },
            CheatsheetEntry {
                keys: "j/k/Ctrl-D/U/g/G in viewer",
                description: "scroll JSON viewer; y/Y yank, q/Esc close",
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

/// Editor mode hint used to swap which cheatsheet pages get
/// rendered. Only the editor-mode chord set changes; the global
/// shortcuts stay constant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum HelpEditorMode {
    #[default]
    Vim,
    Basic,
    Emacs,
}

/// Cheatsheet entries for basic (modeless) editor mode.
pub const CHEATSHEET_BASIC_EDITOR: &[CheatsheetEntry] = &[
    CheatsheetEntry { keys: "Arrow / Home / End", description: "move cursor" },
    CheatsheetEntry { keys: "Ctrl-Arrow", description: "word / paragraph jump" },
    CheatsheetEntry { keys: "Shift-Arrow", description: "extend selection" },
    CheatsheetEntry { keys: "Ctrl-A", description: "select all" },
    CheatsheetEntry { keys: "Ctrl-C / Ctrl-X", description: "copy / cut selection" },
    CheatsheetEntry { keys: "Ctrl-V", description: "paste clipboard" },
    CheatsheetEntry { keys: "Ctrl-Z / Ctrl-Y", description: "undo / redo" },
    CheatsheetEntry { keys: "Ctrl-F", description: "find in buffer" },
    CheatsheetEntry { keys: "Tab", description: "completion / indent" },
    CheatsheetEntry { keys: ":", description: "open command palette" },
    CheatsheetEntry { keys: "Esc", description: "clear selection / close popups" },
];

/// Cheatsheet entries for emacs editor mode.
pub const CHEATSHEET_EMACS_EDITOR: &[CheatsheetEntry] = &[
    CheatsheetEntry { keys: "C-f / C-b", description: "forward / backward char" },
    CheatsheetEntry { keys: "C-n / C-p", description: "next / previous line" },
    CheatsheetEntry { keys: "C-a / C-e", description: "beginning / end of line" },
    CheatsheetEntry { keys: "M-f / M-b", description: "forward / backward word" },
    CheatsheetEntry { keys: "M-< / M->", description: "beginning / end of buffer" },
    CheatsheetEntry { keys: "C-Space", description: "set mark" },
    CheatsheetEntry { keys: "C-w / M-w", description: "kill / copy region" },
    CheatsheetEntry { keys: "C-y", description: "yank (paste)" },
    CheatsheetEntry { keys: "C-k", description: "kill to end of line" },
    CheatsheetEntry { keys: "C-d / M-d", description: "delete char / word" },
    CheatsheetEntry { keys: "C-/ or C-_", description: "undo" },
    CheatsheetEntry { keys: "C-s / C-r", description: "search forward / backward" },
    CheatsheetEntry { keys: "C-x C-s", description: "submit / run statement" },
    CheatsheetEntry { keys: "C-g", description: "cancel / clear region" },
];

/// Render the help modal on top of the current frame.
///
/// The modal occupies a centred rectangle (max 60×24, otherwise 70% of
/// available space) and displays each cheatsheet section as a labelled
/// two-column table (shortcut → description).
///
/// `editor_mode` swaps the editor-section content between vim,
/// basic and emacs without rebuilding the entire cheatsheet.
pub fn render_help_modal(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    editor_mode: HelpEditorMode,
) {
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
        // Replace the vim editor section with the matching
        // basic / emacs chord set when one of those modes is
        // active; every other section is mode-agnostic.
        let entries: &[CheatsheetEntry] = match (section.title, editor_mode) {
            ("Editor (vim)", HelpEditorMode::Basic) => CHEATSHEET_BASIC_EDITOR,
            ("Editor (vim)", HelpEditorMode::Emacs) => CHEATSHEET_EMACS_EDITOR,
            _ => section.entries,
        };
        let title = match (section.title, editor_mode) {
            ("Editor (vim)", HelpEditorMode::Basic) => "Editor (basic)",
            ("Editor (vim)", HelpEditorMode::Emacs) => "Editor (emacs)",
            (t, _) => t,
        };
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            format!(" {title} "),
            heading_style,
        )));
        for entry in entries {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<28}", entry.keys), key_style),
                Span::styled(entry.description, desc_style),
            ]));
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

pub(crate) use super::centred_rect as centred;
