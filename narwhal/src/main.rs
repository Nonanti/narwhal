#![forbid(unsafe_code)]

use std::io::Write as _;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use narwhal_app::clipboard::{ArboardClipboard, Clipboard};
use narwhal_app::export::{ExportFormat, write_format};
use narwhal_app::{App, DriverRegistry as AppDriverRegistry};
use narwhal_config::{
    ConfigPaths, ConnectionsFile, CredentialStore, KeyringStore, MigrateOptions, MigrateOutcome,
    Settings, ValidateOutcome, VaultRegistry, migrate_config, validate_config,
};
use narwhal_core::{ConnectionConfig, DynConnection};
use narwhal_history::Journal;
use narwhal_mcp::{DriverRegistry as McpDriverRegistry, McpServer, ServerContext, Workspace};
use secrecy::ExposeSecret;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// narwhal: TUI database client with built-in MCP server + headless CLI.
#[derive(Debug, Parser)]
#[command(
    name = "narwhal",
    version,
    about = "TUI database client — with MCP server and headless `exec` mode",
    long_about = None,
    propagate_version = true,
    // No args = launch the TUI (the historical behaviour). Subcommands
    // pick up alternative runtimes.
    arg_required_else_help = false,
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Mode>,
    /// Refuse every row-level DML and DDL statement — useful when
    /// pointing the TUI at a production database for read-only
    /// auditing. Mirrors `psql --readonly`: the editor still accepts
    /// any SQL, but the row CRUD pipeline (o/O/d/cell edit) is
    /// disabled and a banner explains why. Also gates the `:write`
    /// path of `exec`. Off by default.
    #[arg(long = "read-only", global = true)]
    read_only: bool,
}

#[derive(Debug, Subcommand)]
enum Mode {
    /// Run as a Model Context Protocol server on stdio.
    Mcp,
    /// Execute one SQL statement and print the result. Pipes-friendly.
    Exec(ExecArgs),
    /// Migrate v1 settings.toml + connections.toml to the v2 schema.
    ///
    /// Writes the v2 file in place and preserves the original at
    /// `<file>.v1.bak`. Idempotent: running twice on a v2 file is
    /// a no-op.
    MigrateConfig(MigrateConfigArgs),
    /// Validate the on-disk config without modifying it. Reports
    /// schema version, parse errors, and whether migration is
    /// required. Exit code is non-zero on any non-OK outcome.
    Config(ConfigArgs),
}

#[derive(Debug, clap::Args)]
struct MigrateConfigArgs {
    /// Print the v2 payload that would be written, but don't touch
    /// the filesystem.
    #[arg(long = "dry-run")]
    dry_run: bool,
    /// Backup suffix appended to each v1 file. Defaults to
    /// `.v1.bak`.
    #[arg(long = "backup-suffix", default_value = ".v1.bak")]
    backup_suffix: String,
    /// Overwrite an existing backup. Refuses otherwise so an earlier
    /// migration's backup is never lost.
    #[arg(long = "force")]
    force: bool,
    /// Override the auto-discovered settings.toml path.
    #[arg(long = "settings-path", value_name = "PATH")]
    settings_path: Option<std::path::PathBuf>,
    /// Override the auto-discovered connections.toml path.
    #[arg(long = "connections-path", value_name = "PATH")]
    connections_path: Option<std::path::PathBuf>,
}

#[derive(Debug, clap::Args)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Read-only schema check. Reports `Ok` / `NeedsMigration` /
    /// `Invalid` / `UnsupportedSchema` for each config file.
    Validate {
        #[arg(long = "settings-path", value_name = "PATH")]
        settings_path: Option<std::path::PathBuf>,
        #[arg(long = "connections-path", value_name = "PATH")]
        connections_path: Option<std::path::PathBuf>,
    },
}

