//! Interactive `:add` connection wizard.
//!
//! The wizard is a small state machine driven by [`crate::core::AppCore`].
//! It exposes a focused field cursor (driver selector + per-driver input
//! fields) plus accumulated values, and emits a [`ConnectionConfig`] when
//! the form is committed.
//!
//! # Secret handling (H13)
//!
//! Password fields store their value as [`secrecy::SecretString`] so that
//! the secret material is zeroized on drop. The `Debug` impl for
//! [`WizardField`] redacts secret values. The only place the password is
//! exposed is in [`ConnectionWizard::build`], where it is transferred into
//! the [`Built`] struct — still wrapped as `Option<SecretString>`. Callers
//! (e.g. `commit_wizard`) pass the `SecretString` directly to the async
//! [`CredentialStore::set`] method, which exposes the secret *only* inside
//! the keyring call.

use std::fmt;

use narwhal_core::{ConnectionConfig, ConnectionParams};
use secrecy::{ExposeSecret, SecretString};
use uuid::Uuid;

pub const DRIVERS: &[&str] = &["sqlite", "postgres", "mysql", "clickhouse", "duckdb"];

/// One input on the wizard form.
pub struct WizardField {
    pub label: &'static str,
    pub value: WizardFieldValue,
    pub kind: WizardFieldKind,
    /// `true` when [`WizardFieldKind::Password`] should be masked.
    pub secret: bool,
    /// Placeholder/default text shown before user types.
    pub placeholder: &'static str,
}

impl fmt::Debug for WizardField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never leak the secret value in debug output.
        let value_display = match &self.value {
            WizardFieldValue::Public(s) => s.as_str(),
            WizardFieldValue::Secret(_) => "***",
        };
        f.debug_struct("WizardField")
            .field("label", &self.label)
            .field("value", &value_display)
            .field("kind", &self.kind)
            .field("secret", &self.secret)
            .field("placeholder", &self.placeholder)
            .finish()
    }
}

/// Value stored in a wizard field. Public fields use plain `String`;
/// secret fields (passwords) use [`SecretString`] which is zeroized on drop.
#[non_exhaustive]
pub enum WizardFieldValue {
    Public(String),
    Secret(SecretString),
}

impl WizardFieldValue {
    /// Returns the visible/display length of the value (for cursor
    /// positioning). For secret fields, this is the actual character count.
    pub fn len(&self) -> usize {
        match self {
            WizardFieldValue::Public(s) => s.len(),
            WizardFieldValue::Secret(s) => s.expose_secret().len(),
        }
    }

    /// Returns `true` if the value is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Append a character. For secret fields, the character is placed inside
    /// the `SecretString`.
    pub fn push(&mut self, ch: char) {
        match self {
            WizardFieldValue::Public(s) => s.push(ch),
            WizardFieldValue::Secret(s) => {
                let mut plain = s.expose_secret().to_owned();
                plain.push(ch);
                // The old SecretString is dropped (its inner Box<str>
                // will be zeroized by SecretString's Drop impl).
                // We reconstruct from the new plain value.
                *s = SecretString::new(plain.into_boxed_str());
                // Note: `plain` was consumed by `into_boxed_str()`,
                // so there's no lingering String to zeroize.
            }
        }
    }

    /// Remove the last character.
    pub fn pop(&mut self) {
        match self {
            WizardFieldValue::Public(s) => {
                s.pop();
            }
            WizardFieldValue::Secret(s) => {
                let mut plain = s.expose_secret().to_owned();
                plain.pop();
                *s = SecretString::new(plain.into_boxed_str());
            }
        }
    }

