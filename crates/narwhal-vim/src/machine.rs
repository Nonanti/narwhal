use crate::action::{Action, Motion};
use crate::key::{Key, KeyCode};
use crate::mode::Mode;

/// Minimal vim state machine.
///
/// This is the "spine" — only handles the most common keys so the UI has
/// something to talk to. We grow it iteratively with tests.
#[derive(Debug, Default)]
pub struct Vim {
    mode: Mode,
    /// Pending count prefix being typed in Normal mode (`5j` → 5).
    pending_count: Option<usize>,
    /// `:`-line buffer while in Command mode.
    command_buffer: String,
}

impl Vim {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn command_buffer(&self) -> &str {
        &self.command_buffer
    }

    /// Feed one key event. Always returns an [`Action`] (possibly `Pending`).
    pub fn handle(&mut self, key: Key) -> Action {
        match self.mode {
            Mode::Normal => self.handle_normal(key),
            Mode::Insert => self.handle_insert(key),
            Mode::Command => self.handle_command(key),
            Mode::Visual | Mode::VisualLine => self.handle_visual(key),
        }
    }

    fn take_count(&mut self) -> usize {
        self.pending_count.take().unwrap_or(1)
    }

    fn handle_normal(&mut self, key: Key) -> Action {
        match key.code {
            KeyCode::Char(c @ '0'..='9') if !(c == '0' && self.pending_count.is_none()) => {
                let d = c.to_digit(10).unwrap() as usize;
                self.pending_count = Some(self.pending_count.unwrap_or(0) * 10 + d);
                Action::Pending
            }
            KeyCode::Char('h') | KeyCode::Left => Action::Move {
                motion: Motion::Left,
                count: self.take_count(),
            },
            KeyCode::Char('l') | KeyCode::Right => Action::Move {
                motion: Motion::Right,
                count: self.take_count(),
            },
            KeyCode::Char('j') | KeyCode::Down => Action::Move {
                motion: Motion::Down,
                count: self.take_count(),
            },
            KeyCode::Char('k') | KeyCode::Up => Action::Move {
                motion: Motion::Up,
                count: self.take_count(),
            },
            KeyCode::Char('w') => Action::Move {
                motion: Motion::WordForward,
                count: self.take_count(),
            },
            KeyCode::Char('b') => Action::Move {
                motion: Motion::WordBackward,
                count: self.take_count(),
            },
            KeyCode::Char('0') => Action::Move {
                motion: Motion::LineStart,
                count: 1,
            },
            KeyCode::Char('$') => Action::Move {
                motion: Motion::LineEnd,
                count: 1,
            },
            KeyCode::Char('G') => Action::Move {
                motion: Motion::FileEnd,
                count: 1,
            },
            KeyCode::Char('i') => {
                self.mode = Mode::Insert;
                Action::EnterMode(Mode::Insert)
            }
            KeyCode::Char('a') => {
                self.mode = Mode::Insert;
                // TODO: cursor should advance one column first.
                Action::EnterMode(Mode::Insert)
            }
            KeyCode::Char('v') => {
                self.mode = Mode::Visual;
                Action::EnterMode(Mode::Visual)
            }
            KeyCode::Char('V') => {
                self.mode = Mode::VisualLine;
                Action::EnterMode(Mode::VisualLine)
            }
            KeyCode::Char(':') => {
                self.mode = Mode::Command;
                self.command_buffer.clear();
                Action::EnterMode(Mode::Command)
            }
            _ => Action::Pending,
        }
    }

    fn handle_insert(&mut self, key: Key) -> Action {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                Action::EnterMode(Mode::Normal)
            }
            KeyCode::Backspace => Action::DeleteChar,
            KeyCode::Enter => Action::InsertText("\n".into()),
            KeyCode::Char(c) => Action::InsertText(c.to_string()),
            _ => Action::Pending,
        }
    }

    fn handle_command(&mut self, key: Key) -> Action {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.command_buffer.clear();
                Action::EnterMode(Mode::Normal)
            }
            KeyCode::Enter => {
                let cmd = std::mem::take(&mut self.command_buffer);
                self.mode = Mode::Normal;
                Action::SubmitCommand(cmd)
            }
            KeyCode::Backspace => {
                self.command_buffer.pop();
                Action::Pending
            }
            KeyCode::Char(c) => {
                self.command_buffer.push(c);
                Action::Pending
            }
            _ => Action::Pending,
        }
    }

    fn handle_visual(&mut self, key: Key) -> Action {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                Action::EnterMode(Mode::Normal)
            }
            // Reuse normal-mode motions inside visual.
            _ => self.handle_normal_motion_only(key),
        }
    }

    fn handle_normal_motion_only(&mut self, key: Key) -> Action {
        match key.code {
            KeyCode::Char('h') => Action::Move {
                motion: Motion::Left,
                count: 1,
            },
            KeyCode::Char('l') => Action::Move {
                motion: Motion::Right,
                count: 1,
            },
            KeyCode::Char('j') => Action::Move {
                motion: Motion::Down,
                count: 1,
            },
            KeyCode::Char('k') => Action::Move {
                motion: Motion::Up,
                count: 1,
            },
            _ => Action::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enters_insert_on_i() {
        let mut v = Vim::new();
        let a = v.handle(Key::char('i'));
        assert_eq!(a, Action::EnterMode(Mode::Insert));
        assert_eq!(v.mode(), Mode::Insert);
    }

    #[test]
    fn counted_motion() {
        let mut v = Vim::new();
        assert_eq!(v.handle(Key::char('5')), Action::Pending);
        assert_eq!(
            v.handle(Key::char('j')),
            Action::Move {
                motion: Motion::Down,
                count: 5,
            }
        );
    }

    #[test]
    fn esc_returns_to_normal() {
        let mut v = Vim::new();
        v.handle(Key::char('i'));
        assert_eq!(v.mode(), Mode::Insert);
        v.handle(Key::special(KeyCode::Esc));
        assert_eq!(v.mode(), Mode::Normal);
    }

    #[test]
    fn command_mode_submits() {
        let mut v = Vim::new();
        v.handle(Key::char(':'));
        v.handle(Key::char('q'));
        let a = v.handle(Key::special(KeyCode::Enter));
        assert_eq!(a, Action::SubmitCommand("q".into()));
        assert_eq!(v.mode(), Mode::Normal);
    }
}
