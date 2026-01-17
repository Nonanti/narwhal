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
//! `CredentialStore::set` method, which exposes the secret *only* inside
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
    /// SSH bastion host (`ssh_host`). When empty, no tunnel is opened.
    SshHost,
    SshPort,
    SshUser,
    /// Path to the SSH identity (private key). Optional; falls back to
    /// `~/.ssh/config` + the agent when blank.
    SshKey,
}

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

impl ConnectionWizard {
    pub fn new() -> Self {
        let mut w = Self {
            driver_index: 0,
            fields: Vec::new(),
            focused: 0,
            existing_id: None,
        };
        w.rebuild_fields();
        w
    }

    /// Build a wizard pre-populated from an existing [`ConnectionConfig`].
    /// Used by `:url <dsn>` (pre-fill then let the user tweak before
    /// committing) and by `:edit <name>` (preserve the original id).
    ///
    /// `password` is optional; when present it lands in the password
    /// field as a [`SecretString`]. The wizard never reads the keyring,
    /// so callers wanting to surface the stored password must fetch it
    /// themselves before calling.
    pub fn from_config(
        config: &ConnectionConfig,
        password: Option<SecretString>,
        existing_id: Option<Uuid>,
    ) -> Self {
        let driver_index = DRIVERS
            .iter()
            .position(|d| *d == config.driver)
            .unwrap_or(0);
        let mut w = Self {
            driver_index,
            fields: Vec::new(),
            focused: 0,
            existing_id,
        };
        w.rebuild_fields();
        // Hydrate every rebuilt field from the config.
        for field in &mut w.fields {
            let next = match field.kind {
                WizardFieldKind::Name => Some(config.name.clone()),
                WizardFieldKind::Host => config.params.host.clone(),
                WizardFieldKind::Port => config.params.port.map(|p| p.to_string()),
                WizardFieldKind::Database => config.params.database.clone(),
                WizardFieldKind::Username => config.params.username.clone(),
                WizardFieldKind::Path => config.params.path.clone(),
                WizardFieldKind::SslMode => Some(match config.params.ssl_mode {
                    narwhal_core::SslMode::Disable => "disable".into(),
                    narwhal_core::SslMode::Prefer => "prefer".into(),
                    narwhal_core::SslMode::Require => "require".into(),
                    narwhal_core::SslMode::VerifyCa => "verify-ca".into(),
                    narwhal_core::SslMode::VerifyFull => "verify-full".into(),
                    _ => "prefer".into(),
                }),
                WizardFieldKind::SslRootCert => config
                    .params
                    .ssl_root_cert
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
                WizardFieldKind::SslCert => config
                    .params
                    .ssl_cert
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
                WizardFieldKind::SslKey => config
                    .params
                    .ssl_key
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
                WizardFieldKind::SshHost => config.params.ssh.as_ref().map(|s| s.host.clone()),
                WizardFieldKind::SshPort => config
                    .params
                    .ssh
                    .as_ref()
                    .and_then(|s| s.port)
                    .map(|p| p.to_string()),
                WizardFieldKind::SshUser => config.params.ssh.as_ref().map(|s| s.user.clone()),
                WizardFieldKind::SshKey => config
                    .params
                    .ssh
                    .as_ref()
                    .and_then(|s| s.key_path.as_ref())
                    .map(|p| p.to_string_lossy().into_owned()),
                WizardFieldKind::Password => None,
            };
            if let Some(v) = next {
                field.value = WizardFieldValue::Public(v);
            }
        }
        // Slot the password (if any) into the password field as a
        // SecretString so it stays zeroized on drop.
        if let Some(secret) = password {
            if let Some(field) = w
                .fields
                .iter_mut()
                .find(|f| f.kind == WizardFieldKind::Password)
            {
                field.value = WizardFieldValue::Secret(secret);
            }
        }
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

    pub fn focused_field(&self) -> Option<&WizardField> {
        if self.focused == 0 {
            return None;
        }
        self.fields.get(self.focused - 1)
    }

    /// True when the focused field expects a filesystem path. Used by
    /// the modal handler to repurpose Tab as a path-completion trigger
    /// instead of the usual focus advancer.
    pub fn focused_is_path(&self) -> bool {
        matches!(
            self.focused_field().map(|f| f.kind),
            Some(
                WizardFieldKind::Path
                    | WizardFieldKind::SslRootCert
                    | WizardFieldKind::SslCert
                    | WizardFieldKind::SslKey
            )
        )
    }

    /// Perform readline-style path completion against the focused
    /// field's current value.
    ///
    /// - No matches → [`PathCompletion::NoMatch`].
    /// - Exactly one match → the field is rewritten to the absolute
    ///   path (with a trailing `/` if it is a directory) and
    ///   [`PathCompletion::Single`] is returned.
    /// - Multiple matches → the field is extended to the longest
    ///   common prefix of the candidates and
    ///   [`PathCompletion::Multiple`] is returned with up to the first
    ///   eight basenames so the caller can render them in the status
    ///   bar.
    pub fn complete_focused_path(&mut self) -> PathCompletion {
        if !self.focused_is_path() {
            return PathCompletion::NoMatch;
        }
        let Some(field) = self.focused_field() else {
            return PathCompletion::NoMatch;
        };
        let current = field.value.expose().to_owned();
        let outcome = complete_path(&current);
        if let Some(new) = outcome.replacement.clone() {
            if let Some(f) = self.focused_field_mut() {
                f.value = WizardFieldValue::Public(new);
            }
        }
        outcome.report
    }
}

/// Report from [`ConnectionWizard::complete_focused_path`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PathCompletion {
    NoMatch,
    Single,
    Multiple { count: usize, samples: Vec<String> },
}

