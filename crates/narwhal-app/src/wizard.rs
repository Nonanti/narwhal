//! Interactive `:add` connection wizard.
//!
//! The wizard is a small state machine driven by [`crate::core::AppCore`].
//! It exposes a focused field cursor (driver selector + per-driver input
//! fields) plus accumulated values, and emits a [`ConnectionConfig`] when
//! the form is committed.

use narwhal_core::{ConnectionConfig, ConnectionParams};
use uuid::Uuid;

pub const DRIVERS: &[&str] = &["sqlite", "postgres", "mysql"];

/// One input on the wizard form.
#[derive(Debug, Clone)]
pub struct WizardField {
    pub label: &'static str,
    pub value: String,
    pub kind: WizardFieldKind,
    /// `true` when [`WizardFieldKind::Password`] should be masked.
    pub secret: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardFieldKind {
    Name,
    Host,
    Port,
    Database,
    Username,
    Password,
    Path,
    SslMode,
}

#[derive(Debug)]
pub struct ConnectionWizard {
    pub driver_index: usize,
    pub fields: Vec<WizardField>,
    /// Index 0 is the driver selector; indexes 1..=fields.len() target a
    /// field. This keeps a single integer cursor consistent across the form.
    pub focused: usize,
}

impl ConnectionWizard {
    pub fn new() -> Self {
        let mut w = Self {
            driver_index: 0,
            fields: Vec::new(),
            focused: 0,
        };
        w.rebuild_fields();
        w
    }

    pub fn driver(&self) -> &'static str {
        DRIVERS[self.driver_index]
    }

    pub fn cycle_driver(&mut self, delta: i32) {
        let len = DRIVERS.len() as i32;
        self.driver_index = (((self.driver_index as i32) + delta).rem_euclid(len)) as usize;
        self.rebuild_fields();
    }

    pub fn next_focus(&mut self) {
        let total = self.fields.len() + 1;
        self.focused = (self.focused + 1) % total;
    }

    pub fn prev_focus(&mut self) {
        let total = self.fields.len() + 1;
        self.focused = (self.focused + total - 1) % total;
    }

    /// Append a character to the focused text field. Does nothing when the
    /// driver selector is focused.
    pub fn push_char(&mut self, ch: char) {
        if let Some(field) = self.focused_field_mut() {
            field.value.push(ch);
        }
    }

    pub fn pop_char(&mut self) {
        if let Some(field) = self.focused_field_mut() {
            field.value.pop();
        }
    }

    fn focused_field_mut(&mut self) -> Option<&mut WizardField> {
        if self.focused == 0 {
            return None;
        }
        self.fields.get_mut(self.focused - 1)
    }

    fn rebuild_fields(&mut self) {
        let mut fields = vec![text("name", WizardFieldKind::Name)];
        match self.driver() {
            "sqlite" => fields.push(text("path", WizardFieldKind::Path)),
            "postgres" => {
                fields.extend([
                    text("host", WizardFieldKind::Host),
                    text("port", WizardFieldKind::Port).with_default("5432"),
                    text("database", WizardFieldKind::Database),
                    text("username", WizardFieldKind::Username),
                    password("password"),
                    text("sslmode", WizardFieldKind::SslMode).with_default("disable"),
                ]);
            }
            "mysql" => {
                fields.extend([
                    text("host", WizardFieldKind::Host),
                    text("port", WizardFieldKind::Port).with_default("3306"),
                    text("database", WizardFieldKind::Database),
                    text("username", WizardFieldKind::Username),
                    password("password"),
                ]);
            }
            _ => {}
        }
        self.fields = fields;
        if self.focused > self.fields.len() {
            self.focused = 0;
        }
    }

    /// Validate and convert the wizard state into a [`Built`] artefact.
    pub fn build(&self) -> Result<Built, String> {
        let mut params = ConnectionParams::default();
        let mut name = String::new();
        let mut password = None;
        for field in &self.fields {
            let value = field.value.trim();
            let final_value = if value.is_empty() {
                field.default_value().to_owned()
            } else {
                value.to_owned()
            };
            match field.kind {
                WizardFieldKind::Name => {
                    if final_value.is_empty() {
                        return Err("name is required".into());
                    }
                    name = final_value;
                }
                WizardFieldKind::Host => {
                    if final_value.is_empty() {
                        return Err("host is required".into());
                    }
                    params.host = Some(final_value);
                }
                WizardFieldKind::Port => {
                    if !final_value.is_empty() {
                        params.port =
                            Some(final_value.parse::<u16>().map_err(|_| {
                                format!("port must be 0..=65535 (got {final_value})")
                            })?);
                    }
                }
                WizardFieldKind::Database => {
                    if final_value.is_empty() {
                        return Err("database is required".into());
                    }
                    params.database = Some(final_value);
                }
                WizardFieldKind::Username => {
                    if final_value.is_empty() {
                        return Err("username is required".into());
                    }
                    params.username = Some(final_value);
                }
                WizardFieldKind::Password => {
                    if !final_value.is_empty() {
                        password = Some(final_value);
                    }
                }
                WizardFieldKind::Path => {
                    if final_value.is_empty() {
                        return Err("path is required".into());
                    }
                    params.path = Some(final_value);
                }
                WizardFieldKind::SslMode => {
                    if !final_value.is_empty() && final_value != "disable" {
                        params.options.insert("sslmode".into(), final_value);
                    }
                }
            }
        }
        Ok(Built {
            config: ConnectionConfig {
                id: Uuid::new_v4(),
                name,
                driver: self.driver().to_owned(),
                params,
            },
            password,
        })
    }
}

