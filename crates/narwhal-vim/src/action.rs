use serde::{Deserialize, Serialize};

use crate::mode::Mode;

/// High-level effect the state machine wants the editor buffer to perform.
///
/// The buffer applies these — the FSM never mutates text directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    /// Move the cursor according to a motion, optionally repeated.
    Move { motion: Motion, count: usize },
    /// Apply an operator over a motion (`d3w`, `yy`, …).
    Operate {
        op: Operator,
        motion: Motion,
        count: usize,
    },
    /// Insert literal text at the cursor (typing in insert mode).
    InsertText(String),
    /// Delete one character (backspace in insert / `x` in normal).
    DeleteChar,
    /// Mode transition. The FSM emits this so the UI can update the status line.
    EnterMode(Mode),
    /// Submit the current `:` command-line buffer.
    SubmitCommand(String),
    /// No-op: key was consumed by the FSM but produced no editor effect
    /// (e.g. partially typed operator).
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
