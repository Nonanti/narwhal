//! narwhal-vim — modal-editor state machine.
//!
//! This crate is intentionally UI-agnostic: it does not know about ratatui
//! or crossterm. It accepts logical [`Key`] events and emits [`Action`]s.
//! The TUI layer translates raw key events into [`Key`] and applies actions
//! to the editor buffer.
//!
//! Design goals
//! ============
//! * Pure state machine, fully unit-testable.
//! * Counts (`5j`), operators (`d`, `y`, `c`), and motions composed together.
//! * Sub-modes: Normal, Insert, Visual (char + line), Command-line.
//!
//! The first cut here only implements the minimum needed for the skeleton
//! to compile and to show "we picked the right shape". Motions/operators
//! are stubbed and will grow in follow-up commits.

pub mod action;
pub mod key;
pub mod machine;
pub mod mode;

pub use action::{Action, Motion, Operator};
pub use key::{Key, KeyCode, KeyMod};
pub use machine::Vim;
pub use mode::Mode;
