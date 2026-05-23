//! Per-driver input field model used by the wizard.

use std::fmt;

use secrecy::{ExposeSecret, SecretString};

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

pub enum WizardFieldValue {
    Public(String),
    Secret(SecretString),
}

impl WizardFieldValue {
    /// Returns the visible/display length of the value (for cursor
    /// positioning). For secret fields, this is the actual character count.
    pub fn len(&self) -> usize {
        match self {
            Self::Public(s) => s.len(),
            Self::Secret(s) => s.expose_secret().len(),
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
            Self::Public(s) => s.push(ch),
            Self::Secret(s) => {
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
            Self::Public(s) => {
                s.pop();
            }
            Self::Secret(s) => {
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
            Self::Public(s) => s,
            Self::Secret(s) => s.expose_secret(),
        }
    }
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
    SslRootCert,
    SslCert,
    SslKey,
    /// SSH bastion host (`ssh_host`). When empty, no tunnel is opened.
    SshHost,
    SshPort,
    SshUser,
    /// Path to the SSH identity (private key). Optional; falls back to
    /// `~/.ssh/config` + the agent when blank.
    SshKey,
}

impl WizardField {
    pub const fn default_value(&self) -> &'static str {
        // When the user clears the field, we still want to consult the
        // originally-set default. The `value` field is seeded with the
        // default in `with_default` so empty means the user cleared it;
        // in that case return empty and let `build()` fall back to the
        // struct defaults.
        ""
    }
}

pub(super) trait WithDefault {
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

pub(super) fn server_fields(default_port: &str) -> Vec<WizardField> {
    vec![
        text("host", WizardFieldKind::Host),
        text("port", WizardFieldKind::Port).with_default(default_port),
        text("database", WizardFieldKind::Database),
        text("username", WizardFieldKind::Username),
        password("password"),
        text("ssl_mode", WizardFieldKind::SslMode)
            .with_default("prefer")
            .with_placeholder("disable|prefer|require|verify-ca|verify-full"),
        text("ssl_root_cert", WizardFieldKind::SslRootCert).with_placeholder("/path/to/ca.pem"),
        text("ssl_cert", WizardFieldKind::SslCert).with_placeholder("/path/to/client-cert.pem"),
        text("ssl_key", WizardFieldKind::SslKey).with_placeholder("/path/to/client-key.pem"),
        // SSH bastion. Leave ssh_host blank to disable the tunnel.
        text("ssh_host", WizardFieldKind::SshHost)
            .with_placeholder("jump.example.com (leave blank to disable)"),
        text("ssh_port", WizardFieldKind::SshPort).with_placeholder("22"),
        text("ssh_user", WizardFieldKind::SshUser).with_placeholder("ubuntu"),
        text("ssh_key", WizardFieldKind::SshKey)
            .with_placeholder("~/.ssh/id_ed25519 (uses agent if blank)"),
    ]
}

pub(super) const fn text(label: &'static str, kind: WizardFieldKind) -> WizardField {
    WizardField {
        label,
        value: WizardFieldValue::Public(String::new()),
        kind,
        secret: false,
        placeholder: "",
    }
}

pub(super) fn password(label: &'static str) -> WizardField {
    WizardField {
        label,
        value: WizardFieldValue::Secret(SecretString::new(String::new().into_boxed_str())),
        kind: WizardFieldKind::Password,
        secret: true,
        placeholder: "",
    }
}
