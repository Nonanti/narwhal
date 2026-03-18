//! Schema-diagram modal (Focused + Impact views).
//!
//! Renders a centred overlay over the result pane. Both modes share the
//! outer block; the body is rebuilt every frame from the live
//! [`narwhal_diagram`] model so re-centering (Enter on a neighbour)
//! shows immediately without any extra state plumbing.
//!
//! The widget owns no state — scroll, selection and the active mode
//! live in the host's `DiagramModalState`.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use narwhal_diagram::{
    Cardinality, DiagramModel, IconSet, ImpactNode, ImpactTree, Node, NodeColumn, QualifiedName,
};

use crate::theme::Theme;
use crate::widgets::centred_rect;

/// Which body the modal renders. Mirrors the host-side enum so the
/// widget does not need to know the host's exact type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagramViewMode {
    Focused,
    Impact,
}

/// Borrowed snapshot of the modal at render time.
#[derive(Debug, Clone, Copy)]
pub struct DiagramView<'a> {
    pub mode: DiagramViewMode,
    /// Full diagram cached on the host. The widget filters down to
    /// `center` + 1-hop neighbours for Focused mode and walks `impact`
    /// for Impact mode.
    pub model: &'a DiagramModel,
    pub center: &'a QualifiedName,
    pub impact: &'a ImpactTree,
    /// Selection cursor for Focused-mode neighbour list (outbound
    /// concatenated with inbound). Clamped at render time so a stale
    /// host-side value cannot wander past the end of the list.
    pub selected: usize,
    pub scroll: u16,
    pub icons: IconSet,
}

/// Render the modal on top of `area`. Always centred at 80% × 80% so
/// the underlying panes remain visible as context.
pub fn render_diagram(frame: &mut Frame<'_>, area: Rect, view: &DiagramView<'_>, theme: &Theme) {
    let modal = centred_rect(area, area.width * 8 / 10, area.height * 8 / 10);
    frame.render_widget(Clear, modal);

    let title = match view.mode {
        DiagramViewMode::Focused => format!(
            " Focused: {} \u{2014} 1-hop: {} table(s) ",
            view.center.display(),
            neighbour_count(view.model, view.center)
        ),
        DiagramViewMode::Impact => format!(
            " Impact: {} \u{2014} {} inbound ",
            view.center.display(),
            count_impact_nodes(&view.impact.inbound),
        ),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    if inner.height < 3 {
        // Terminal too small — show a friendly hint instead of garbled output.
        let hint = Paragraph::new("(terminal too small for diagram modal)")
            .style(Style::default().fg(theme.muted))
            .wrap(Wrap { trim: false });
        frame.render_widget(hint, inner);
        return;
    }

    let footer_height: u16 = 1;
    let body_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: inner.height.saturating_sub(footer_height),
    };
    let footer_area = Rect {
        x: inner.x,
        y: inner.y + body_area.height,
        width: inner.width,
        height: footer_height,
    };

    let lines = match view.mode {
        DiagramViewMode::Focused => focused_lines(view, theme),
        DiagramViewMode::Impact => impact_lines(view, theme),
    };

    // Clamp scroll against the actual content length.
    let total = lines.len() as u16;
    let scroll = view.scroll.min(total.saturating_sub(1));
    let body = Paragraph::new(lines).scroll((scroll, 0));
    frame.render_widget(body, body_area);

    let hint = match view.mode {
        DiagramViewMode::Focused => Line::from(vec![
            Span::styled(" Tab ", key_style(theme)),
            Span::raw("cycle  "),
            Span::styled(" Enter ", key_style(theme)),
            Span::raw("re-center  "),
            Span::styled(" i ", key_style(theme)),
            Span::raw("impact  "),
            Span::styled(" e ", key_style(theme)),
            Span::raw("export  "),
            Span::styled(" y ", key_style(theme)),
            Span::raw("yank  "),
            Span::styled(" q ", key_style(theme)),
            Span::raw("close"),
        ]),
        DiagramViewMode::Impact => Line::from(vec![
            Span::styled(" i ", key_style(theme)),
            Span::raw("focused  "),
            Span::styled(" y ", key_style(theme)),
            Span::raw("yank tree  "),
            Span::styled(" e ", key_style(theme)),
            Span::raw("export  "),
            Span::styled(" q ", key_style(theme)),
            Span::raw("close"),
        ]),
    };
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(theme.muted)),
        footer_area,
    );
}

