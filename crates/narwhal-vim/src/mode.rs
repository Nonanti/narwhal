use serde::{Deserialize, Serialize};

use crate::action::Operator;

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
    /// Waiting for a motion after an operator (d, y, c).
    OperatorPending(Operator),
}

impl Mode {
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Normal => "NOR",
            Self::Insert => "INS",
            Self::Visual => "VIS",
            Self::VisualLine => "V-L",
            Self::Command => "CMD",
            Self::OperatorPending(Operator::Delete) => "O-D",
            Self::OperatorPending(Operator::Yank) => "O-Y",
            Self::OperatorPending(Operator::Change) => "O-C",
        }
    }
}
