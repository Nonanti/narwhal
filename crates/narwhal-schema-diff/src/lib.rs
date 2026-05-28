//! Structural diff between two database schemas, plus DDL emission.
//!
//! Consumes `TableSchema` slices (the
//! same shape `narwhal-diagram` already reads from the drivers) and
//! produces a [`SchemaDiff`] describing what is missing, what is
//! extra, and what differs. The diff is dialect-agnostic; DDL is
//! emitted per-driver by the [`emit::DdlEmitter`] implementations.
//!
//! ## Direction
//!
//! Throughout the crate, **`source`** is the desired state and
//! **`target`** is the database that will be migrated. Emitted DDL
//! transforms `target` into `source`. A table that exists in `source`
//! but not `target` is therefore [`TableChange::Added`] (we *will*
//! add it to target), not "removed from source".
//!
//! ## Determinism
//!
//! Every collection returned by [`diff()`] is sorted by qualified name.
//! Running the same diff twice produces byte-identical output, which
//! lets CI gate schema drift on a stable hash.
//!
//! ## Cycle / self-diff
//!
//! [`diff()`] does *not* short-circuit when the two inputs are the same
//! slice — that decision belongs to the caller. The TUI dispatcher
//! adds the guard so a `:schema-diff prod prod` produces a friendly
//! "nothing to do" status instead of an empty modal.
//!
//! ## Scope (v2.0)
//!
//! In scope:
//! * tables: added / removed / changed
//! * columns: added / removed / type / nullable / default
//! * indexes: added / removed / changed
//! * foreign keys: added / removed / changed
//! * unique constraints: added / removed / changed
//!
//! Out of scope (deferred to v2.4+):
//! * views, materialised views, functions, procedures
//! * sequences, custom types, extensions
//! * permissions / GRANTs
//! * row-level data diffs

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod diff;
pub mod emit;
pub mod normalise;

pub use diff::{
    ColumnChange, ForeignKeyChange, IndexChange, SchemaDiff, TableChange, UniqueConstraintChange,
    diff,
};
pub use normalise::{canonical_type, defaults_equal};

/// Errors raised by this crate.
///
/// DDL emission is fallible (a dialect may not support a requested
/// change, e.g. `SQLite` cannot drop a NOT NULL constraint without
/// a table rebuild) so emitters surface that situation through this
/// enum instead of silently producing broken SQL.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SchemaDiffError {
    /// The dialect does not support the requested change.
    ///
    /// `dialect` is the emitter that refused; `what` is a human-
    /// readable description of the operation. The TUI surfaces this
    /// verbatim in the modal's status footer so the user can see why
    /// a particular line is a comment rather than a real `ALTER`.
    #[error("{dialect}: unsupported — {what}")]
    Unsupported {
        /// Dialect name (`"postgres"`, `"mysql"`, etc.).
        dialect: &'static str,
        /// What was attempted.
        what: String,
    },
}
