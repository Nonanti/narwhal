use crate::action::{Action, Motion};
use crate::key::{Key, KeyCode};
use crate::mode::Mode;

/// Modal keystroke processor.
#[derive(Debug, Default)]
pub struct Vim {
    mode: Mode,
    pending_count: Option<usize>,
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

    /// Replace the trailing non-whitespace token in the command buffer
    /// with `replacement`. Called by the host after a prompt Tab-completion
    /// produces a single match or a longest-common-prefix insertion.
    pub fn replace_command_token(&mut self, replacement: &str) {
        while self.command_buffer.ends_with(|c: char| !c.is_whitespace()) {
            self.command_buffer.pop();
        }
        self.command_buffer.push_str(replacement);
    }

    /// Feed one key event and obtain the resulting action.
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
                let digit = c.to_digit(10).unwrap_or(0) as usize;
                self.pending_count = Some(self.pending_count.unwrap_or(0) * 10 + digit);
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
            KeyCode::Char('i' | 'a') => {
                self.mode = Mode::Insert;
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
            KeyCode::Tab => Action::PromptComplete,
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
        let mut vim = Vim::new();
        assert_eq!(vim.handle(Key::char('i')), Action::EnterMode(Mode::Insert));
        assert_eq!(vim.mode(), Mode::Insert);
    }

    #[test]
    fn counted_motion() {
        let mut vim = Vim::new();
        assert_eq!(vim.handle(Key::char('5')), Action::Pending);
        assert_eq!(
            vim.handle(Key::char('j')),
            Action::Move {
                motion: Motion::Down,
                count: 5,
            }
        );
    }

    #[test]
    fn esc_returns_to_normal() {
        let mut vim = Vim::new();
        vim.handle(Key::char('i'));
        vim.handle(Key::special(KeyCode::Esc));
        assert_eq!(vim.mode(), Mode::Normal);
    }

    #[test]
    fn command_mode_submits_on_enter() {
        let mut vim = Vim::new();
        vim.handle(Key::char(':'));
        vim.handle(Key::char('q'));
        assert_eq!(
            vim.handle(Key::special(KeyCode::Enter)),
            Action::SubmitCommand("q".into())
        );
        assert_eq!(vim.mode(), Mode::Normal);
    }

    #[test]
    fn command_mode_clears_on_escape() {
        let mut vim = Vim::new();
        vim.handle(Key::char(':'));
        vim.handle(Key::char('w'));
        vim.handle(Key::special(KeyCode::Esc));
        assert_eq!(vim.mode(), Mode::Normal);
        assert!(vim.command_buffer().is_empty());
    }

    #[test]
    fn count_resets_after_motion() {
        let mut vim = Vim::new();
        vim.handle(Key::char('3'));
        vim.handle(Key::char('j'));
        assert_eq!(
            vim.handle(Key::char('j')),
            Action::Move {
                motion: Motion::Down,
                count: 1,
            }
        );
    }
}
