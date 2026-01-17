//! Modal keystroke processor.
//!
//! [`Vim`] consumes logical [`Key`] events and emits [`Action`]s describing
//! the buffer mutations the editor should perform. The state machine is
//! intentionally independent of any terminal back-end so it can be exercised
//! with plain unit tests.

#![forbid(unsafe_code)]
#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro,
    clippy::print_stdout,
    clippy::print_stderr
)]

pub mod action;
pub mod key;
pub mod machine;
pub mod mode;

pub use action::{Action, Motion, Operator, SearchDirection};
pub use key::{Key, KeyCode, KeyMod};
pub use machine::Vim;
pub use mode::Mode;
