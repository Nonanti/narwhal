use crate::action::{Action, Motion, Operator, SearchDirection};
use crate::key::{Key, KeyCode};
use crate::mode::Mode;

/// Maximum count value. Prevents overflow from sticky keys or malicious
/// scripts. Vim's real-world practical cap is well below this.
const MAX_COUNT: usize = 999_999;

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

    pub const fn mode(&self) -> Mode {
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
            Mode::OperatorPending(_) => self.handle_operator_pending(key),
        }
    }

    fn take_count(&mut self) -> usize {
        self.pending_count.take().unwrap_or(1)
    }

    /// Accumulate a digit into the pending count, clamping to `MAX_COUNT`.
    fn push_count_digit(&mut self, digit: usize) {
        let next = self
            .pending_count
            .unwrap_or(0)
            .checked_mul(10)
            .and_then(|v| v.checked_add(digit))
            .unwrap_or(MAX_COUNT)
            .min(MAX_COUNT);
        self.pending_count = Some(next);
    }

    fn handle_normal(&mut self, key: Key) -> Action {
        match key.code {
            KeyCode::Char(c @ '0'..='9') if !(c == '0' && self.pending_count.is_none()) => {
                let digit = c.to_digit(10).unwrap_or(0) as usize;
                self.push_count_digit(digit);
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
            KeyCode::Char('0') => {
                self.pending_count = None;
                Action::Move {
                    motion: Motion::LineStart,
                    count: 1,
                }
            }
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
            KeyCode::Char('/') => Action::OpenSearch(SearchDirection::Forward),
            KeyCode::Char('?') => Action::OpenSearch(SearchDirection::Backward),
            KeyCode::Char('n') => Action::RepeatSearch,
            KeyCode::Char('N') => Action::RepeatSearchReverse,
            KeyCode::Char(':') => {
                self.mode = Mode::Command;
                self.command_buffer.clear();
                Action::EnterMode(Mode::Command)
            }
            // Operators — enter OperatorPending mode
            KeyCode::Char('d') => {
                self.mode = Mode::OperatorPending(Operator::Delete);
                Action::Pending
            }
            KeyCode::Char('y') => {
                self.mode = Mode::OperatorPending(Operator::Yank);
                Action::Pending
            }
            KeyCode::Char('c') => {
                self.mode = Mode::OperatorPending(Operator::Change);
                Action::Pending
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
                // L14: cap the prompt buffer to a sane size so a stuck
                // key repeat can't grow it unboundedly. 4 KiB is well
                // beyond any legitimate `:`-command.
                const COMMAND_BUFFER_MAX: usize = 4 * 1024;
                if self.command_buffer.len() < COMMAND_BUFFER_MAX {
                    self.command_buffer.push(c);
                }
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
            // Operators in visual mode apply to the selection
            KeyCode::Char('d' | 'x') => {
                let op = Operator::Delete;
                self.mode = Mode::Normal;
                Action::Operate {
                    op,
                    motion: Motion::CurrentLine,
                    count: 1,
                }
            }
            KeyCode::Char('y') => {
                let op = Operator::Yank;
                self.mode = Mode::Normal;
                Action::Operate {
                    op,
                    motion: Motion::CurrentLine,
                    count: 1,
                }
            }
            KeyCode::Char('c') => {
                let op = Operator::Change;
                self.mode = Mode::Insert;
                Action::Operate {
                    op,
                    motion: Motion::CurrentLine,
                    count: 1,
                }
            }
            _ => Action::Pending,
        }
    }

    /// Handle keys in `OperatorPending` mode. After an operator (d/y/c) is
    /// pressed, we wait for a motion or a doubled operator (dd/yy/cc for
    /// line-wise).
    fn handle_operator_pending(&mut self, key: Key) -> Action {
        let op = match self.mode {
            Mode::OperatorPending(op) => op,
            _ => unreachable!("handle_operator_pending called outside OperatorPending"),
        };

        match key.code {
            // Digit accumulation continues into operator-pending
            KeyCode::Char(c @ '0'..='9') if !(c == '0' && self.pending_count.is_none()) => {
                let digit = c.to_digit(10).unwrap_or(0) as usize;
                self.push_count_digit(digit);
                Action::Pending
            }
            // Doubled operator: line-wise (dd, yy, cc)
            KeyCode::Char('d') if op == Operator::Delete => {
                let count = self.take_count();
                self.mode = Mode::Normal;
                Action::Operate {
                    op: Operator::Delete,
                    motion: Motion::CurrentLine,
                    count,
                }
            }
            KeyCode::Char('y') if op == Operator::Yank => {
                let count = self.take_count();
                self.mode = Mode::Normal;
                Action::Operate {
                    op: Operator::Yank,
                    motion: Motion::CurrentLine,
                    count,
                }
            }
            KeyCode::Char('c') if op == Operator::Change => {
                let count = self.take_count();
                self.mode = Mode::Insert;
                Action::Operate {
                    op: Operator::Change,
                    motion: Motion::CurrentLine,
                    count,
                }
            }
            // Motions
            KeyCode::Char('w') => {
                let count = self.take_count();
                let next_mode = if op == Operator::Change {
                    Mode::Insert
                } else {
                    Mode::Normal
                };
                self.mode = next_mode;
                Action::Operate {
                    op,
                    motion: Motion::WordForward,
                    count,
                }
            }
            KeyCode::Char('b') => {
                let count = self.take_count();
                let next_mode = if op == Operator::Change {
                    Mode::Insert
                } else {
                    Mode::Normal
                };
                self.mode = next_mode;
                Action::Operate {
                    op,
                    motion: Motion::WordBackward,
                    count,
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                let count = self.take_count();
                let next_mode = if op == Operator::Change {
                    Mode::Insert
                } else {
                    Mode::Normal
                };
                self.mode = next_mode;
                Action::Operate {
                    op,
                    motion: Motion::Left,
                    count,
                }
            }
            KeyCode::Char('l') | KeyCode::Right => {
                let count = self.take_count();
                let next_mode = if op == Operator::Change {
                    Mode::Insert
                } else {
                    Mode::Normal
                };
                self.mode = next_mode;
                Action::Operate {
                    op,
                    motion: Motion::Right,
                    count,
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let count = self.take_count();
                let next_mode = if op == Operator::Change {
                    Mode::Insert
                } else {
                    Mode::Normal
                };
                self.mode = next_mode;
                Action::Operate {
                    op,
                    motion: Motion::Down,
                    count,
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let count = self.take_count();
                let next_mode = if op == Operator::Change {
                    Mode::Insert
                } else {
                    Mode::Normal
                };
                self.mode = next_mode;
                Action::Operate {
                    op,
                    motion: Motion::Up,
                    count,
                }
            }
            KeyCode::Char('0') => {
                let next_mode = if op == Operator::Change {
                    Mode::Insert
                } else {
                    Mode::Normal
                };
                self.mode = next_mode;
                Action::Operate {
                    op,
                    motion: Motion::LineStart,
                    count: 1,
                }
            }
            KeyCode::Char('$') => {
                let next_mode = if op == Operator::Change {
                    Mode::Insert
                } else {
                    Mode::Normal
                };
                self.mode = next_mode;
                Action::Operate {
                    op,
                    motion: Motion::LineEnd,
                    count: 1,
                }
            }
            KeyCode::Char('G') => {
                let next_mode = if op == Operator::Change {
                    Mode::Insert
                } else {
                    Mode::Normal
                };
                self.mode = next_mode;
                Action::Operate {
                    op,
                    motion: Motion::FileEnd,
                    count: 1,
                }
            }
            // Escape cancels the operator
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.pending_count = None;
                Action::EnterMode(Mode::Normal)
            }
            _ => {
                // Unknown key cancels the operator and returns to Normal
                self.mode = Mode::Normal;
                self.pending_count = None;
                Action::Pending
            }
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

    // ---- M16: Operator state machine tests ----

    #[test]
    fn dd_deletes_line() {
        let mut vim = Vim::new();
        assert_eq!(vim.handle(Key::char('d')), Action::Pending);
        assert_eq!(
            vim.handle(Key::char('d')),
            Action::Operate {
                op: Operator::Delete,
                motion: Motion::CurrentLine,
                count: 1,
            }
        );
        assert_eq!(vim.mode(), Mode::Normal);
    }

    #[test]
    fn yy_yanks_line() {
        let mut vim = Vim::new();
        assert_eq!(vim.handle(Key::char('y')), Action::Pending);
        assert_eq!(
            vim.handle(Key::char('y')),
            Action::Operate {
                op: Operator::Yank,
                motion: Motion::CurrentLine,
                count: 1,
            }
        );
        assert_eq!(vim.mode(), Mode::Normal);
    }

    #[test]
    fn cc_changes_line() {
        let mut vim = Vim::new();
        assert_eq!(vim.handle(Key::char('c')), Action::Pending);
        assert_eq!(
            vim.handle(Key::char('c')),
            Action::Operate {
                op: Operator::Change,
                motion: Motion::CurrentLine,
                count: 1,
            }
        );
        // Change leaves you in Insert mode
        assert_eq!(vim.mode(), Mode::Insert);
    }

    #[test]
    fn dw_deletes_word() {
        let mut vim = Vim::new();
        assert_eq!(vim.handle(Key::char('d')), Action::Pending);
        assert_eq!(
            vim.handle(Key::char('w')),
            Action::Operate {
                op: Operator::Delete,
                motion: Motion::WordForward,
                count: 1,
            }
        );
        assert_eq!(vim.mode(), Mode::Normal);
    }

    #[test]
    fn c_dollar_changes_to_end_of_line() {
        let mut vim = Vim::new();
        assert_eq!(vim.handle(Key::char('c')), Action::Pending);
        assert_eq!(
            vim.handle(Key::char('$')),
            Action::Operate {
                op: Operator::Change,
                motion: Motion::LineEnd,
                count: 1,
            }
        );
        assert_eq!(vim.mode(), Mode::Insert);
    }

    #[test]
    fn counted_operator() {
        let mut vim = Vim::new();
        assert_eq!(vim.handle(Key::char('3')), Action::Pending);
        assert_eq!(vim.handle(Key::char('d')), Action::Pending);
        assert_eq!(
            vim.handle(Key::char('w')),
            Action::Operate {
                op: Operator::Delete,
                motion: Motion::WordForward,
                count: 3,
            }
        );
    }

    #[test]
    fn d_j_deletes_down() {
        let mut vim = Vim::new();
        assert_eq!(vim.handle(Key::char('d')), Action::Pending);
        assert_eq!(
            vim.handle(Key::char('j')),
            Action::Operate {
                op: Operator::Delete,
                motion: Motion::Down,
                count: 1,
            }
        );
    }

    #[test]
    fn visual_d_deletes_selection() {
        let mut vim = Vim::new();
        vim.handle(Key::char('v'));
        assert_eq!(
            vim.handle(Key::char('d')),
            Action::Operate {
                op: Operator::Delete,
                motion: Motion::CurrentLine,
                count: 1,
            }
        );
        assert_eq!(vim.mode(), Mode::Normal);
    }

    #[test]
    fn visual_y_yanks_selection() {
        let mut vim = Vim::new();
        vim.handle(Key::char('v'));
        assert_eq!(
            vim.handle(Key::char('y')),
            Action::Operate {
                op: Operator::Yank,
                motion: Motion::CurrentLine,
                count: 1,
            }
        );
        assert_eq!(vim.mode(), Mode::Normal);
    }

    #[test]
    fn visual_c_changes_selection() {
        let mut vim = Vim::new();
        vim.handle(Key::char('v'));
        assert_eq!(
            vim.handle(Key::char('c')),
            Action::Operate {
                op: Operator::Change,
                motion: Motion::CurrentLine,
                count: 1,
            }
        );
        assert_eq!(vim.mode(), Mode::Insert);
    }

    #[test]
    fn esc_cancels_operator() {
        let mut vim = Vim::new();
        vim.handle(Key::char('d'));
        assert_eq!(vim.mode(), Mode::OperatorPending(Operator::Delete));
        vim.handle(Key::special(KeyCode::Esc));
        assert_eq!(vim.mode(), Mode::Normal);
    }

    #[test]
    fn unknown_key_cancels_operator() {
        let mut vim = Vim::new();
        vim.handle(Key::char('d'));
        // 'z' is not a recognized motion or operator
        vim.handle(Key::char('z'));
        assert_eq!(vim.mode(), Mode::Normal);
    }

    // ---- M17: pending_count overflow guard ----

    #[test]
    fn pending_count_clamps_to_max() {
        let mut vim = Vim::new();
        // Type a lot of 9s — should clamp to MAX_COUNT
        for _ in 0..10 {
            vim.handle(Key::char('9'));
        }
        assert_eq!(vim.pending_count, Some(MAX_COUNT));
    }
}
