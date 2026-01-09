use serde::{Deserialize, Serialize};

use crate::mode::Mode;

/// Direction for the search prompt opened by `/` or `?`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[non_exhaustive]
pub enum SearchDirection {
    #[default]
    Forward,
    Backward,
}

/// Effect produced by the state machine.
///
/// The buffer applies actions; the state machine never mutates text directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
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
    /// Open the search prompt (`/` for forward, `?` for backward).
    OpenSearch(SearchDirection),
    /// Repeat the last search in the original direction (`n`).
    RepeatSearch,
    /// Repeat the last search in the reverse direction (`N`).
    RepeatSearchReverse,
    /// The key was consumed but produced no observable effect (for example, a
    /// partially typed count or operator).
    Pending,
    /// Tab was pressed in command mode; the host should attempt prompt
    /// completion against the relevant universe.
    PromptComplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
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
    /// The current line (used by dd, yy, cc).
    CurrentLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Operator {
    Delete,
    Yank,
    Change,
}
