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
}

impl Theme {
    /// Default palette — cool accent on the terminal's native
    /// background. Designed to look right on a dark terminal.
    pub const DARK: Theme = Theme {
        background: Color::Reset,
        foreground: Color::Gray,
        accent: Color::Cyan,
        muted: Color::DarkGray,
        error: Color::LightRed,
        warning: Color::Yellow,
    };

    /// Light-terminal palette. `Color::Reset` keeps the terminal's
    /// background, but the foreground / accent shift down a step so
    /// text and selection still contrast on a white background.
    pub const LIGHT: Theme = Theme {
        background: Color::Reset,
        foreground: Color::Black,
        accent: Color::Blue,
        muted: Color::Gray,
        error: Color::Red,
        warning: Color::Rgb(0xb0, 0x60, 0x00), // amber that survives on white
    };

    /// High-contrast palette — saturated primaries on a black
    /// background. Aimed at low-vision users and projector displays
    /// where the default dark theme washes out.
    pub const HIGH_CONTRAST: Theme = Theme {
        background: Color::Black,
        foreground: Color::White,
        accent: Color::Yellow,
        muted: Color::White,
        error: Color::LightRed,
        warning: Color::LightYellow,
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
}

impl Default for Theme {
    fn default() -> Self {
        Theme::DARK
    }
}
