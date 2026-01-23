//! Schema-related domain types shared across the UI and command crates.

use narwhal_core::{Schema, Table};

/// One schema together with the list of tables it contains, as surfaced to
/// the sidebar and the completion engine.
pub type SchemaListing = (Schema, Vec<Table>);
