//! inline ASCII chart widget.
//!
//! Renders a [`ChartView`] as a ratatui [`BarChart`], [`Chart`], or
//! [`Sparkline`] depending on the kind. The widget owns no state — the
//! caller derives a fresh [`ChartView`] from result rows on every frame
//! and hands it in.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Axis, BarChart, Block, Borders, Chart, Dataset, GraphType, Paragraph, Sparkline,
};

use crate::theme::Theme;

/// Which underlying ratatui widget to render. Mirrors
/// `narwhal_app::core::chart::ChartKind` so the host can hand the value
/// through verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartViewKind {
    Bar,
    Line,
    Sparkline,
}

/// Pure-data view for one chart render. Mirrors
/// `narwhal_app::core::chart::ChartData`; sit between the data layer
/// and the ratatui widgets so the TUI crate stays free of
/// `narwhal-core` value types.
#[derive(Debug, Clone)]
pub struct ChartView<'a> {
    pub kind: ChartViewKind,
    pub title: &'a str,
    /// Bar / line: one label per point. Sparkline: empty.
    pub labels: &'a [String],
    /// Numeric values; same length as `labels` for bar / line.
    pub values: &'a [f64],
}

/// Placeholder view rendered when chart derivation fails (no rows,
/// wrong column type, etc). Lives in this module so the host can hand
/// the message straight to the renderer instead of duplicating the
/// "centred dim paragraph" pattern in `render_results`.
#[derive(Debug, Clone)]
pub struct ChartPlaceholder<'a> {
    pub title: &'a str,
    pub message: &'a str,
}

/// Draw the chart pane inside `area`. Surrounds the chart with a thin
/// block; the inner area is the actual plot.
pub fn render_chart(frame: &mut Frame<'_>, area: Rect, view: &ChartView<'_>, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.muted))
        .title(Span::styled(
            format!(" chart · {} ", view.title),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 4 || inner.height < 2 {
        // Not enough room to draw anything useful.
        return;
    }

    match view.kind {
        ChartViewKind::Bar => render_bar(frame, inner, view, theme),
        ChartViewKind::Line => render_line(frame, inner, view, theme),
        ChartViewKind::Sparkline => render_sparkline(frame, inner, view, theme),
    }
}

/// Draw the placeholder block (used when the host has no `ChartView` to
/// pass — e.g. derivation failed). Title matches `render_chart`'s
/// styling so the pane feels consistent.
pub fn render_chart_placeholder(
    frame: &mut Frame<'_>,
    area: Rect,
    placeholder: &ChartPlaceholder<'_>,
    theme: &Theme,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.muted))
        .title(Span::styled(
            format!(" chart · {} ", placeholder.title),
            Style::default().fg(theme.muted),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let para = Paragraph::new(Line::from(Span::styled(
        format!("  {}", placeholder.message),
        Style::default().fg(theme.muted),
    )));
    frame.render_widget(para, inner);
}

fn render_bar(frame: &mut Frame<'_>, area: Rect, view: &ChartView<'_>, theme: &Theme) {
    // BarChart takes &[(&str, u64)]. We round to u64 — anything beyond
    // the integer range smears anyway. Negative values are clamped to
    // 0 since the widget doesn't support a baseline shift in 0.28.
    let owned: Vec<(String, u64)> = view
        .labels
        .iter()
        .zip(view.values.iter())
        .map(|(label, value)| (label.clone(), value.max(0.0).round() as u64))
        .collect();
    let pairs: Vec<(&str, u64)> = owned.iter().map(|(s, v)| (s.as_str(), *v)).collect();

    // Pick a per-bar width that fits everything in the visible area;
    // ratatui requires bar_width >= 1.
    let max_bar_width = area
        .width
        .saturating_sub(pairs.len().saturating_sub(1) as u16)
        .checked_div(pairs.len().max(1) as u16)
        .unwrap_or(1)
        .max(1);
    let bar_width = max_bar_width.min(8);

    let chart = BarChart::default()
        .data(&pairs)
        .bar_width(bar_width)
        .bar_gap(1)
        .bar_style(Style::default().fg(theme.accent))
        .value_style(
            Style::default()
                .fg(theme.background)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
        .label_style(Style::default().fg(theme.foreground));
    frame.render_widget(chart, area);
}

fn render_line(frame: &mut Frame<'_>, area: Rect, view: &ChartView<'_>, theme: &Theme) {
    // Build the dataset as `(x, y)` pairs. X is the row index — line
    // charts are inherently sequential in this MVP; honouring an x
    // column is deferred to v2.1 (needs numeric / time coercion).
    let points: Vec<(f64, f64)> = view
        .values
        .iter()
        .enumerate()
        .map(|(i, v)| (i as f64, *v))
        .collect();

    let (y_min, y_max) = points
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |acc, (_, y)| {
            (acc.0.min(*y), acc.1.max(*y))
        });
    // Guard against degenerate domain (single point or all-equal).
    // The bit-level check avoids clippy's `float_cmp` lint and is the
    // right thing semantically: NaN is unreachable here (`derive_chart_data`
    // filters it out) and the only false negative would be `+0 == -0`,
    // which is harmless — the chart still renders.
    let (y_min, y_max) = if y_min.to_bits() == y_max.to_bits() {
        (y_min - 1.0, y_max + 1.0)
    } else {
        (y_min, y_max)
    };
    let x_max = (points.len().saturating_sub(1) as f64).max(1.0);

    let datasets = vec![
        Dataset::default()
            .name(view.title)
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(theme.accent))
            .data(&points),
    ];

    let x_axis = Axis::default()
        .style(Style::default().fg(theme.muted))
        .bounds([0.0, x_max])
        .labels(vec![
            Span::styled(
                view.labels.first().cloned().unwrap_or_default(),
                Style::default().fg(theme.muted),
            ),
            Span::styled(
                view.labels.last().cloned().unwrap_or_default(),
                Style::default().fg(theme.muted),
            ),
        ]);
    let y_axis = Axis::default()
        .style(Style::default().fg(theme.muted))
        .bounds([y_min, y_max])
        .labels(vec![
            Span::styled(format!("{y_min:.1}"), Style::default().fg(theme.muted)),
            Span::styled(format!("{y_max:.1}"), Style::default().fg(theme.muted)),
        ]);

    let chart = Chart::new(datasets).x_axis(x_axis).y_axis(y_axis);
    frame.render_widget(chart, area);
}

