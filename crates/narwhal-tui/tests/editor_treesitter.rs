//! Smoke test for the editor → tree-sitter render path.
//!
//! Renders the editor into an off-screen `ratatui` `TestBackend` and
//! asserts that SQL keywords land on a styled span rather than the
//! plain-text fallback. We don't pin specific colours (themes
//! override them), only the *style change* against unhighlighted
//! rendering.

use narwhal_sql::treesitter::Parser;
use narwhal_tui::EditorBuffer;
use narwhal_tui::theme::Theme;
use narwhal_tui::widgets::editor::render_editor;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

#[test]
fn editor_renders_with_sql_highlight_overlay() {
    let mut buf = EditorBuffer::new();
    buf.insert_str("SELECT id FROM users;");
    let theme = Theme::DARK;

    let mut parser = Parser::new().expect("grammar");
    let source = buf.entire_text();
    parser.parse(&source).expect("parse");
    let spans = parser.tree().expect("tree").highlights(&source);
    assert!(!spans.is_empty(), "expected highlight spans, got none");

    // Render twice: once without highlights, once with. The styled
    // rendering must differ from the plain rendering at the keyword
    // cells.
    let area = Rect::new(0, 0, 60, 5);
    let mut term_plain = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    term_plain
        .draw(|f| {
            render_editor(
                f,
                area,
                &mut buf.clone(),
                &theme,
                true,
                "scratch",
                None,
                None,
            );
        })
        .unwrap();
    let plain = term_plain.backend().buffer().clone();

    let mut term_hl = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    term_hl
        .draw(|f| {
            render_editor(
                f,
                area,
                &mut buf.clone(),
                &theme,
                true,
                "scratch",
                None,
                Some(&spans),
            );
        })
        .unwrap();
    let hl = term_hl.backend().buffer().clone();

    // Find the row column where 'S' (start of SELECT) sits in the
    // rendered buffer. The outer block adds a one-cell border, then
    // the gutter sits at the start of the inner area; we locate the
    // glyph by scanning rather than hard-coding the gutter width.
    let row = 1; // outer border on row 0
    let select_col = (0..area.width)
        .find(|&c| {
            plain
                .cell((c, row))
                .is_some_and(|cell| cell.symbol() == "S")
        })
        .expect("plain rendering must contain a 'S' glyph for SELECT");
    let plain_cell = plain.cell((select_col, row)).expect("plain cell");
    let hl_cell = hl.cell((select_col, row)).expect("hl cell");
    assert_eq!(
        plain_cell.symbol(),
        hl_cell.symbol(),
        "highlighted rendering should keep the same glyphs"
    );
    assert_ne!(
        plain_cell.style(),
        hl_cell.style(),
        "highlighted rendering should change the style on SELECT"
    );
}
