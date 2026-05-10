//! Re-export of the candidate types that now live in `narwhal-domain`.
//!
//! `Completion` and `CompletionKind` moved out of this crate so the
//! result-pane state in `narwhal-domain` can name the candidate
//! without pulling `narwhal-commands` along. The engine that produces
//! candidates stays in this crate; this shim just keeps the old
//! `narwhal_commands::completion::{Completion, CompletionKind}` import
//! path working.

pub use narwhal_domain::completion::{Completion, CompletionKind};
