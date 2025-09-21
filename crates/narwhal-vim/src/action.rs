use serde::{Deserialize, Serialize};

use crate::mode::Mode;

/// Effect produced by the state machine.
///
/// The buffer applies actions; the state machine never mutates text directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    /// Move the cursor along a motion, optionally repeated.
    Move { motion: Motion, count: usize },
    /// Apply an operator over a motion (`d3w`, `yy`, …).
    Operate {
        op: Operator,
        motion: Motion,
        count: usize,
    },
    /// Insert literal text at the cursor.
    InsertText(String),
    /// Delete one character.
    DeleteChar,
    /// Mode transition.
    EnterMode(Mode),
    /// Submit the current command-line buffer.
    SubmitCommand(String),
    /// The key was consumed but produced no observable effect (for example, a
    /// partially typed count or operator).
    Pending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordForward,
    WordBackward,
    LineStart,
    LineEnd,
    FileStart,
    FileEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operator {
    Delete,
    Yank,
    Change,
}