// ───── Focused body ────────────────────────────────────────────────

fn focused_lines<'a>(view: &DiagramView<'_>, theme: &'a Theme) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'a>> = Vec::with_capacity(48);

    // 1. Centre table box (always shown, with all its columns).
    if let Some(node) = view.model.node(view.center) {
        push_table_box(&mut lines, node, theme, view.icons);
    } else {
        lines.push(Line::from(Span::styled(
            format!("(table not found in cached model: {})", view.center.display()),
            Style::default().fg(theme.error),
        )));
        return lines;
    }
    lines.push(Line::raw(""));

    // 2. Outbound: edges where centre is the child (it references
    //    `to`). Display name = the referenced parent.
    let outbound: Vec<&narwhal_diagram::Edge> = view
        .model
        .edges
        .iter()
        .filter(|e| &e.from == view.center)
        .collect();
    let inbound: Vec<&narwhal_diagram::Edge> = view
        .model
        .edges
        .iter()
        .filter(|e| &e.to == view.center)
        .collect();
    let total_navigable = outbound.len() + inbound.len();
    // Clamp selection at render time so stale host state is harmless.
    let selected = if total_navigable == 0 {
        0
    } else {
        view.selected.min(total_navigable - 1)
    };

    lines.push(Line::from(Span::styled(
        format!("  References (outbound, {}):", outbound.len()),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )));
    if outbound.is_empty() {
        lines.push(Line::from(Span::styled(
            "    (none)".to_string(),
            Style::default().fg(theme.muted),
        )));
    } else {
        for (i, edge) in outbound.iter().enumerate() {
            let active = i == selected && total_navigable > 0;
            lines.push(neighbour_line(
                &edge.to,
                &edge.label(),
                edge.cardinality,
                Direction::Outbound,
                active,
                theme,
            ));
        }
    }
    lines.push(Line::raw(""));

    lines.push(Line::from(Span::styled(
        format!("  Referenced by (inbound, {}):", inbound.len()),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )));
    if inbound.is_empty() {
        lines.push(Line::from(Span::styled(
            "    (none)".to_string(),
            Style::default().fg(theme.muted),
        )));
    } else {
        for (i, edge) in inbound.iter().enumerate() {
            let active = (outbound.len() + i) == selected && total_navigable > 0;
            lines.push(neighbour_line(
                &edge.from,
                &edge.label(),
                edge.cardinality,
                Direction::Inbound,
                active,
                theme,
            ));
        }
    }

    lines
}

#[derive(Clone, Copy)]
enum Direction {
    Outbound,
    Inbound,
}

fn neighbour_line<'a>(
    other: &QualifiedName,
    via: &str,
    card: Cardinality,
    dir: Direction,
    active: bool,
    theme: &'a Theme,
) -> Line<'a> {
    let arrow = match dir {
        Direction::Outbound => "\u{2500}\u{2500}\u{25b6}", // ──▶
        Direction::Inbound => "\u{25c0}\u{2500}\u{2500}",  // ◀──
    };
    let nullable_hint = matches!(
        card,
        Cardinality::ZeroOrOneToMany | Cardinality::ZeroOrOneToOne
    )
    .then_some("  (nullable)")
    .unwrap_or("");
    let one_to_one = matches!(card, Cardinality::OneToOne | Cardinality::ZeroOrOneToOne)
        .then_some("  [1\u{2011}to\u{2011}1]")
        .unwrap_or("");

    let cursor = if active { "  > " } else { "    " };
    let row = format!(
        "{cursor}{name:<32}  via {via:<20} {arrow}{one_to_one}{nullable_hint}",
        name = other.display(),
    );
    let style = if active {
        Style::default()
            .fg(theme.background)
            .bg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.foreground)
    };
    Line::from(Span::styled(row, style))
}