#[derive(Debug, clap::Args)]
struct ExecArgs {
    /// Connection name from `~/.config/narwhal/connections.toml`.
    #[arg(short = 'c', long = "conn", value_name = "NAME")]
    connection: String,
    /// SQL statement to execute. Quote it so the shell does not split.
    sql: String,
    /// Output format. `table` is human-friendly; the others are
    /// machine-friendly.
    #[arg(
        short = 'f',
        long = "format",
        value_name = "FORMAT",
        default_value = "table"
    )]
    format: String,
    /// Cap the number of returned rows. Defaults to "all".
    #[arg(short = 'l', long = "limit", value_name = "N")]
    limit: Option<usize>,
    /// Disable the default `BEGIN ... ROLLBACK` sandwich. Required for
    /// writes; without it any mutation runs and is rolled back.
    #[arg(long = "write")]
    write: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = ConfigPaths::discover().context("resolving user directories")?;
    paths
        .ensure()
        .context("creating configuration directories")?;

    match cli.command {
        Some(Mode::Mcp) => run_mcp(paths, cli.read_only).await,
        Some(Mode::Exec(args)) => run_exec(paths, args, cli.read_only).await,
        Some(Mode::MigrateConfig(args)) => run_migrate_config(paths, args),
        Some(Mode::Config(args)) => run_config(paths, args),
        None => run_tui(paths, cli.read_only).await,
    }
}

/// Centralised settings loader.
///
/// Maps `ConfigError::NeedsMigration` to a friendly warning that
/// names the binary's `migrate-config` subcommand, so the user is
/// never confronted with a raw "file is v1" error.
fn load_settings_or_warn(paths: &ConfigPaths) -> Settings {
    match Settings::load(&paths.settings_file()) {
        Ok(s) => s,
        Err(narwhal_config::ConfigError::NeedsMigration { path }) => {
            tracing::warn!(
                path = %path.display(),
                "settings.toml is v1 — run `narwhal migrate-config` to convert; using defaults until then"
            );
            Settings::default()
        }
        Err(error) => {
            tracing::warn!(
                path = %paths.settings_file().display(),
                error = %error,
                "falling back to default settings"
            );
            Settings::default()
        }
    }
}

fn load_connections_or_warn(paths: &ConfigPaths) -> ConnectionsFile {
    match ConnectionsFile::load(&paths.connections_file()) {
        Ok(c) => c,
        Err(narwhal_config::ConfigError::NeedsMigration { path }) => {
            tracing::warn!(
                path = %path.display(),
                "connections.toml is v1 — run `narwhal migrate-config` to convert; using empty list until then"
            );
            ConnectionsFile::default()
        }
        Err(error) => {
            tracing::warn!(
                path = %paths.connections_file().display(),
                error = %error,
                "falling back to empty connections file"
            );
            ConnectionsFile::default()
        }
    }
}

fn run_migrate_config(paths: ConfigPaths, args: MigrateConfigArgs) -> Result<()> {
    let settings_path = args.settings_path.unwrap_or_else(|| paths.settings_file());
    let connections_path = args
        .connections_path
        .unwrap_or_else(|| paths.connections_file());
    let opts = MigrateOptions::with(|o| {
        o.dry_run = args.dry_run;
        o.backup_suffix = args.backup_suffix;
        o.force = args.force;
    });
    let report =
        migrate_config(&settings_path, &connections_path, &opts).context("migrate-config")?;
    print_migrate_outcome("settings", &settings_path, &report.settings, args.dry_run);
    print_migrate_outcome(
        "connections",
        &connections_path,
        &report.connections,
        args.dry_run,
    );
    Ok(())
}

