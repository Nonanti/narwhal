//! v1 → v2 settings + connections migration.
//!
//! See `crates/narwhal-config/tests/migrate_v1_to_v2.rs` for the
//! kitchen-sink round-trip; this module ships the pure transform
//! and the file-level orchestration that the `narwhal
//! migrate-config` CLI invokes.

use std::path::{Path, PathBuf};

use crate::settings::{ConfigError, ConnectionsFile, Settings};

/// Options driving the file-level migration.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct MigrateOptions {
    /// When `true`, no files are written. The function still
    /// computes the v2 payload and returns it via
    /// [`MigrateReport`] so the caller can diff.
    pub dry_run: bool,
    /// Suffix appended to the v1 file to produce the backup name.
    /// Defaults to `.v1.bak`.
    pub backup_suffix: String,
    /// When `true`, overwrite an existing backup file. Without this
    /// the function refuses and returns
    /// [`ConfigError::Validation`] so the user doesn't lose an
    /// earlier backup.
    pub force: bool,
}

impl Default for MigrateOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            backup_suffix: ".v1.bak".to_owned(),
            force: false,
        }
    }
}

impl MigrateOptions {
    /// Functional-update constructor. The struct is `#[non_exhaustive]`
    /// so callers outside this crate can't use struct-literal syntax.
    #[must_use]
    pub fn with(f: impl FnOnce(&mut Self)) -> Self {
        let mut o = Self::default();
        f(&mut o);
        o
    }
}

/// Outcome of a `migrate-config` invocation.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum MigrateOutcome {
    /// File didn't exist (nothing to do).
    Absent,
    /// File was already v2 (nothing to do; idempotent path).
    AlreadyV2,
    /// File was migrated. `backup_path` is `None` in dry-run mode.
    Migrated {
        backup_path: Option<PathBuf>,
        rendered_v2: String,
    },
}

/// Aggregate report covering both `settings.toml` and
/// `connections.toml`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct MigrateReport {
    pub settings: MigrateOutcome,
    pub connections: MigrateOutcome,
}

/// Run the full migration for a pair of v1 paths. Idempotent: a
/// second invocation against already-v2 files returns
/// [`MigrateOutcome::AlreadyV2`] for both halves with no side
/// effects.
pub fn migrate(
    settings_path: &Path,
    connections_path: &Path,
    opts: &MigrateOptions,
) -> Result<MigrateReport, ConfigError> {
    let settings = migrate_settings(settings_path, opts)?;
    let connections = migrate_connections(connections_path, opts)?;
    Ok(MigrateReport {
        settings,
        connections,
    })
}

/// Migrate a single `settings.toml`. Public so the CLI can invoke
/// the two halves independently when the user passes only
/// `--settings-path` or only `--connections-path`.
pub fn migrate_settings(path: &Path, opts: &MigrateOptions) -> Result<MigrateOutcome, ConfigError> {
    if !path.exists() {
        return Ok(MigrateOutcome::Absent);
    }
    let text = std::fs::read_to_string(path)?;
    match peek_settings_schema(&text)? {
        Some(2) => return Ok(MigrateOutcome::AlreadyV2),
        Some(1) | None => {}
        Some(n) => return Err(ConfigError::UnsupportedSchema(n)),
    }
    // v1 layout: same field set as v2 minus the new sections, all at
    // top level. `Settings::load_v1_from_str` is a thin wrapper that
    // uses the v2 struct with serde defaults — the v2 additions stay
    // at their defaults, which is lossless.
    let v2 = Settings::load_v1_from_str(&text)?;
    let rendered = render_settings_v2(&v2)?;
    let backup_path = backup_and_write(path, &text, &rendered, opts)?;
    Ok(MigrateOutcome::Migrated {
        backup_path,
        rendered_v2: rendered,
    })
}

/// Migrate a single `connections.toml`.
pub fn migrate_connections(
    path: &Path,
    opts: &MigrateOptions,
) -> Result<MigrateOutcome, ConfigError> {
    if !path.exists() {
        return Ok(MigrateOutcome::Absent);
    }
    let text = std::fs::read_to_string(path)?;
    match peek_connections_schema(&text)? {
        Some(2) => return Ok(MigrateOutcome::AlreadyV2),
        Some(1) | None => {}
        Some(n) => return Err(ConfigError::UnsupportedSchema(n)),
    }
    // v1: same struct shape, just no schema_version. Re-serialise
    // with the discriminant injected.
    let mut file = ConnectionsFile::load_v1(path)?;
    file.schema_version = Some(crate::settings::CURRENT_SCHEMA_VERSION);
    let rendered = toml::to_string_pretty(&file)?;
    let backup_path = backup_and_write(path, &text, &rendered, opts)?;
    Ok(MigrateOutcome::Migrated {
        backup_path,
        rendered_v2: rendered,
    })
}

