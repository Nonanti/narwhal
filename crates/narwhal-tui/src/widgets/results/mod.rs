//! Tabular result viewer. The big monolithic file used to live here;
//! see the submodules under this directory for sort, model, render,
//! cell helpers, schema detail, popups, and the table painter.

mod cells;
mod model;
mod popups;
mod schema_detail;
mod sort;
mod table_paint;

pub use cells::sanitize_for_display;
pub use model::{
    CellEditView, CellPopup, ExplainPlanLine, ResultDisplay, ResultHitRegions, ResultView,
    SearchHighlight,
};
pub use sort::{compare_values, SortDir};

use popups::draw_explain;
use schema_detail::draw_table_detail;
use table_paint::draw_table;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Paragraph, Wrap,
};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

#[allow(clippy::too_many_arguments)]
pub fn render_results(
    frame: &mut Frame<'_>,
    area: Rect,
    display: &ResultDisplay<'_>,
    view: &mut ResultView,
    theme: &Theme,
    focused: bool,
    result_count: usize,
    active_result: usize,
) -> ResultHitRegions {
    let border_style = if focused {
        Style::default().fg(theme.accent)
    } else {
        Style::default().fg(theme.muted)
    };
    let title = build_title(display, view);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Render the result tab strip when there are multiple results.
    let tab_rects = if result_count > 1 {
        let strip_area = Rect { height: 1, ..inner };
        let mut rects = Vec::with_capacity(result_count);
        let mut x = strip_area.x;
        for i in 0..result_count {
            let label = format!(" result {}/{} ", i + 1, result_count);
            let label_width = label.len() as u16;
            let is_active = i == active_result;
            let style = if is_active {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
            };
            let tab_rect = Rect {
                x,
                y: strip_area.y,
                width: label_width.min(strip_area.width.saturating_sub(x - strip_area.x)),
                height: 1,
            };
            let span = Span::styled(label, style);
            frame.render_widget(Paragraph::new(span), tab_rect);
            if tab_rect.width > 0 {
                rects.push((tab_rect, i));
            }
            x += label_width;
            if x >= strip_area.x + strip_area.width {
                break;
            }
        }
        rects
    } else {
        Vec::new()
    };

    let content_area = if result_count > 1 {
        Rect {
            y: inner.y + 1,
            height: inner.height.saturating_sub(1),
            ..inner
        }
    } else {
        inner
    };

    let mut regions = match display {
        ResultDisplay::Empty => {
            let p = Paragraph::new(Span::styled(
                "  no results yet — F5 / Alt-Enter runs cursor statement, F6 runs whole buffer, Ctrl-Space completes",
                Style::default().fg(theme.muted),
            ))
            .wrap(Wrap { trim: false });
            frame.render_widget(p, content_area);
            ResultHitRegions::default()
        }
        ResultDisplay::Running {
            sql, columns, rows, ..
        } => {
            if columns.is_empty() {
                let p = Paragraph::new(vec![Line::from(Span::styled(
                    format!("  ⏳ running: {sql}"),
                    Style::default().fg(theme.muted),
                ))]);
                frame.render_widget(p, content_area);
                ResultHitRegions::default()
            } else {
                draw_table(frame, content_area, columns, rows, None, theme, view)
            }
        }
        ResultDisplay::Affected {
            rows, elapsed_ms, ..
        } => {
            let msg = format!(
                "  {} row{} affected · {} ms",
                rows,
                if *rows == 1 { "" } else { "s" },
                elapsed_ms
            );
            let p = Paragraph::new(Span::styled(msg, Style::default().fg(theme.foreground)));
            frame.render_widget(p, content_area);
            ResultHitRegions::default()
        }
        ResultDisplay::Rows {
            columns,
            rows,
            search,
            ..
        } => draw_table(frame, content_area, columns, rows, *search, theme, view),
        ResultDisplay::TableDetail { schema } => {
            draw_table_detail(frame, content_area, schema, theme);
            ResultHitRegions::default()
        }
        ResultDisplay::Explain {
            lines,
            planning_time_ms,
            execution_time_ms,
        } => {
            draw_explain(
                frame,
                content_area,
                lines,
                *planning_time_ms,
                *execution_time_ms,
                theme,
            );
            ResultHitRegions::default()
        }
        ResultDisplay::Cancelled {
            rows_so_far,
            elapsed_ms,
        } => {
            let msg = format!(
                "  cancelled at {} rows · {} ms",
                format_count(*rows_so_far),
                elapsed_ms
            );
            let p = Paragraph::new(Span::styled(msg, Style::default().fg(theme.muted)));
            frame.render_widget(p, inner);
            ResultHitRegions::default()
        }
        ResultDisplay::Error {
            message,
            elapsed_ms,
        } => {
            let p = Paragraph::new(vec![Line::from(Span::styled(
                format!("  error ({elapsed_ms} ms): {message}"),
                Style::default().fg(theme.error),
            ))]);
            frame.render_widget(p, content_area);
            ResultHitRegions::default()
        }
    };
    regions.tabs = tab_rects;
    regions
}

