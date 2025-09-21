use serde::{Deserialize, Serialize};

/// Static description of features a driver supports.
///
/// The UI inspects [`Capabilities`] to enable or disable commands without
/// hard-coding driver names. New flags are added by extending this struct;
/// existing drivers default to `false` for unknown features.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Capabilities {
    /// Driver supports explicit transactions (`BEGIN`/`COMMIT`/`ROLLBACK`).
    pub transactions: bool,
    /// Driver can request asynchronous cancellation of an in-flight query.
    pub cancellation: bool,
    /// Driver exposes more than one logical schema/namespace.
    pub multiple_schemas: bool,
    /// Driver supports server-side prepared statements with parameter binding.
    pub prepared_statements: bool,
    /// Driver supports nested transactions via savepoints.
    pub savepoints: bool,
    /// Driver can report row counts for `UPDATE`/`DELETE` statements.
    pub rows_affected: bool,
}

impl Capabilities {
    /// Builder-style mutation used by drivers to assemble their capability set.
    #[must_use]
    pub const fn with_transactions(mut self, value: bool) -> Self {
        self.transactions = value;
        self
    }

    #[must_use]
    pub const fn with_cancellation(mut self, value: bool) -> Self {
        self.cancellation = value;
        self
    }

    #[must_use]
    pub const fn with_multiple_schemas(mut self, value: bool) -> Self {
        self.multiple_schemas = value;
        self
    }

    #[must_use]
    pub const fn with_prepared_statements(mut self, value: bool) -> Self {
        self.prepared_statements = value;
        self
    }

    #[must_use]
    pub const fn with_savepoints(mut self, value: bool) -> Self {
        self.savepoints = value;
        self
    }

    #[must_use]
    pub const fn with_rows_affected(mut self, value: bool) -> Self {
        self.rows_affected = value;
        self
    }
}
