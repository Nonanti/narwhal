//! Interactive `:add` connection wizard.
//!
//! Small state machine: driver selector plus per-driver fields,
//! producing a `ConnectionConfig` (and an optional `SecretString`
//! password) on commit. Secrets stay wrapped through every layer.

mod fields;
mod logic;
mod path;
mod state;

pub use fields::{WizardField, WizardFieldKind, WizardFieldValue};
pub use path::PathCompletion;
pub use state::{Built, ConnectionWizard, DRIVERS};

#[cfg(test)]
mod tests {
    use super::*;
    use super::fields::{WizardFieldKind, WizardFieldValue};
    use secrecy::{ExposeSecret, SecretString};
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
        assert_eq!(ssl.value.expose(), "prefer");
        let port = w
            .fields
            .iter()
            .find(|f| f.kind == WizardFieldKind::Port)
            .unwrap();
        assert_eq!(port.value.expose(), "5432");
    }

    #[test]
    fn build_requires_name_and_path_for_sqlite() {
        let mut w = ConnectionWizard::new();
        let err = w.build().unwrap_err();
        assert!(err.contains("name"));

        w.fields[0].value = WizardFieldValue::Public("local".into());
        let err = w.build().unwrap_err();
        assert!(err.contains("path"));

        w.fields[1].value = WizardFieldValue::Public("/tmp/x.db".into());
        let built = w.build().unwrap();
        assert_eq!(built.config.name, "local");
        assert_eq!(built.config.driver, "sqlite");
        assert_eq!(built.config.params.path.as_deref(), Some("/tmp/x.db"));
    }

    #[test]
    fn build_round_trips_postgres_form() {
        let mut w = ConnectionWizard::new();
        w.cycle_driver(1);
        w.fields[0].value = WizardFieldValue::Public("prod".into());
        w.fields[1].value = WizardFieldValue::Public("db.example.com".into());
        w.fields[3].value = WizardFieldValue::Public("inventory".into());
        w.fields[4].value = WizardFieldValue::Public("admin".into());
        w.fields[5].value =
            WizardFieldValue::Secret(SecretString::new("s3cret".to_owned().into_boxed_str()));
        let built = w.build().unwrap();
        assert_eq!(built.config.driver, "postgres");
        assert_eq!(built.config.params.port, Some(5432));
        assert_eq!(
            built.password.as_ref().map(|s| s.expose_secret() as &str),
            Some("s3cret")
        );
    }

    #[test]
    fn build_rejects_invalid_port() {
        let mut w = ConnectionWizard::new();
        w.cycle_driver(1);
        w.fields[0].value = WizardFieldValue::Public("x".into());
        w.fields[1].value = WizardFieldValue::Public("h".into());
        w.fields[2].value = WizardFieldValue::Public("99999".into());
        w.fields[3].value = WizardFieldValue::Public("d".into());
        w.fields[4].value = WizardFieldValue::Public("u".into());
        let err = w.build().unwrap_err();
        assert!(err.contains("port"));
    }

    /// H13 regression: password field values are SecretString, not plain String.
    #[test]
    fn password_field_is_secret_variant() {
        let w = ConnectionWizard::new();
        let pw_field = w
            .fields
            .iter()
            .find(|f| f.kind == WizardFieldKind::Password);
        // sqlite has no password field
        assert!(pw_field.is_none());

        let mut w = ConnectionWizard::new();
        w.cycle_driver(1); // postgres
        let pw_field = w
            .fields
            .iter()
            .find(|f| f.kind == WizardFieldKind::Password)
            .unwrap();
        assert!(pw_field.secret);
        assert!(matches!(pw_field.value, WizardFieldValue::Secret(_)));
    }

    /// H13 regression: Debug output never leaks the password.
    #[test]
    fn debug_does_not_leak_password() {
        let mut w = ConnectionWizard::new();
        w.cycle_driver(1);
        w.fields[5].value =
            WizardFieldValue::Secret(SecretString::new("hunter2".to_owned().into_boxed_str()));
        let debug_output = format!("{w:?}");
        assert!(!debug_output.contains("hunter2"));
        assert!(debug_output.contains("***"));
    }

    /// H13 regression: Built Debug output never leaks the password.
    #[test]
    fn built_debug_does_not_leak_password() {
        let mut w = ConnectionWizard::new();
        w.cycle_driver(1);
        w.fields[0].value = WizardFieldValue::Public("prod".into());
        w.fields[1].value = WizardFieldValue::Public("db.example.com".into());
        w.fields[3].value = WizardFieldValue::Public("inventory".into());
        w.fields[4].value = WizardFieldValue::Public("admin".into());
        w.fields[5].value =
            WizardFieldValue::Secret(SecretString::new("topsecret".to_owned().into_boxed_str()));
        let built = w.build().unwrap();
        let debug_output = format!("{built:?}");
        assert!(!debug_output.contains("topsecret"));
        assert!(debug_output.contains("***"));
    }

    /// H13 regression: push_char and pop_char work on Secret fields.
    #[test]
    fn secret_field_push_pop() {
        let mut w = ConnectionWizard::new();
        w.cycle_driver(1); // postgres
        let pw_idx = w
            .fields
            .iter()
            .position(|f| f.kind == WizardFieldKind::Password)
            .unwrap();
        // Focus the password field (focused 0 = driver, 1+ = field index).
        w.focused = pw_idx + 1;
        w.push_char('a');
        w.push_char('b');
        w.push_char('c');
        assert_eq!(w.fields[pw_idx].value.expose(), "abc");
        w.pop_char();
        assert_eq!(w.fields[pw_idx].value.expose(), "ab");
    }
}