fn push_table_box<'a>(lines: &mut Vec<Line<'a>>, node: &Node, theme: &'a Theme, icons: IconSet) {
    let header = format!("\u{250c}\u{2500} {} ", node.qualified.display());
    let header_len = header.chars().count();
    let pad_chars = 56usize.saturating_sub(header_len);
    let mut header_line = header;
    header_line.push_str(&"\u{2500}".repeat(pad_chars));
    header_line.push('\u{2510}');
    lines.push(Line::from(Span::styled(
        header_line,
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )));

    for col in &node.columns {
        lines.push(column_line(col, icons, theme));
    }
    lines.push(Line::from(Span::styled(
        format!("\u{2514}{}\u{2518}", "\u{2500}".repeat(56)),
        Style::default().fg(theme.accent),
    )));
}

fn column_line<'a>(col: &NodeColumn, icons: IconSet, theme: &'a Theme) -> Line<'a> {
    let marker = if col.primary_key {
        icons.pk()
    } else if col.foreign_key {
        icons.fk()
    } else if col.unique {
        icons.uk()
    } else {
        "    "
    };
    let nullable = if col.nullable { "?" } else { " " };
    let body = format!(
        "\u{2502} {marker:<6} {name:<22} {ty:<20} {nullable} \u{2502}",
        name = col.name,
        ty = col.data_type,
    );
    let style = if col.primary_key {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else if col.foreign_key {
        Style::default().fg(theme.accent)
    } else {
        Style::default().fg(theme.foreground)
    };
    Line::from(Span::styled(body, style))
}

fn neighbour_count(model: &DiagramModel, centre: &QualifiedName) -> usize {
    model
        .edges
        .iter()
        .filter(|e| &e.from == centre || &e.to == centre)
        .map(|e| if &e.from == centre { &e.to } else { &e.from })
        .collect::<std::collections::HashSet<_>>()
        .len()
}

// ───── Impact body ─────────────────────────────────────────────────

fn impact_lines<'a>(view: &DiagramView<'_>, theme: &'a Theme) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'a>> = Vec::with_capacity(16);
    lines.push(Line::from(Span::styled(
        format!("  {}", view.center.display()),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )));
    push_impact_children(&mut lines, &view.impact.inbound, "  ", view.icons, theme);
    lines
}

fn push_impact_children<'a>(
    lines: &mut Vec<Line<'a>>,
    children: &[ImpactNode],
    prefix: &str,
    icons: IconSet,
    theme: &'a Theme,
) {
    let last = children.len().saturating_sub(1);
    for (i, node) in children.iter().enumerate() {
        let is_last = i == last;
        let connector = if is_last {
            "\u{2514}\u{2500}\u{25c0}"
        } else {
            "\u{251c}\u{2500}\u{25c0}"
        };
        let action_label = action_text(node);
        let warning = if matches!(
            node.on_delete,
            Some(narwhal_core::schema::ReferentialAction::NoAction) | None
        ) {
            format!("  {}", icons.warning())
        } else {
            String::new()
        };
        let row = format!(
            "{prefix}{connector} {table}.{cols}{action}{warning}",
            table = node.table.display(),
            cols = node.fk_columns.join(","),
            action = action_label,
        );
        lines.push(Line::from(Span::styled(
            row,
            Style::default().fg(theme.foreground),
        )));
        let child_prefix = if is_last {
            format!("{prefix}    ")
        } else {
            format!("{prefix}\u{2502}   ")
        };
        push_impact_children(lines, &node.children, &child_prefix, icons, theme);
    }
}

fn action_text(node: &ImpactNode) -> String {
    match node.on_delete {
        Some(a) => format!("    [ON DELETE {}]", a.as_sql()),
        None => "    [ON DELETE NO ACTION]".into(),
    }
}

fn count_impact_nodes(nodes: &[ImpactNode]) -> usize {
    let mut n = nodes.len();
    for node in nodes {
        n += count_impact_nodes(&node.children);
    }
    n
}

fn key_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.background)
        .bg(theme.accent)
        .add_modifier(Modifier::BOLD)
}
