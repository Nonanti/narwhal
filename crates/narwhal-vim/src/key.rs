use serde::{Deserialize, Serialize};

/// Logical key event consumed by the vim state machine.
///
/// The TUI layer is responsible for translating raw `crossterm` events into
/// this type. Keeping it ratatui-free lets us test the FSM in isolation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    pub code: KeyCode,
    pub mods: KeyMod,
}

impl Key {
    pub fn char(c: char) -> Self {
        Self {
            code: KeyCode::Char(c),
            mods: KeyMod::NONE,
        }
    }

    pub fn special(code: KeyCode) -> Self {
        Self {
            code,
            mods: KeyMod::NONE,
        }
    }

    pub fn ctrl(c: char) -> Self {
        Self {
            code: KeyCode::Char(c),
            mods: KeyMod::CTRL,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyCode {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Tab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyMod(u8);

impl KeyMod {
    pub const NONE: Self = Self(0);
    pub const CTRL: Self = Self(1 << 0);
    pub const ALT: Self = Self(1 << 1);
    pub const SHIFT: Self = Self(1 << 2);

    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}