fn print_migrate_outcome(
    label: &str,
    path: &std::path::Path,
    outcome: &MigrateOutcome,
    dry_run: bool,
) {
    match outcome {
        MigrateOutcome::Absent => {
            println!("{label}: {} — absent, skipped", path.display());
        }
        MigrateOutcome::AlreadyV2 => {
            println!("{label}: {} — already v2, no action", path.display());
        }
        MigrateOutcome::Migrated {
            backup_path,
            rendered_v2,
        } => {
            if dry_run {
                println!("{label}: {} — dry-run, would write:", path.display());
                println!("---");
                println!("{rendered_v2}");
                println!("---");
            } else {
                let backup = backup_path
                    .as_ref()
                    .map_or_else(|| "(none)".to_owned(), |p| p.display().to_string());
                println!(
                    "{label}: {} — migrated to v2 (backup: {backup})",
                    path.display()
                );
            }
        }
        // `MigrateOutcome` is `#[non_exhaustive]`. A new variant
        // added in a future minor means the binary was built
        // against an older `narwhal-config` than the one wired
        // here — highly unusual but possible during partial
        // upgrades. Surface it loudly rather than swallowing.
        other => {
            debug_assert!(false, "unhandled MigrateOutcome variant: {other:?}");
            tracing::error!(?other, "unhandled MigrateOutcome variant in CLI dispatch");
            println!(
                "{label}: {} — unknown outcome (open an issue: {other:?})",
                path.display()
            );
        }
    }
}