fn format_count(n: usize) -> String {
    // The M threshold compares against the value that *rounds up* into
    // the next unit so 999_999 displays as `1.0M`, not `1000.0k`
    // (L18). The k threshold stays at 10_000 to match the previous
    // small-number boundary.
    if n >= 999_500 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn format_elapsed(d: std::time::Duration) -> String {
    let total = d.as_secs_f64();
    // Anything that would round up to 60.0s belongs in mm:ss
    // form (L19); 59.999s used to print as `60.0s`.
    if total < 59.95 {
        format!("{total:.1}s")
    } else {
        let total_secs = total.round() as u64;
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        format!("{mins:02}:{secs:02}")
    }
}


fn build_title(display: &ResultDisplay<'_>, view: &ResultView) -> String {
    let base = match display {
        ResultDisplay::Empty => " results ".into(),
        ResultDisplay::Running {
            index: _,
            total: _,
            rows,
            streaming: true,
            started_at,
            ..
        } => {
            let count = format_count(rows.len());
            let elapsed = format_elapsed(started_at.elapsed());
            format!(" streaming · {count} rows · {elapsed} ")
        }
        ResultDisplay::Running {
            index, total, rows, ..
        } => format!(" results · running {index}/{total} · {} rows ", rows.len()),
        ResultDisplay::Affected {
            index,
            total,
            elapsed_ms,
            ..
        } => format!(" results · {index}/{total} · {elapsed_ms} ms "),
        ResultDisplay::Rows {
            index,
            total,
            rows,
            elapsed_ms,
            streamed,
            ..
        } => {
            if *streamed {
                let count = format_count(rows.len());
                format!(" results · {count} rows · {elapsed_ms}ms ")
            } else {
                let badge = "exec";
                format!(
                    " results · {index}/{total} · {} rows · {elapsed_ms} ms · {badge} ",
                    rows.len()
                )
            }
        }
        ResultDisplay::Explain {
            execution_time_ms, ..
        } => match execution_time_ms {
            Some(ms) => format!(" results · explain · {ms:.3} ms "),
            None => " results · explain ".to_owned(),
        },
        ResultDisplay::TableDetail { schema } => {
            let qualifier = if schema.table.schema.is_empty() {
                String::new()
            } else {
                format!("{}.", schema.table.schema)
            };
            format!(
                " {}{} · {} cols · {} idx · {} fk ",
                qualifier,
                schema.table.name,
                schema.columns.len(),
                schema.indexes.len(),
                schema.foreign_keys.len()
            )
        }
        ResultDisplay::Cancelled {
            rows_so_far,
            elapsed_ms,
        } => {
            let count = format_count(*rows_so_far);
            format!(" cancelled at {count} rows · {elapsed_ms}ms ")
        }
        ResultDisplay::Error { elapsed_ms, .. } => format!(" results · error · {elapsed_ms} ms "),
    };
    // Append filter tag when a filter is active but the prompt is closed.
    if !view.filter.is_empty() && !view.filter_prompt_open {
        format!("{base}[filter: {}] ", view.filter)
    } else {
        base
    }
}


#[cfg(test)]
mod tests {
    use std::cmp::Ordering;
    use std::time::Duration;

    use narwhal_core::Value;

    use super::cells::{is_dangerous_glyph, render_for_grid};
    use super::*;

    #[test]
    fn format_count_small() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(9999), "9999");
    }

    #[test]
    fn format_count_k_suffix() {
        assert_eq!(format_count(10_000), "10.0k");
        assert_eq!(format_count(12_345), "12.3k");
        // The boundary case that triggered L18: 999_499 still rounds
        // *down* to "999.5k" (within the k bucket); 999_500 promotes.
        assert_eq!(format_count(999_499), "999.5k");
    }

    #[test]
    fn format_count_m_suffix() {
        // L18 boundary: 999_500 rounds up into the M bucket.
        assert_eq!(format_count(999_500), "1.0M");
        assert_eq!(format_count(999_999), "1.0M");
        assert_eq!(format_count(1_000_000), "1.0M");
        assert_eq!(format_count(1_234_567), "1.2M");
        assert_eq!(format_count(12_345_678), "12.3M");
    }

    #[test]
    fn format_elapsed_under_60s() {
        assert_eq!(format_elapsed(Duration::from_millis(0)), "0.0s");
        assert_eq!(format_elapsed(Duration::from_millis(100)), "0.1s");
        assert_eq!(format_elapsed(Duration::from_millis(2100)), "2.1s");
        // L19 boundary case: 59.949s stays in the seconds bucket.
        assert_eq!(format_elapsed(Duration::from_millis(59_949)), "59.9s");
    }

    #[test]
    fn format_elapsed_over_60s() {
        // L19: 59.95s and above now promote to mm:ss; previously 59.999s
        // printed as "60.0s".
        assert_eq!(format_elapsed(Duration::from_millis(59_950)), "01:00");
        assert_eq!(format_elapsed(Duration::from_millis(59_999)), "01:00");
        assert_eq!(format_elapsed(Duration::from_secs(60)), "01:00");
        assert_eq!(format_elapsed(Duration::from_secs(125)), "02:05");
        assert_eq!(format_elapsed(Duration::from_secs(3661)), "61:01");
    }

    #[test]
    fn sanitize_replaces_bidi_override() {
        // U+202E RIGHT-TO-LEFT OVERRIDE — Trojan Source attack vector.
        let input = "SELECT \u{202E}1;";
        let output = render_for_grid(input);
        assert_eq!(output, "SELECT ·1;");
    }

    #[test]
    fn sanitize_replaces_control_chars() {
        // NULL, BEL, BS, VT, FF, DEL
        let input = "a\u{0000}b\u{0007}c\u{0008}d\u{000B}e\u{000C}f\u{007F}g";
        let output = render_for_grid(input);
        assert_eq!(output, "a·b·c·d·e·f·g");
    }

    #[test]
    fn sanitize_preserves_normal_unicode() {
        let input = "Türkçe 中文 🦄";
        let output = render_for_grid(input);
        assert_eq!(output, input);
    }

    #[test]
    fn sanitize_handles_newline_and_bidi_combined() {
        let input = "a\n\u{202E}b";
        let output = render_for_grid(input);
        assert_eq!(output, "a⏎·b");
    }

    #[test]
    fn compare_json_structural_matches_string_for_equal_inputs() {
        let a = serde_json::json!({"id": 1, "tags": ["a", "b"]});
        let b = serde_json::json!({"id": 1, "tags": ["a", "b"]});
        assert_eq!(
            compare_values(Some(&Value::Json(a)), Some(&Value::Json(b))),
            Ordering::Equal
        );
    }

    #[test]
    fn compare_json_orders_by_first_differing_field() {
        let a = serde_json::json!({"name": "alice"});
        let b = serde_json::json!({"name": "bob"});
        assert_eq!(
            compare_values(Some(&Value::Json(a)), Some(&Value::Json(b))),
            Ordering::Less
        );
    }

    #[test]
    fn compare_json_orders_numbers_numerically_not_lexically() {
        // String-based compare would put "10" before "2"; structural
        // compare orders 2 < 10.
        let a = serde_json::json!(2);
        let b = serde_json::json!(10);
        assert_eq!(
            compare_values(Some(&Value::Json(a)), Some(&Value::Json(b))),
            Ordering::Less
        );
    }

    #[test]
    fn compare_json_arrays_use_lexicographic_order() {
        let a = serde_json::json!([1, 2, 3]);
        let b = serde_json::json!([1, 2, 4]);
        let c = serde_json::json!([1, 2]);
        assert_eq!(
            compare_values(Some(&Value::Json(a.clone())), Some(&Value::Json(b))),
            Ordering::Less
        );
        assert_eq!(
            compare_values(Some(&Value::Json(c)), Some(&Value::Json(a))),
            Ordering::Less
        );
    }

    #[test]
    fn compare_json_different_kinds_use_type_rank() {
        // bool ranks below string, regardless of payload.
        let a = serde_json::json!(true);
        let b = serde_json::json!("a");
        assert_eq!(
            compare_values(Some(&Value::Json(a)), Some(&Value::Json(b))),
            Ordering::Less
        );
    }
}
