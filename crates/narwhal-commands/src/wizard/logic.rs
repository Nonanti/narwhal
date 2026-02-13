//! Wizard step transitions, validation and `ConnectionConfig`
//! construction.

use narwhal_core::{ConnectionConfig, ConnectionParams};
use secrecy::{ExposeSecret, SecretString};
use uuid::Uuid;

use super::fields::{server_fields, text, WizardField, WizardFieldKind, WizardFieldValue};
use super::path::{complete_path, PathCompletion};
use super::state::{Built, ConnectionWizard, DRIVERS};

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
                        // Sprint 6 (M10): drop the prod `unreachable!`.
                        // A future field-mutation bug that lands a
                        // Public value here would panic in release; we
                        // now log + skip so the wizard surfaces the
                        // misconfiguration instead of crashing.
                        password = match &field.value {
                            WizardFieldValue::Public(_) => {
                                tracing::warn!(
                                    target: "narwhal::wizard",
                                    "password field unexpectedly held a Public value; skipping",
                                );
                                None
                            }
                            WizardFieldValue::Secret(s) => Some(SecretString::new(
                                s.expose_secret().to_owned().into_boxed_str(),
                            )),
                        };
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
