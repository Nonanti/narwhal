//! History modal renderer (Ctrl+R).
//!
//! Renders a centred modal overlay listing recent journal entries.
//! The caller provides a [`HistoryModalState`] with entries, filter
//! string, and selected index; rendering is pure — no I/O.
//!
//! L36 #5: rows now carry the entry's outcome (rendered as a coloured
//! status glyph), elapsed milliseconds, and a rows-affected /
//! rows-returned summary so the user can spot slow queries and
//! failed statements without paging the underlying journal.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::theme::Theme;
use crate::widgets::help::centred;
use crate::widgets::results::sanitize_for_display;
use unicode_width::UnicodeWidthStr;

/// View model passed from `AppCore` to the render path. Owns only the
/// data needed for display; the full `HistoryState` stays in the core.
pub struct HistoryModalState<'a> {
    /// All loaded entries (used to compute `total`).
    pub total: usize,
    /// The filtered subset to render.
    pub visible: Vec<HistoryRow<'a>>,
    /// Current filter string.
    pub filter: &'a str,
    /// Index into the `visible` list.
    pub selected: usize,
}

/// Outcome marker shown as a coloured glyph in front of every row.
/// Mirrors `narwhal_history::Outcome` but kept locally so the tui
/// crate doesn't have to depend on the history crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryRowOutcome {
    Success,
    Cancelled,
    Failed,
}

/// One row in the history modal.
pub struct HistoryRow<'a> {
    /// Formatted timestamp: `YYYY-MM-DD HH:MM:SS`.
    pub timestamp: &'a str,
    /// Connection name (or `"<local>"` if absent).
    pub connection: &'a str,
    /// Single-line SQL preview (pre-truncated by the caller).
    pub sql: &'a str,
    /// L36 #5: outcome glyph colour.
    pub outcome: HistoryRowOutcome,
    /// Pre-formatted elapsed timing (`"12ms"`, `"1.4s"`, ...).
    pub elapsed: &'a str,
    /// Pre-formatted rows summary (`"↓42"` for returned, `"~3"` for
    /// affected, empty for none).
    pub rows: &'a str,
}