    /// Returns the trimmed value as a plain `&str` for public fields,
    /// or exposes the secret for password fields.
    ///
    /// # Security
    /// For `Secret` variants, this exposes the secret material. Callers
    /// must not store or clone the returned reference beyond the
    /// immediate operation.
    pub fn expose(&self) -> &str {
        match self {
            WizardFieldValue::Public(s) => s,
            WizardFieldValue::Secret(s) => s.expose_secret(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WizardFieldKind {
    Name,
    Host,
    Port,
    Database,
    Username,
    Password,
    Path,
    SslMode,
    SslRootCert,
    SslCert,
    SslKey,
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
            "sqlite" | "duckdb" => fields.push(text("path", WizardFieldKind::Path)),
            "postgres" => {
                fields.extend([
                    text("host", WizardFieldKind::Host),
                    text("port", WizardFieldKind::Port).with_default("5432"),
                    text("database", WizardFieldKind::Database),
                    text("username", WizardFieldKind::Username),
                    password("password"),
                    text("ssl_mode", WizardFieldKind::SslMode)
                        .with_default("prefer")
                        .with_placeholder("disable|prefer|require|verify-ca|verify-full"),
                    text("ssl_root_cert", WizardFieldKind::SslRootCert)
                        .with_placeholder("/path/to/ca.pem"),
                    text("ssl_cert", WizardFieldKind::SslCert)
                        .with_placeholder("/path/to/client-cert.pem"),
                    text("ssl_key", WizardFieldKind::SslKey)
                        .with_placeholder("/path/to/client-key.pem"),
                ]);
            }
            "mysql" => {
                fields.extend([
                    text("host", WizardFieldKind::Host),
                    text("port", WizardFieldKind::Port).with_default("3306"),
                    text("database", WizardFieldKind::Database),
                    text("username", WizardFieldKind::Username),
                    password("password"),
                    text("ssl_mode", WizardFieldKind::SslMode)
                        .with_default("prefer")
                        .with_placeholder("disable|prefer|require|verify-ca|verify-full"),
                    text("ssl_root_cert", WizardFieldKind::SslRootCert)
                        .with_placeholder("/path/to/ca.pem"),
                    text("ssl_cert", WizardFieldKind::SslCert)
                        .with_placeholder("/path/to/client-cert.pem"),
                    text("ssl_key", WizardFieldKind::SslKey)
                        .with_placeholder("/path/to/client-key.pem"),
                ]);
            }
            "clickhouse" => {
                fields.extend([
                    text("host", WizardFieldKind::Host),
                    text("port", WizardFieldKind::Port).with_default("8123"),
                    text("database", WizardFieldKind::Database),
                    text("username", WizardFieldKind::Username),
                    password("password"),
                    text("ssl_mode", WizardFieldKind::SslMode)
                        .with_default("prefer")
                        .with_placeholder("disable|prefer|require|verify-ca|verify-full"),
                    text("ssl_root_cert", WizardFieldKind::SslRootCert)
                        .with_placeholder("/path/to/ca.pem"),
                    text("ssl_cert", WizardFieldKind::SslCert)
                        .with_placeholder("/path/to/client-cert.pem"),
                    text("ssl_key", WizardFieldKind::SslKey)
                        .with_placeholder("/path/to/client-key.pem"),
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
        let mut password: Option<SecretString> = None;
        for field in &self.fields {
            let value = field.value.expose().trim();
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
                    if !field.value.is_empty() {
                        // Clone the SecretString — the only copy beyond the
                        // field itself, and it will be consumed by
                        // `commit_wizard` → `credentials.set`.
                        password = Some(match &field.value {
                            WizardFieldValue::Public(_) => {
                                unreachable!("password field is always Secret")
                            }
                            WizardFieldValue::Secret(s) => {
                                SecretString::new(s.expose_secret().to_owned().into_boxed_str())
                            }
                        });
                    }
                }
                WizardFieldKind::Path => {
                    if final_value.is_empty() {
                        return Err("path is required".into());
                    }
                    params.path = Some(final_value);
                }
                WizardFieldKind::SslMode => {
                    if !final_value.is_empty() {
                        params.ssl_mode = match final_value.as_str() {
                            "disable" => narwhal_core::SslMode::Disable,
                            "prefer" => narwhal_core::SslMode::Prefer,
                            "require" => narwhal_core::SslMode::Require,
                            "verify-ca" => narwhal_core::SslMode::VerifyCa,
                            "verify-full" => narwhal_core::SslMode::VerifyFull,
                            other => {
                                return Err(format!(
                                    "invalid ssl_mode '{other}' \
                                     (use disable|prefer|require|verify-ca|verify-full)"
                                ));
                            }
                        };
                    }
                }
                WizardFieldKind::SslRootCert => {
                    if !final_value.is_empty() {
                        params.ssl_root_cert = Some(final_value.into());
                    }
                }
                WizardFieldKind::SslCert => {
                    if !final_value.is_empty() {
                        params.ssl_cert = Some(final_value.into());
                    }
                }
                WizardFieldKind::SslKey => {
                    if !final_value.is_empty() {
                        params.ssl_key = Some(final_value.into());
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
        // When the user clears the field, we still want to consult the
        // originally-set default. The `value` field is seeded with the
        // default in `with_default` so empty means the user cleared it;
        // in that case return empty and let `build()` fall back to the
        // struct defaults.
        ""
    }
}

trait WithDefault {
    fn with_default(self, default: &str) -> Self;
    fn with_placeholder(self, placeholder: &'static str) -> Self;
}

impl WithDefault for WizardField {
    fn with_default(mut self, default: &str) -> Self {
        self.value = WizardFieldValue::Public(default.to_owned());
        self
    }

    fn with_placeholder(mut self, placeholder: &'static str) -> Self {
        self.placeholder = placeholder;
        self
    }
}

fn text(label: &'static str, kind: WizardFieldKind) -> WizardField {
    WizardField {
        label,
        value: WizardFieldValue::Public(String::new()),
        kind,
        secret: false,
        placeholder: "",
    }
}

fn password(label: &'static str) -> WizardField {
    WizardField {
        label,
        value: WizardFieldValue::Secret(SecretString::new(String::new().into_boxed_str())),
        kind: WizardFieldKind::Password,
        secret: true,
        placeholder: "",
    }
}

/// Output of [`ConnectionWizard::build`].
///
/// The password, if present, is wrapped in [`SecretString`] so that it is
/// zeroized when this struct is dropped. Callers should pass the secret
/// directly to [`narwhal_config::CredentialStore::set`] which accepts
/// `SecretString`.
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