struct CompletionResult {
    replacement: Option<String>,
    report: PathCompletion,
}

fn complete_path(input: &str) -> CompletionResult {
    use std::path::Path;

    // Resolve `~` so completion works inside the home directory.
    let expanded = expand_tilde(input);
    let path = Path::new(&expanded);

    // Split into a directory + basename prefix. Trailing-slash inputs
    // list every child of the directory.
    let (dir, prefix): (std::path::PathBuf, String) =
        if expanded.is_empty() || expanded.ends_with('/') {
            (
                if expanded.is_empty() {
                    std::path::PathBuf::from(".")
                } else {
                    path.to_path_buf()
                },
                String::new(),
            )
        } else {
            (
                path.parent()
                    .filter(|p| !p.as_os_str().is_empty()).map_or_else(|| std::path::PathBuf::from("."), Path::to_path_buf),
                path.file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            )
        };

    let Ok(entries) = std::fs::read_dir(&dir) else {
        return CompletionResult {
            replacement: None,
            report: PathCompletion::NoMatch,
        };
    };
    let mut matches: Vec<(String, bool)> = entries
        .filter_map(std::result::Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with(&prefix) {
                return None;
            }
            // Skip dotfiles unless the user explicitly typed a leading dot.
            if name.starts_with('.') && !prefix.starts_with('.') {
                return None;
            }
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            Some((name, is_dir))
        })
        .collect();
    matches.sort_by(|a, b| a.0.cmp(&b.0));

    match matches.len() {
        0 => CompletionResult {
            replacement: None,
            report: PathCompletion::NoMatch,
        },
        1 => {
            let (name, is_dir) = &matches[0];
            let mut joined = dir.join(name).to_string_lossy().into_owned();
            if *is_dir {
                joined.push('/');
            }
            CompletionResult {
                replacement: Some(joined),
                report: PathCompletion::Single,
            }
        }
        _ => {
            // Extend to the longest common prefix so successive Tabs
            // converge on the user's target.
            let names: Vec<&str> = matches.iter().map(|(n, _)| n.as_str()).collect();
            let lcp = longest_common_prefix(&names);
            let replacement = if lcp.len() > prefix.len() {
                Some(dir.join(lcp).to_string_lossy().into_owned())
            } else {
                None
            };
            let samples: Vec<String> = matches.iter().take(8).map(|(n, _)| n.clone()).collect();
            CompletionResult {
                replacement,
                report: PathCompletion::Multiple {
                    count: matches.len(),
                    samples,
                },
            }
        }
    }
}

