use serde::{Deserialize, Serialize};

/// Modal editor states.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    /// Character-wise visual selection.
    Visual,
    /// Line-wise visual selection.
    VisualLine,
    /// Command-line entered via `:`.
    Command,
}

impl Mode {
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Normal => "NOR",
            Self::Insert => "INS",
            Self::Visual => "VIS",
            Self::VisualLine => "V-L",
            Self::Command => "CMD",
        }
    }
}
