use ratatui::style::{Color, Modifier, Style};

/// Minimal palette. We'll grow this into proper theme loading later.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub background: Color,
    pub foreground: Color,
    pub accent: Color,
    pub muted: Color,
    pub error: Color,
}

impl Theme {
    pub const DARK: Theme = Theme {
        background: Color::Reset,
        foreground: Color::Gray,
        accent: Color::Cyan,
        muted: Color::DarkGray,
        error: Color::LightRed,
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

    pub fn sidebar_title(&self) -> Style {
        Style::default().fg(self.accent).add_modifier(Modifier::BOLD)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::DARK
    }
}
