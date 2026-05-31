//! Glyph set used when rendering column markers (PK / FK / UNIQUE).
//!
//! Mermaid and DOT outputs always use ASCII because their downstream
//! renderers (mermaid.live, Graphviz HTML labels) do not reliably ship
//! Nerd Font glyphs. The TUI widget reads [`IconSet`] from config and
//! formats columns itself.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IconSet {
    /// Plain `[PK]`, `[FK]`, `[UK]`. Safe everywhere.
    #[default]
    Ascii,
    /// Nerd Font glyphs for terminals with a patched font.
    Nerdfont,
}

impl IconSet {
    /// Marker shown next to a primary-key column.
    pub const fn pk(self) -> &'static str {
        match self {
            // f084 (nf-fa-key)
            Self::Nerdfont => "\u{f084}",
            Self::Ascii => "[PK]",
        }
    }

    /// Marker shown next to a foreign-key column.
    pub const fn fk(self) -> &'static str {
        match self {
            // f0c1 (nf-fa-link)
            Self::Nerdfont => "\u{f0c1}",
            Self::Ascii => "[FK]",
        }
    }

    /// Marker shown next to a non-PK unique column.
    pub const fn uk(self) -> &'static str {
        match self {
            // f084 with star? keep distinct: f005 (nf-fa-star)
            Self::Nerdfont => "\u{f005}",
            Self::Ascii => "[UK]",
        }
    }

    /// Marker used in impact trees for `NO ACTION` references (dangerous
    /// on delete).
    pub const fn warning(self) -> &'static str {
        match self {
            // f071 (nf-fa-warning)
            Self::Nerdfont => "\u{f071}",
            Self::Ascii => "(!)",
        }
    }
}