/// Render the in-memory v2 [`Settings`] to a TOML string with the
/// `schema_version = 2` header at the top. Used by both the
/// migrator and the CLI's `--dry-run` mode. Goes through the same
/// `SettingsFile` envelope that
/// [`Settings::save`] uses so the produced text round-trips
/// through [`Settings::load`].
pub fn render_settings_v2(settings: &Settings) -> Result<String, ConfigError> {
    let envelope = crate::settings::SettingsFile {
        schema_version: crate::settings::CURRENT_SCHEMA_VERSION,
        settings: settings.clone(),
    };
    Ok(toml::to_string_pretty(&envelope)?)
}

fn peek_settings_schema(text: &str) -> Result<Option<u32>, ConfigError> {
    crate::settings::peek_schema_version_public(text)
}

fn peek_connections_schema(text: &str) -> Result<Option<u32>, ConfigError> {
    peek_settings_schema(text)
}

fn backup_and_write(
    path: &Path,
    original_text: &str,
    new_text: &str,
    opts: &MigrateOptions,
) -> Result<Option<PathBuf>, ConfigError> {
    if opts.dry_run {
        return Ok(None);
    }
    let mut backup = path.as_os_str().to_owned();
    backup.push(&opts.backup_suffix);
    let backup_path = PathBuf::from(backup);
    if backup_path.exists() && !opts.force {
        return Err(ConfigError::Validation(format!(
            "backup already exists at {} — pass --force to overwrite",
            backup_path.display()
        )));
    }
    // Write the backup atomically too: a crash between the open and
    // close of `std::fs::write` could leave a half-written `.v1.bak`
    // file that a second `migrate-config` run would refuse to
    // overwrite without --force, even though it carries no real
    // backup. `atomic_write` does a write-then-rename so the visible
    // backup is either the full v1 text or absent.
    crate::settings::atomic_write(&backup_path, original_text)?;
    crate::settings::atomic_write(path, new_text)?;
    Ok(Some(backup_path))
}

/// Read-only schema check used by `narwhal config validate`. Walks
/// both files, returns the structured outcome without writing
/// anything.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ValidateReport {
    pub settings: ValidateOutcome,
    pub connections: ValidateOutcome,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ValidateOutcome {
    Absent,
    Ok { schema_version: u32 },
    NeedsMigration,
    Invalid(String),
    UnsupportedSchema(u32),
}

pub fn validate(settings_path: &Path, connections_path: &Path) -> ValidateReport {
    ValidateReport {
        settings: validate_one(settings_path, |t| {
            // For settings, also try the v2 parse so syntax errors
            // are reported even when schema_version is present.
            match peek_settings_schema(t)? {
                Some(2) => match Settings::load(settings_path) {
                    Ok(_) => Ok(ValidateOutcome::Ok { schema_version: 2 }),
                    Err(ConfigError::Toml(e)) => Ok(ValidateOutcome::Invalid(e.to_string())),
                    Err(e) => Ok(ValidateOutcome::Invalid(e.to_string())),
                },
                Some(1) | None => Ok(ValidateOutcome::NeedsMigration),
                Some(n) => Ok(ValidateOutcome::UnsupportedSchema(n)),
            }
        }),
        connections: validate_one(connections_path, |t| match peek_connections_schema(t)? {
            Some(2) => match ConnectionsFile::load(connections_path) {
                Ok(_) => Ok(ValidateOutcome::Ok { schema_version: 2 }),
                Err(ConfigError::Toml(e)) => Ok(ValidateOutcome::Invalid(e.to_string())),
                Err(ConfigError::Validation(e)) => Ok(ValidateOutcome::Invalid(e)),
                Err(e) => Ok(ValidateOutcome::Invalid(e.to_string())),
            },
            Some(1) | None => Ok(ValidateOutcome::NeedsMigration),
            Some(n) => Ok(ValidateOutcome::UnsupportedSchema(n)),
        }),
    }
}

fn validate_one(
    path: &Path,
    check: impl FnOnce(&str) -> Result<ValidateOutcome, ConfigError>,
) -> ValidateOutcome {
    if !path.exists() {
        return ValidateOutcome::Absent;
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => return ValidateOutcome::Invalid(format!("io: {e}")),
    };
    check(&text).unwrap_or_else(|e| ValidateOutcome::Invalid(e.to_string()))
}