fn render_sparkline(frame: &mut Frame<'_>, area: Rect, view: &ChartView<'_>, theme: &Theme) {
    // Sparkline takes &[u64]; clamp negatives, round to integer.
    let values: Vec<u64> = view
        .values
        .iter()
        .map(|v| v.max(0.0).round() as u64)
        .collect();
    let spark = Sparkline::default()
        .data(&values)
        .style(Style::default().fg(theme.accent));
    frame.render_widget(spark, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render_with(view: ChartView<'_>) -> Terminal<TestBackend> {
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).expect("backend");
        let theme = Theme::default();
        terminal
            .draw(|frame| {
                render_chart(frame, frame.area(), &view, &theme);
            })
            .expect("draw");
        terminal
    }

    #[test]
    fn bar_chart_renders_within_area() {
        let labels = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let values = vec![1.0, 5.0, 3.0];
        let view = ChartView {
            kind: ChartViewKind::Bar,
            title: "test",
            labels: &labels,
            values: &values,
        };
        let _ = render_with(view);
    }

    #[test]
    fn line_chart_renders_with_degenerate_domain() {
        // All-equal values: y_min == y_max guard kicks in.
        let labels = vec!["1".to_owned(), "2".to_owned(), "3".to_owned()];
        let values = vec![7.0, 7.0, 7.0];
        let view = ChartView {
            kind: ChartViewKind::Line,
            title: "flat",
            labels: &labels,
            values: &values,
        };
        let _ = render_with(view);
    }

    #[test]
    fn sparkline_renders_with_negative_values_clamped() {
        let labels: Vec<String> = Vec::new();
        let values = vec![-3.0, 0.0, 5.0, 2.0];
        let view = ChartView {
            kind: ChartViewKind::Sparkline,
            title: "spark",
            labels: &labels,
            values: &values,
        };
        let _ = render_with(view);
    }

    #[test]
    fn placeholder_renders_without_panic() {
        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend).expect("backend");
        let theme = Theme::default();
        let placeholder = ChartPlaceholder {
            title: "off",
            message: "no data yet",
        };
        terminal
            .draw(|frame| {
                render_chart_placeholder(frame, frame.area(), &placeholder, &theme);
            })
            .expect("draw");
    }

    #[test]
    fn chart_in_tiny_area_does_not_panic() {
        let backend = TestBackend::new(4, 2);
        let mut terminal = Terminal::new(backend).expect("backend");
        let theme = Theme::default();
        let labels = vec!["a".to_owned()];
        let values = vec![1.0];
        let view = ChartView {
            kind: ChartViewKind::Bar,
            title: "tiny",
            labels: &labels,
            values: &values,
        };
        terminal
            .draw(|frame| {
                render_chart(frame, frame.area(), &view, &theme);
            })
            .expect("draw");
    }
}