fn expand_tilde(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let mut p = std::path::PathBuf::from(home);
            p.push(rest);
            return p.to_string_lossy().into_owned();
        }
    }
    if s == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(home)
                .to_string_lossy()
                .into_owned();
        }
    }
    s.to_owned()
}

fn longest_common_prefix(strs: &[&str]) -> String {
    if strs.is_empty() {
        return String::new();
    }
    let mut prefix = strs[0].to_owned();
    for s in &strs[1..] {
        while !s.starts_with(&prefix) {
            prefix.pop();
            if prefix.is_empty() {
                return String::new();
            }
        }
    }
    prefix
}

impl ConnectionWizard {
    fn rebuild_fields(&mut self) {
        let mut fields = vec![text("name", WizardFieldKind::Name)];
        match self.driver() {
            "sqlite" | "duckdb" => fields.push(text("path", WizardFieldKind::Path)),
            "postgres" => {
                fields.extend(server_fields("5432"));
            }
            "mysql" => {
                fields.extend(server_fields("3306"));
            }
            "clickhouse" => {
                fields.extend(server_fields("8123"));
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
        // SSH tunnel pieces are accumulated outside the field loop so we
        // can validate the trio (`host` + `user`, optional port + key)
        // together at the end — a half-filled SSH block is a user
        // error worth surfacing rather than silently dropping.
        let mut ssh_host: Option<String> = None;
        let mut ssh_port: Option<u16> = None;
        let mut ssh_user: Option<String> = None;
        let mut ssh_key: Option<std::path::PathBuf> = None;
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
                WizardFieldKind::SshHost => {
                    if !final_value.is_empty() {
                        ssh_host = Some(final_value);
                    }
                }
                WizardFieldKind::SshPort => {
                    if !final_value.is_empty() {
                        ssh_port = Some(final_value.parse::<u16>().map_err(|_| {
                            format!("ssh_port must be 0..=65535 (got {final_value})")
                        })?);
                    }
                }
                WizardFieldKind::SshUser => {
                    if !final_value.is_empty() {
                        ssh_user = Some(final_value);
                    }
                }
                WizardFieldKind::SshKey => {
                    if !final_value.is_empty() {
                        ssh_key = Some(final_value.into());
                    }
                }
            }
        }
        // Build the SSH block only when the user actually filled in a
        // host. Requiring `ssh_user` alongside avoids cryptic failures
        // from the ssh subprocess ("hostname nor servname provided").
        if let Some(host) = ssh_host {
            let Some(user) = ssh_user else {
                return Err(
                    "ssh_user is required when ssh_host is set (ssh has no default user)".into(),
                );
            };
            // SshConfig is `#[non_exhaustive]`, so we go through the
            // `new` constructor and then mutate the optional fields.
            // Keeps cross-crate SemVer guarantees intact.
            let mut ssh = narwhal_core::SshConfig::new(host, user);
            ssh.port = ssh_port;
            ssh.key_path = ssh_key;
            params.ssh = Some(ssh);
        } else if ssh_user.is_some() || ssh_port.is_some() || ssh_key.is_some() {
            // Partial SSH config without a host is almost always a typo;
            // explicit error beats silent disable.
            return Err("ssh_host is required when any ssh_* field is set".into());
        }
        Ok(Built {
            config: ConnectionConfig {
                // Reuse the original id when editing so `connections.toml`
                // and the keyring entry stay in sync.
                id: self.existing_id.unwrap_or_else(Uuid::new_v4),
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
    const fn default_value(&self) -> &'static str {
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

/// The full set of fields shown for every network driver. Keeps the
/// per-driver match arms small and guarantees ssl/ssh ordering stays
/// consistent across postgres/mysql/clickhouse.
fn server_fields(default_port: &str) -> Vec<WizardField> {
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

const fn text(label: &'static str, kind: WizardFieldKind) -> WizardField {
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