fn run_config(paths: ConfigPaths, args: ConfigArgs) -> Result<()> {
    match args.command {
        ConfigCommand::Validate {
            settings_path,
            connections_path,
        } => {
            let settings_path = settings_path.unwrap_or_else(|| paths.settings_file());
            let connections_path = connections_path.unwrap_or_else(|| paths.connections_file());
            let report = validate_config(&settings_path, &connections_path);
            let mut ok = true;
            print_validate_outcome("settings", &settings_path, &report.settings, &mut ok);
            print_validate_outcome(
                "connections",
                &connections_path,
                &report.connections,
                &mut ok,
            );
            if !ok {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

fn print_validate_outcome(
    label: &str,
    path: &std::path::Path,
    outcome: &ValidateOutcome,
    ok: &mut bool,
) {
    match outcome {
        ValidateOutcome::Absent => {
            println!("{label}: {} — absent", path.display());
        }
        ValidateOutcome::Ok { schema_version } => {
            println!("{label}: {} — ok (v{schema_version})", path.display());
        }
        ValidateOutcome::NeedsMigration => {
            *ok = false;
            println!(
                "{label}: {} — v1, run `narwhal migrate-config` to convert",
                path.display()
            );
        }
        ValidateOutcome::Invalid(msg) => {
            *ok = false;
            println!("{label}: {} — invalid: {msg}", path.display());
        }
        ValidateOutcome::UnsupportedSchema(n) => {
            *ok = false;
            println!(
                "{label}: {} — unsupported schema_version = {n}; upgrade narwhal",
                path.display()
            );
        }
        // `ValidateOutcome` is `#[non_exhaustive]`; see the same
        // note on `MigrateOutcome` above.
        other => {
            *ok = false;
            debug_assert!(false, "unhandled ValidateOutcome variant: {other:?}");
            tracing::error!(?other, "unhandled ValidateOutcome variant in CLI dispatch");
            println!(
                "{label}: {} — unknown outcome (open an issue: {other:?})",
                path.display()
            );
        }
    }
}

/// TUI mode (default): logs go to a daily-rotating file because the
/// terminal is owned by the UI in raw mode.
async fn run_tui(paths: ConfigPaths, read_only: bool) -> Result<()> {
    let file_appender = tracing_appender::rolling::daily(paths.log_dir(), "narwhal.log");
    let (writer, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(fmt::layer().with_writer(writer).with_ansi(false))
        .init();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting narwhal");

    // L20: log instead of silently swallowing a malformed settings file
    // — a user-visible warning beats falling back to defaults blind.
    let settings = load_settings_or_warn(&paths);
    let connections = load_connections_or_warn(&paths);
    let history = match Journal::open(paths.history_file()).await {
        Ok(j) => Some(Arc::new(j)),
        Err(error) => {
            tracing::warn!(error = %error, "history journal disabled");
            None
        }
    };

    let registry = AppDriverRegistry::with_defaults();
    let credentials: Arc<dyn CredentialStore> = Arc::new(KeyringStore::new());
    let clipboard: Arc<dyn Clipboard> = Arc::new(ArboardClipboard::new());
    // T1-T2-B: build the vault registry from settings.vault.providers.
    // A misconfigured provider sub-section here is non-fatal — the
    // TUI still starts so the user can edit settings.toml; only an
    // actual `password = "vault:…"` reference will fail at connect
    // time with a clear NotConfigured error.
    let vault = Arc::new(build_vault_registry(&settings.vault, "TUI"));
    // T1-T3-B: workspace-state restore. Wired before `with_settings`
    // so the persist toggles from `[settings.workspace.persist]` are
    // already cached when `with_workspace_state_path` consults them.
    let workspace_state_path = paths.workspace_state_file();
    let app = App::with_services(registry, connections, history, credentials, clipboard)
        .with_vault(vault)
        .with_connections_path(paths.connections_file())
        .with_last_used_path(paths.last_used_file())
        .with_settings(settings)
        .with_workspace_state_path(workspace_state_path)
        .with_plugins_dir(&paths.plugins_dir())
        .with_read_only(read_only);

    if let Err(error) = app.run().await {
        tracing::error!(error = %error, "fatal error");
        eprintln!("narwhal: fatal: {error:#}");
        // L40: drop the non-blocking appender guard *before* exiting so
        // the final tracing::error reliably reaches disk. `process::exit`
        // skips destructors of in-scope bindings, including `_guard`.
        drop(_guard);
        std::process::exit(1);
    }

    Ok(())
}

/// MCP mode: stdout is the JSON-RPC transport, so logs MUST go to stderr.
/// We use a synchronous appender here because the JSON-RPC reader is what
/// drives runtime activity — there's no UI competing for the terminal.
///
/// `force_read_only` reflects the global `--read-only` CLI flag. When
/// true, the server refuses any `read_only=false` tool call regardless
/// of the workspace ACL — the flag is the most user-visible safety
/// promise and MUST apply to the highest-risk surface (LLM authorship).
async fn run_mcp(paths: ConfigPaths, force_read_only: bool) -> Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(false)
                .with_target(false),
        )
        .init();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting narwhal MCP server"
    );

    let connections = load_connections_or_warn(&paths);
    let settings = load_settings_or_warn(&paths);

    let drivers = Arc::new(McpDriverRegistry::with_defaults());
    let credentials: Arc<dyn CredentialStore> = Arc::new(KeyringStore::new());
    let vault = Arc::new(build_vault_registry(&settings.vault, "MCP"));
    let journal = match Journal::open(paths.history_file()).await {
        Ok(j) => Some(Arc::new(j)),
        Err(error) => {
            // Audit logging is best-effort; carry on without it so an
            // unwriteable disk does not prevent the agent from talking
            // to a working database.
            tracing::warn!(error = %error, "MCP audit journal disabled");
            None
        }
    };
    let mut ctx = ServerContext::new(drivers, Arc::new(connections), credentials)
        .with_vault(vault)
        .with_force_read_only(force_read_only);
    if force_read_only {
        tracing::info!("MCP server forced to read-only by --read-only flag");
    }
    if let Some(journal) = journal {
        ctx = ctx.with_journal(journal);
    }

    // Workspace discovery: walk up from `pwd` looking for
    // `.narwhal/workspace.toml`. Found = scoped MCP; not found = legacy
    // behaviour (expose every connection, allow writes).
    let cwd = std::env::current_dir().context("resolving current directory")?;
    match Workspace::discover(&cwd) {
        Ok(Some(ws)) => {
            tracing::info!(
                root = %ws.root.display(),
                allowed_connections = ws.file.allowed_connections.len(),
                allow_writes = ws.file.allow_writes,
                "workspace attached"
            );
            ctx = ctx.with_workspace(Arc::new(ws));
        }
        Ok(None) => {
            tracing::info!("no workspace file found; exposing every connection");
        }
        Err(error) => {
            // Refuse to start with a broken workspace file — silent
            // fallback would expose more than the user intended.
            return Err(anyhow::anyhow!("workspace discovery: {error}"));
        }
    }

    McpServer::new(ctx)
        .serve_stdio()
        .await
        .context("MCP stdio loop terminated with IO error")?;

    Ok(())
}

/// Headless `exec` mode: run one statement, dump the result, exit.
///
/// Logs go to stderr at the `warn` level by default so a piped stdout
/// stays clean (`narwhal exec ... | wc -l` does the right thing). Set
/// `RUST_LOG=info,narwhal=debug` to see the dialled connection + audit
/// entry.
async fn run_exec(paths: ConfigPaths, args: ExecArgs, global_read_only: bool) -> Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(false)
                .with_target(false),
        )
        .init();

    let format = ExportFormat::from_token(&args.format).with_context(|| {
        format!(
            "unknown format `{}` — choose one of: csv, json, tsv, table, markdown",
            args.format
        )
    })?;
    if matches!(format, ExportFormat::Insert) {
        // `insert` needs a source table that the CLI cannot know about
        // — refuse here so the failure surfaces as a friendly error
        // instead of a deep ExportError::NoSourceTable later.
        anyhow::bail!("`insert` is not supported in exec mode — use the TUI's `:export` command");
    }
    // T1-T4-B: Parquet needs to own the sink for atomic write +
    // footer flush, which the streaming `write_format` path cannot
    // provide. Direct the user at the TUI command (which goes through
    // `export_rows` with a real path).
    if matches!(format, ExportFormat::Parquet) {
        anyhow::bail!(
            "`parquet` is not supported in exec mode (binary footer needs file ownership) — use the TUI's `:export parquet <path>` command"
        );
    }

    let connections_file = match ConnectionsFile::load(&paths.connections_file()) {
        Ok(c) => c,
        Err(narwhal_config::ConfigError::NeedsMigration { path }) => {
            anyhow::bail!(
                "connections file at {} is v1 — run `narwhal migrate-config` first",
                path.display()
            )
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "loading connections file: {}",
                    paths.connections_file().display()
                )
            });
        }
    };
    let config = connections_file
        .connections
        .iter()
        .find(|c| c.name == args.connection)
        .cloned()
        .with_context(|| {
            format!(
                "unknown connection `{}` (defined in {})",
                args.connection,
                paths.connections_file().display()
            )
        })?;

    let registry = McpDriverRegistry::with_defaults();
    let credentials: Arc<dyn CredentialStore> = Arc::new(KeyringStore::new());
    // T1-T2-B: load settings just for the vault block. The exec
    // path doesn't care about the rest of the settings struct —
    // it has its own CLI flags for everything else.
    let settings = load_settings_or_warn(&paths);
    let vault = build_vault_registry(&settings.vault, "exec");

    let password = resolve_password(&*credentials, &vault, &config).await;
    let driver = registry
        .get(&config.driver)
        .map_err(|e| anyhow::anyhow!("driver: {e}"))?;

    // L36 #7 + #C3 + #C4: run the pre-connect pipeline before the
    // driver dials in. Skipped entirely under `--read-only` so an
    // auditor can't be tricked into shell exec; the password channel
    // is also passed through substitute_password so a vault step's
    // output can land in the keyring placeholder.
    let mut config = config;
    let mut password = password;
    if global_read_only {
        if !config.params.pre_connect.is_empty() {
            tracing::warn!(
                steps = config.params.pre_connect.len(),
                "exec: skipping pre-connect under --read-only"
            );
        }
    } else {
        let pc_vars = narwhal_commands::pre_connect::run_pre_connect(&config.params.pre_connect)
            .await
            .context("running pre-connect steps")?;
        if !pc_vars.is_empty() {
            narwhal_commands::pre_connect::substitute_pre_connect(&mut config.params, &pc_vars)
                .context("applying pre-connect substitution")?;
            password = narwhal_commands::pre_connect::substitute_password(password, &pc_vars)
                .context("applying pre-connect password substitution")?;
        }
    }

    let mut conn: Box<dyn DynConnection> = driver
        .connect(&config, password.as_deref())
        .await
        .context("opening connection")?;

    // Best-effort audit log: piping the same `source` tag as the MCP
    // path lets users `jq 'select(.source == "exec")'` to isolate CLI
    // traffic. Failures are non-fatal (read-only filesystem, etc.).
    if let Ok(journal) = Journal::open(paths.history_file()).await {
        let entry = narwhal_history::HistoryEntry::success(&args.sql)
            .with_connection(config.id, &config.name)
            .with_driver(&config.driver)
            .with_source("exec");
        let _ = journal.append(&entry).await;
    }

    // Sandbox writes by default; `--write` opts out. The MCP server uses
    // the same pattern so behaviour stays predictable across runtimes.
    // L36 #11: the global `--read-only` flag forces a sandbox even when
    // the per-command `--write` opt-out is set.
    if global_read_only && args.write {
        anyhow::bail!(
            "--read-only forbids --write: drop one of them or relaunch without --read-only"
        );
    }
    let read_only = !args.write || global_read_only;
    let exec_result = if read_only {
        match conn.begin().await {
            Ok(()) => {
                let r = conn.execute(&args.sql, &[]).await;
                let _ = conn.rollback().await;
                r
            }
            Err(_) => conn.execute(&args.sql, &[]).await,
        }
    } else {
        conn.execute(&args.sql, &[]).await
    };
    let _ = conn.close().await;
    let mut query = exec_result.context("executing statement")?;

    if let Some(limit) = args.limit {
        if query.rows.len() > limit {
            query.rows.truncate(limit);
        }
    }

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    write_format(&mut handle, format, &query.columns, &query.rows)
        .context("writing result to stdout")?;
    handle.flush().context("flushing stdout")?;

    Ok(())
}

