//! Wizard state machine: which step, focused field, accumulated
//! field values, and the `Built` artefact returned when the form is
//! committed.

use std::fmt;

use narwhal_core::ConnectionConfig;
use secrecy::SecretString;
use uuid::Uuid;

use super::fields::{WizardField, WizardFieldKind, WizardFieldValue};

pub const DRIVERS: &[&str] = &["sqlite", "postgres", "mysql", "clickhouse", "duckdb"];

#[derive(Debug)]
pub struct ConnectionWizard {
    pub driver_index: usize,
    pub fields: Vec<WizardField>,
    /// Index 0 is the driver selector; indexes `1..=fields.len()` target a
    /// field. This keeps a single integer cursor consistent across the form.
    pub focused: usize,
    /// `Some(uuid)` when the wizard is editing an existing connection.
    /// `commit_wizard` updates the entry in place instead of pushing a new
    /// one and the name-collision check is relaxed for the original name.
    pub existing_id: Option<Uuid>,
}

impl Default for ConnectionWizard {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Built {
    pub config: ConnectionConfig,
    pub password: Option<SecretString>,
}

impl fmt::Debug for Built {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Built")
            .field("config", &self.config)
            // Never include the password in debug output.
            .field("password", &self.password.as_ref().map(|_| "***"))
            .finish()
    }
}
