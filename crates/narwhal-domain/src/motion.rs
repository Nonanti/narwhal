//! Cursor motions for the editor buffer.
//!
//! This is a domain-level definition of cursor motions so that
//! `narwhal-domain` does not depend on `narwhal-vim`. The app layer
//! converts from `narwhal_vim::Motion` at the boundary when available.

/// Directional or positional cursor motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
