//! Type-`YES` confirmation modal renderer (v1.1 #2).
//!
//! Drawn on top of everything else when a connection with
//! `confirm_writes = true` is about to run a mutating statement. The
//! state coming in is intentionally narrow: a prompt body, an accept
//! keyword, and the user's accumulated buffer. Match-status is
//! computed here so the renderer can colour the input cue red/green
//! without duplicating the modal struct.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::theme::Theme;

/// Borrowed view of the confirmation modal.
pub struct ConfirmModalView<'a> {
    /// Multi-line prompt body. Newlines are honoured.
    pub prompt: &'a str,
    /// Expected accept keyword (e.g. `"YES"`).
    pub accept_keyword: &'a str,
    /// What the user has typed so far.
    pub buffer: &'a str,
    /// `true` when `buffer` matches `accept_keyword` exactly
    /// (case-insensitive, trimmed). Drives the input border colour.
    pub satisfied: bool,
}

/// Render the confirm overlay centred on `area`. Caller is
/// responsible for opening / closing the modal in state; this
/// function only draws.
pub fn render_confirm_modal(
    frame: &mut Frame<'_>,
    area: Rect,
    view: &ConfirmModalView<'_>,
    theme: &Theme,
) {
    let width = (area.width * 7 / 10).clamp(40, 90);
    let height: u16 = 12;
    if area.width < 30 || area.height < height {
        return;
    }
    let popup = centred(area, width, height);
    frame.render_widget(Clear, popup);

    // Outer block — red border to grab attention, regardless of theme.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(ratatui::style::Color::Red)
                .add_modifier(Modifier::BOLD),
        )
        .title(Span::styled(
            " confirm write · esc cancels ",
            Style::default()
                .fg(ratatui::style::Color::Red)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // Split inner area: prompt body (top) + input cue (bottom 3 rows).
    let prompt_h = inner.height.saturating_sub(3);
    let prompt_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: prompt_h,
    };
    let input_area = Rect {
        x: inner.x,
        y: inner.y + prompt_h,
        width: inner.width,
        height: inner.height - prompt_h,
    };

    let prompt_lines: Vec<Line<'_>> = view
        .prompt
        .lines()
        .map(|line| Line::from(Span::styled(line, Style::default().fg(theme.foreground))))
        .collect();
    frame.render_widget(
        Paragraph::new(prompt_lines).wrap(Wrap { trim: false }),
        prompt_area,
    );

    let input_border = if view.satisfied {
        ratatui::style::Color::Green
    } else {
        ratatui::style::Color::Yellow
    };
    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(input_border))
        .title(Span::styled(
            format!(" type {} + Enter ", view.accept_keyword),
            Style::default().fg(input_border),
        ));
    let input_inner = input_block.inner(input_area);
    frame.render_widget(input_block, input_area);

    // Caret block (`▌`) at the end so the user sees a cursor without
    // us touching the terminal cursor API (which sometimes conflicts
    // with the inner editor's cursor positioning).
    let caret = "\u{258C}";
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(view.buffer, Style::default().fg(theme.foreground)),
            Span::styled(caret, Style::default().fg(input_border)),
        ])),
        input_inner,
    );
}

fn centred(area: Rect, width: u16, height: u16) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}