/// Render the history modal on top of the current frame.
///
/// The modal occupies a centred rectangle (60% width × 70% height,
/// capped at 80×24) and displays a three-column table:
/// timestamp | connection | SQL preview.
///
/// The selected row is rendered in reverse video.
pub fn render_history_modal(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &HistoryModalState<'_>,
    theme: &Theme,
) {
    let (max_width, max_height) = crate::constants::HISTORY_MODAL_MAX;
    let width = (area.width * 6 / 10).min(max_width);
    let height = (area.height * 7 / 10).min(max_height);
    if width < 30 || height < 6 {
        return;
    }
    let popup = centred(area, width, height);
    frame.render_widget(Clear, popup);

    let total = state.total;
    let visible_count = state.visible.len();
    let title = format!(
        " history · {visible_count}/{total}  filter: {}_ ",
        if state.filter.is_empty() {
            ""
        } else {
            state.filter
        }
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            &title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let outcome_width: usize = 1;
    let timestamp_width: usize = 19;
    let connection_width: usize = 12;
    let elapsed_width: usize = 7;
    let rows_width: usize = 7;
    let sql_min_width: usize = 20;
    let inner_width = inner.width as usize;
    let sql_width = inner_width
        .saturating_sub(outcome_width)
        .saturating_sub(timestamp_width)
        .saturating_sub(connection_width)
        .saturating_sub(elapsed_width)
        .saturating_sub(rows_width)
        .saturating_sub(10) // padding/separators
        .max(sql_min_width);

    let mut lines: Vec<Line<'_>> = Vec::new();
    let header_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let normal_style = Style::default().fg(theme.foreground);
    let selected_style = Style::default()
        .bg(theme.accent)
        .fg(ratatui::style::Color::Black);

    // Column header.
    lines.push(Line::from(vec![
        Span::styled(format!(" {:outcome_width$}", "·"), header_style),
        Span::styled(format!(" {:timestamp_width$}", "TIMESTAMP"), header_style),
        Span::styled(format!(" {:connection_width$}", "CONNECTION"), header_style),
        Span::styled(format!(" {:>elapsed_width$}", "TIME"), header_style),
        Span::styled(format!(" {:>rows_width$}", "ROWS"), header_style),
        Span::styled(format!(" {:sql_width$}", "SQL"), header_style),
    ]));

    for (i, row) in state.visible.iter().enumerate() {
        let style = if i == state.selected {
            selected_style
        } else {
            normal_style
        };
        let (glyph, glyph_colour) = match row.outcome {
            HistoryRowOutcome::Success => ("●", Color::Green),
            HistoryRowOutcome::Cancelled => ("●", Color::Yellow),
            HistoryRowOutcome::Failed => ("●", Color::Red),
        };
        let glyph_style = if i == state.selected {
            selected_style.fg(glyph_colour)
        } else {
            Style::default().fg(glyph_colour)
        };
        let sql_truncated = truncate_display(&sanitize_for_display(row.sql), sql_width);
        let ts = pad_to_width(row.timestamp, timestamp_width);
        let conn = pad_to_width(row.connection, connection_width);
        let elapsed = format!("{:>elapsed_width$}", row.elapsed);
        let rows = format!("{:>rows_width$}", row.rows);
        lines.push(Line::from(vec![
            Span::styled(format!(" {glyph}"), glyph_style),
            Span::styled(format!(" {ts}"), style),
            Span::styled(format!(" {conn}"), style),
            Span::styled(format!(" {elapsed}"), style),
            Span::styled(format!(" {rows}"), style),
            Span::styled(format!(" {sql_truncated}"), style),
        ]));
    }

    // Pad remaining lines so the selection highlight fills the width.
    let body_height = inner.height.saturating_sub(1) as usize; // minus header
    while lines.len() < body_height + 1 {
        lines.push(Line::from(""));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

/// L36 #m2: format a millisecond duration the way the history modal
/// wants to see it. Lives in the widget crate (alongside the
/// columns that consume it) so the formatting choices stay close to
/// the rendered layout.
///
/// Output shape:
///
/// * `0`      → `"-"` (entry predates timing capture)
/// * `< 1s`   → `"12ms"`
/// * `< 1m`   → `"1.4s"` (one tenth of a second)
/// * `≥ 1m`   → `"1m23s"` (drops the tenths in the minutes branch —
///   anything that took a minute is already "slow" and the user just
///   wants to know the order of magnitude)
#[must_use]
pub fn format_elapsed(ms: u64) -> String {
    if ms == 0 {
        return "-".into();
    }
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    let total_secs = ms / 1_000;
    if total_secs < 60 {
        let tenths = (ms % 1_000) / 100;
        return format!("{total_secs}.{tenths}s");
    }
    let minutes = total_secs / 60;
    let seconds = total_secs % 60;
    format!("{minutes}m{seconds:02}s")
}

/// L36 #m2: format the rows-returned / rows-affected pair into a
/// single column.
///
/// * `↓N` for rows returned (the SELECT-style case)
/// * `∼N` for rows affected (the UPDATE/DELETE-style case)
/// * empty when both are absent
///
/// Returned takes precedence so a SELECT that *also* set
/// `rows_affected` (some drivers do) still shows the result-set
/// size.
#[must_use]
pub fn format_rows(returned: Option<u64>, affected: Option<u64>) -> String {
    if let Some(r) = returned {
        return format!("↓{r}");
    }
    if let Some(a) = affected {
        return format!("∼{a}");
    }
    String::new()
}

/// Truncate a string so its **display width** does not exceed `max_width`
/// cells, appending `…` if truncated. Uses `unicode_width` so CJK,
/// emoji, and other wide characters are counted correctly.
fn truncate_display(s: &str, max_width: usize) -> String {
    if s.width() <= max_width {
        return s.to_owned();
    }
    // Sprint 10 (LOW): use `UnicodeWidthChar::width` so we avoid the
    // per-char `String` allocation the old `ch.to_string().as_str()`
    // pattern did. A 1000-row history modal previously allocated
    // ~10k throwaway strings just to pretty-print SQL column.
    use unicode_width::UnicodeWidthChar;
    let mut out = String::with_capacity(s.len().min(max_width * 4));
    let mut w = 0;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw + 1 > max_width {
            out.push('…');
            break;
        }
        out.push(ch);
        w += cw;
    }
    out
}

/// Pad a string with trailing spaces so its **display width** equals
/// `target_width` cells. Handles wide characters correctly by computing
/// the difference between display width and target.
///
/// L36 #m4: stayed on `repeat(…).take(N)` because the workspace MSRV
/// is 1.75; `iter::repeat_n` is only available from 1.82. Worth
/// revisiting when the MSRV moves — it's a one-line swap.
fn pad_to_width(s: &str, target_width: usize) -> String {
    let display_w = s.width();
    let mut out = s.to_owned();
    let need = target_width.saturating_sub(display_w);
    out.extend(std::iter::repeat_n(' ', need));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_display_short() {
        assert_eq!(truncate_display("hello", 10), "hello");
    }

    #[test]
    fn truncate_display_exact() {
        assert_eq!(truncate_display("hello", 5), "hello");
    }

    #[test]
    fn truncate_display_long() {
        let result = truncate_display("hello world", 6);
        assert_eq!(result, "hello…");
    }

    #[test]
    fn truncate_display_respects_wide_chars() {
        // CJK character '中' has display width 2.
        // "中日" = 4 display cells. Truncating to 3 should yield "中…" (2+1=3).
        let result = truncate_display("中日", 3);
        assert_eq!(result, "中…");
    }

    #[test]
    fn pad_to_width_handles_wide_chars() {
        // '中' = 2 cells wide. Pad to 5 → need 3 spaces.
        let result = pad_to_width("中", 5);
        assert_eq!(result, "中   ");
    }

    #[test]
    fn pad_to_width_ascii() {
        let result = pad_to_width("abc", 6);
        assert_eq!(result, "abc   ");
    }

    // L36 #m2: tests live with the formatter now that it ships from
    // the widget crate. The host-side dispatch helper is gone.

    #[test]
    fn format_elapsed_thresholds() {
        assert_eq!(format_elapsed(0), "-");
        assert_eq!(format_elapsed(7), "7ms");
        assert_eq!(format_elapsed(999), "999ms");
        assert_eq!(format_elapsed(1_000), "1.0s");
        assert_eq!(format_elapsed(1_450), "1.4s");
        assert_eq!(format_elapsed(59_999), "59.9s");
        assert_eq!(format_elapsed(60_000), "1m00s");
        assert_eq!(format_elapsed(83_000), "1m23s");
    }

    #[test]
    fn format_rows_variants() {
        assert_eq!(format_rows(Some(42), None), "↓42");
        assert_eq!(format_rows(None, Some(3)), "∼3");
        assert_eq!(format_rows(None, None), "");
        assert_eq!(format_rows(Some(1), Some(2)), "↓1");
    }
}