/// Credential resolution chain shared with the MCP path: T1-T2-B
/// vault reference → keyring → `~/.pgpass` / env-var fallback.
/// Failures are not fatal — drivers that accept passwordless auth
/// simply receive `None`. Vault failures are logged at warn level so
/// the operator notices a misconfigured `vault:` reference without
/// the connect path silently degrading to no-password.
async fn resolve_password(
    credentials: &dyn CredentialStore,
    vault: &VaultRegistry,
    config: &ConnectionConfig,
) -> Option<String> {
    match narwhal_config::resolve_connection_password(config, Some(vault), Some(credentials)).await
    {
        Ok(Some(secret)) => Some(secret.expose_secret().to_string()),
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                connection = %config.name,
                %error,
                "password resolution failed; connect will proceed without a password",
            );
            None
        }
    }
}

/// Build a [`VaultRegistry`] from `settings.vault.providers`,
/// logging (but not failing) on a misconfigured provider section.
///
/// `entry_point` tags log lines so the operator can tell whether the
/// TUI, MCP server, or exec path produced the warning.
fn build_vault_registry(
    settings: &narwhal_config::settings::VaultSettings,
    entry_point: &'static str,
) -> VaultRegistry {
    match VaultRegistry::from_settings(settings) {
        Ok(registry) => {
            let providers: Vec<&str> = registry.provider_names();
            if providers.is_empty() {
                tracing::debug!(entry = entry_point, "no vault providers configured");
            } else {
                tracing::info!(
                    entry = entry_point,
                    providers = ?providers,
                    "vault providers ready"
                );
            }
            registry
        }
        Err(error) => {
            tracing::warn!(
                entry = entry_point,
                %error,
                "vault providers misconfigured; references will fail at connect time"
            );
            VaultRegistry::empty()
        }
    }
}