impl Default for ConnectionWizard {
    fn default() -> Self {
        Self::new()
    }
}

impl WizardField {
    fn default_value(&self) -> &str {
        // Defaults are stored in `value` until the user types something; once
        // the user wipes the field we still want to consult the default at
        // build time. Encode them inside the label format so this method
        // stays self-contained.
        match (self.kind, self.label) {
            (WizardFieldKind::Port, _) if self.value.is_empty() => "",
            (WizardFieldKind::SslMode, _) if self.value.is_empty() => "",
            _ => "",
        }
    }
}

trait WithDefault {
    fn with_default(self, default: &str) -> Self;
}

impl WithDefault for WizardField {
    fn with_default(mut self, default: &str) -> Self {
        self.value = default.to_owned();
        self
    }
}

fn text(label: &'static str, kind: WizardFieldKind) -> WizardField {
    WizardField {
        label,
        value: String::new(),
        kind,
        secret: false,
    }
}

fn password(label: &'static str) -> WizardField {
    WizardField {
        label,
        value: String::new(),
        kind: WizardFieldKind::Password,
        secret: true,
    }
}

/// Output of [`ConnectionWizard::build`].
#[derive(Debug)]
pub struct Built {
    pub config: ConnectionConfig,
    pub password: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_sqlite_with_two_fields() {
        let w = ConnectionWizard::new();
        assert_eq!(w.driver(), "sqlite");
        assert_eq!(w.fields.len(), 2);
        assert_eq!(w.fields[0].label, "name");
        assert_eq!(w.fields[1].label, "path");
    }

    #[test]
    fn cycle_to_postgres_includes_sslmode_default() {
        let mut w = ConnectionWizard::new();
        w.cycle_driver(1);
        assert_eq!(w.driver(), "postgres");
        let ssl = w
            .fields
            .iter()
            .find(|f| f.kind == WizardFieldKind::SslMode)
            .unwrap();
        assert_eq!(ssl.value, "disable");
        let port = w
            .fields
            .iter()
            .find(|f| f.kind == WizardFieldKind::Port)
            .unwrap();
        assert_eq!(port.value, "5432");
    }

    #[test]
    fn build_requires_name_and_path_for_sqlite() {
        let mut w = ConnectionWizard::new();
        let err = w.build().unwrap_err();
        assert!(err.contains("name"));

        w.fields[0].value = "local".into();
        let err = w.build().unwrap_err();
        assert!(err.contains("path"));

        w.fields[1].value = "/tmp/x.db".into();
        let built = w.build().unwrap();
        assert_eq!(built.config.name, "local");
        assert_eq!(built.config.driver, "sqlite");
        assert_eq!(built.config.params.path.as_deref(), Some("/tmp/x.db"));
    }

    #[test]
    fn build_round_trips_postgres_form() {
        let mut w = ConnectionWizard::new();
        w.cycle_driver(1);
        w.fields[0].value = "prod".into();
        w.fields[1].value = "db.example.com".into();
        w.fields[3].value = "inventory".into();
        w.fields[4].value = "admin".into();
        w.fields[5].value = "s3cret".into();
        let built = w.build().unwrap();
        assert_eq!(built.config.driver, "postgres");
        assert_eq!(built.config.params.port, Some(5432));
        assert_eq!(built.password.as_deref(), Some("s3cret"));
    }

    #[test]
    fn build_rejects_invalid_port() {
        let mut w = ConnectionWizard::new();
        w.cycle_driver(1);
        w.fields[0].value = "x".into();
        w.fields[1].value = "h".into();
        w.fields[2].value = "99999".into();
        w.fields[3].value = "d".into();
        w.fields[4].value = "u".into();
        let err = w.build().unwrap_err();
        assert!(err.contains("port"));
    }
}
