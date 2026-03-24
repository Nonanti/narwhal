use narwhal_sql::treesitter::HighlightKind;
use ratatui::style::{Color, Modifier, Style};

/// Colour palette used when rendering the interface.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub background: Color,
    pub foreground: Color,
    pub accent: Color,
    pub muted: Color,
    pub error: Color,
    /// Used for CMD mode highlight and transaction badge.
    pub warning: Color,
    /// Used by the SQL syntax highlighter for string literals — added
    /// in T1-T3-A. Most themes paint strings green; the existing
    /// palette had no "success"-y slot.
    pub success: Color,
}

impl Theme {
    /// Default palette — cool accent on the terminal's native
    /// background. Designed to look right on a dark terminal.
    pub const DARK: Self = Self {
        background: Color::Reset,
        foreground: Color::Gray,
        accent: Color::Cyan,
        muted: Color::DarkGray,
        error: Color::LightRed,
        warning: Color::Yellow,
        success: Color::LightGreen,
    };

    /// Light-terminal palette. `Color::Reset` keeps the terminal's
    /// background, but the foreground / accent shift down a step so
    /// text and selection still contrast on a white background.
    pub const LIGHT: Self = Self {
        background: Color::Reset,
        foreground: Color::Black,
        accent: Color::Blue,
        muted: Color::Gray,
        error: Color::Red,
        warning: Color::Rgb(0xb0, 0x60, 0x00), // amber that survives on white
        success: Color::Rgb(0x00, 0x80, 0x00), // green that survives on white
    };

    /// High-contrast palette — saturated primaries on a black
    /// background. Aimed at low-vision users and projector displays
    /// where the default dark theme washes out.
    /// Sprint 7 (LOW): `muted` was the same `White` as `foreground`,
    /// which made the status bar (`bg=muted, fg=foreground`) render as
    /// a single flat block of white with the text effectively
    /// invisible. Switched to `LightCyan` for a saturated, high-
    /// contrast accent that still meets the WCAG 4.5:1 ratio on black.
    pub const HIGH_CONTRAST: Self = Self {
        background: Color::Black,
        foreground: Color::White,
        accent: Color::Yellow,
        muted: Color::LightCyan,
        error: Color::LightRed,
        warning: Color::LightYellow,
        success: Color::LightGreen,
    };

    pub fn status_bar(&self) -> Style {
        Style::default().bg(self.muted).fg(self.foreground)
    }

    pub fn mode_indicator(&self) -> Style {
        Style::default()
            .bg(self.accent)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    }

    /// Mode indicator style for normal mode — muted background.
    pub fn mode_normal(&self) -> Style {
        Style::default()
            .bg(self.muted)
            .fg(self.foreground)
            .add_modifier(Modifier::BOLD)
    }

    /// Mode indicator style for insert mode — accent background.
    pub fn mode_insert(&self) -> Style {
        Style::default()
            .bg(self.accent)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    }

    /// Mode indicator style for command mode — warning background.
    pub fn mode_command(&self) -> Style {
        Style::default()
            .bg(self.warning)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    }

    /// Transaction badge style — warning-coloured text.
    pub fn transaction_badge(&self) -> Style {
        Style::default()
            .fg(self.warning)
            .add_modifier(Modifier::BOLD)
    }

    pub fn sidebar_title(&self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    /// Map a tree-sitter SQL [`HighlightKind`] to a ratatui [`Style`]
    /// using the theme palette.
    ///
    /// Added in T1-T3-A. The mapping is intentionally conservative —
    /// every kind gets a foreground colour pulled from the existing
    /// palette so a custom theme that overrides the named slots also
    /// re-skins the syntax highlighter for free.
    #[must_use]
    pub fn sql_style(&self, kind: HighlightKind) -> Style {
        match kind {
            HighlightKind::Keyword => Style::default()
                .fg(self.accent)
                .add_modifier(Modifier::BOLD),
            HighlightKind::String => Style::default().fg(self.success),
            HighlightKind::Number | HighlightKind::Constant | HighlightKind::Type => {
                Style::default().fg(self.warning)
            }
            HighlightKind::LineComment | HighlightKind::BlockComment => Style::default()
                .fg(self.muted)
                .add_modifier(Modifier::ITALIC),
            HighlightKind::Operator | HighlightKind::Punctuation => {
                Style::default().fg(self.foreground)
            }
            HighlightKind::FunctionCall => Style::default().fg(self.accent),
            HighlightKind::TableRef => Style::default()
                .fg(self.foreground)
                .add_modifier(Modifier::BOLD),
            HighlightKind::ColumnRef => Style::default().fg(self.foreground),
            HighlightKind::Alias => Style::default()
                .fg(self.foreground)
                .add_modifier(Modifier::ITALIC),
            HighlightKind::Identifier => Style::default().fg(self.foreground),
            HighlightKind::Error => Style::default()
                .fg(self.error)
                .add_modifier(Modifier::UNDERLINED),
            // `HighlightKind` is `#[non_exhaustive]`; future variants
            // fall back to plain foreground so the editor keeps
            // working until the palette gains a new slot.
            _ => Style::default().fg(self.foreground),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::DARK
    }
}
