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
    /// Inspect the audit log.
    ///
    /// T2-T2-D: read-only viewer for the JSONL audit sink. Resolves
    /// the active sink path from `settings.audit.sinks` (the first
    /// `file:` entry) unless `--path` overrides it.
    Audit(AuditArgs),
    /// Diff two connections and emit DDL that migrates target onto
    /// source.
    ///
    /// T2-T2-C: headless variant of the `:schema-diff` TUI command.
    /// Opens both connections, introspects their schemas, computes
    /// a structural diff, and renders DDL through the chosen
    /// dialect emitter.
    SchemaDiff(SchemaDiffArgs),
}

#[derive(Debug, clap::Args)]
struct AuditArgs {
    #[command(subcommand)]
    command: AuditCommand,
}

#[derive(Debug, Subcommand)]
enum AuditCommand {
    /// Print the tail of the audit log, optionally following new
    /// lines as they are appended.
    Tail {
        /// Override the audit file path. When absent, the first
        /// `file:` sink from `settings.audit.sinks` is used.
        #[arg(long = "path", value_name = "PATH")]
        path: Option<std::path::PathBuf>,
        /// Print this many lines before tailing (defaults to all when
        /// `--follow` is not set).
        #[arg(short = 'n', long = "lines", value_name = "N")]
        lines: Option<usize>,
        /// Keep the file open and stream new lines as they appear.
        #[arg(short = 'f', long = "follow")]
        follow: bool,
        /// Filter to a single event kind: `query`, `connection_opened`,
        /// `connection_closed`, `configuration`, `plugin_loaded`.
        #[arg(long = "kind", value_name = "KIND")]
        kind: Option<String>,
    },
}

#[derive(Debug, clap::Args)]
struct SchemaDiffArgs {
    /// Source connection (desired state). Looked up in
    /// `connections.toml` by name.
    source: String,
    /// Target connection (the one that will be migrated). Looked up
    /// by name. The emitted DDL transforms this side into the source.
    target: String,
    /// Dialect to emit. Defaults to the source connection's driver
    /// (`postgres`, `mysql`, `sqlite`, `mssql`). `generic` is the
    /// ANSI fallback.
    #[arg(long = "dialect", value_name = "NAME")]
    dialect: Option<String>,
    /// Write the DDL to this file instead of stdout.
    #[arg(short = 'o', long = "out", value_name = "PATH")]
    out: Option<std::path::PathBuf>,
    /// Restrict the diff to one schema (matches `schema` on both
    /// sides after `--schema-map` resolution).
    #[arg(long = "schema", value_name = "NAME")]
    schema: Option<String>,
    /// Restrict the diff to one table (matches the table name on
    /// both sides; combine with `--schema` to disambiguate).
    #[arg(long = "table", value_name = "NAME")]
    table: Option<String>,
    /// Map a source schema to a different target schema. Repeatable.
    /// Format: `--schema-map source=target`. Useful when prod and
    /// staging use different namespaces (`prod.public` vs
    /// `staging.public2`).
    #[arg(long = "schema-map", value_name = "MAP")]
    schema_map: Vec<String>,
    /// Exit with a non-zero status when the diff is non-empty.
    /// Pairs naturally with CI gating.
    #[arg(long = "fail-on-drift")]
    fail_on_drift: bool,
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
        Some(Mode::Audit(args)) => run_audit(paths, args).await,
        Some(Mode::SchemaDiff(args)) => run_schema_diff(paths, args, cli.read_only).await,
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
    // T2-T2-D: build the audit service if at least one sink is
    // configured. Failure to open any individual sink is non-fatal
    // — the TUI still starts; missing sinks are logged. When the
    // resulting service has zero live sinks, audit stays silent.
    let audit = build_audit_service(&settings.audit).await;
    // T1-T3-B: workspace-state restore. Wired before `with_settings`
    // so the persist toggles from `[settings.workspace.persist]` are
    // already cached when `with_workspace_state_path` consults them.
    let workspace_state_path = paths.workspace_state_file();
    let mut app = App::with_services(registry, connections, history, credentials, clipboard)
        .with_vault(vault)
        .with_connections_path(paths.connections_file())
        .with_last_used_path(paths.last_used_file())
        .with_settings(settings)
        .with_workspace_state_path(workspace_state_path);
    // T2-T2-D: install the audit service *before* auto-loading
    // plugins so the `PluginLoaded` event for each startup plugin
    // makes it into the audit log alongside `:plug-load` events.
    if let Some(svc) = audit {
        app = app.with_audit_service(svc);
    }
    app = app
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

/// T2-T2-D: build an [`narwhal_audit::AuditService`] from the
/// `[settings.audit]` block.
///
/// Returns `None` when audit is disabled, no sinks are configured, or
/// every configured sink failed to open. The TUI still starts in any
/// of those cases — audit is opt-in compliance machinery, not a
/// load-bearing feature.
async fn build_audit_service(
    cfg: &narwhal_audit::AuditConfig,
) -> Option<Arc<narwhal_audit::AuditService>> {
    if !cfg.enabled || cfg.sinks.is_empty() {
        return None;
    }
    let mut builder = narwhal_audit::AuditService::builder().block_on_full(cfg.block_on_full);
    builder = builder.with_redactor(narwhal_audit::Redactor::new(
        narwhal_audit::RedactorConfig {
            redact_passwords: cfg.redact_passwords,
            redact_columns: cfg.redact_columns.clone(),
        },
    ));
    for spec in &cfg.sinks {
        match spec {
            narwhal_audit::SinkSpec::File(path) => {
                match narwhal_audit::sinks::FileSink::open(
                    narwhal_audit::sinks::file::FileSinkConfig::new(path),
                )
                .await
                {
                    Ok(sink) => builder = builder.with_sink(Arc::new(sink)),
                    Err(error) => {
                        tracing::warn!(%error, path = %path, "audit file sink open failed; skipping");
                    }
                }
            }
            narwhal_audit::SinkSpec::Stdout => {
                builder = builder.with_sink(Arc::new(narwhal_audit::sinks::StdoutSink::new()));
            }
            narwhal_audit::SinkSpec::Syslog => {
                #[cfg(feature = "audit-syslog")]
                {
                    match narwhal_audit::sinks::syslog::SyslogSink::open() {
                        Ok(sink) => builder = builder.with_sink(Arc::new(sink)),
                        Err(error) => {
                            tracing::warn!(%error, "audit syslog sink open failed; skipping");
                        }
                    }
                }
                #[cfg(not(feature = "audit-syslog"))]
                {
                    tracing::warn!(
                        "audit sink 'syslog' configured but binary was built without the \
                         `audit-syslog` feature; skipping"
                    );
                }
            }
        }
    }
    let svc = builder.start()?;
    tracing::info!(
        sinks = svc.sink_count(),
        block_on_full = cfg.block_on_full,
        "audit log enabled"
    );
    Some(Arc::new(svc))
}

/// T2-T2-D: `narwhal audit tail` implementation.
///
/// Resolves the audit file path (CLI override > first `file:` sink
/// from `settings.audit.sinks`), prints the requested tail, and
/// optionally follows. Designed as a read-only inspector — never
/// rotates, truncates, or mutates the file.
async fn run_audit(paths: ConfigPaths, args: AuditArgs) -> Result<()> {
    // Stderr-only logging so the JSONL output stays clean for pipes.
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .with(fmt::layer().with_writer(std::io::stderr).with_ansi(false))
        .init();

    match args.command {
        AuditCommand::Tail {
            path,
            lines,
            follow,
            kind,
        } => audit_tail(&paths, path, lines, follow, kind).await,
    }
}

/// Resolve, open, optionally seek-back-N-lines, and stream a JSONL
/// audit file. Filters are applied per-line so a stale `--kind`
/// argument never accidentally drops a real entry from disk.
async fn audit_tail(
    paths: &ConfigPaths,
    path_override: Option<std::path::PathBuf>,
    lines: Option<usize>,
    follow: bool,
    kind: Option<String>,
) -> Result<()> {
    use tokio::io::{AsyncBufReadExt as _, AsyncSeekExt as _, BufReader};

    let path = resolve_audit_path(paths, path_override)?;

    let mut file = tokio::fs::File::open(&path)
        .await
        .with_context(|| format!("opening audit file {}", path.display()))?;

    // Default behaviour: when --follow is unset, dump everything.
    // When --follow is set without --lines, start from EOF (the
    // standard `tail -f` shape). `--lines N --follow` does both:
    // print the last N then continue streaming.
    let lines_to_print = match (lines, follow) {
        (Some(n), _) => Some(n),
        (None, true) => Some(0),
        (None, false) => None,
    };

    if let Some(n) = lines_to_print {
        let metadata = file.metadata().await?;
        let size = metadata.len();
        let start = seek_back_n_lines(&mut file, size, n).await?;
        file.seek(std::io::SeekFrom::Start(start)).await?;
    }

    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let mut stdout = tokio::io::stdout();
    use tokio::io::AsyncWriteExt as _;
    loop {
        line.clear();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            if !follow {
                break;
            }
            // Sleep briefly to avoid busy-looping. The audit emit rate
            // is dominated by query dispatch, not by this tail loop.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            continue;
        }
        if let Some(filter) = kind.as_deref() {
            if !line_matches_kind(&line, filter) {
                continue;
            }
        }
        stdout.write_all(line.as_bytes()).await?;
    }
    stdout.flush().await?;
    Ok(())
}

/// Pick the audit file the operator is most likely tailing.
///
/// Preference order: explicit `--path`, then the first `file:` entry
/// in `settings.audit.sinks`. The strftime tokens in the configured
/// path are expanded against UTC `now` — the same logic the runtime
/// uses — so a daily-rotating template resolves to today's file.
fn resolve_audit_path(
    paths: &ConfigPaths,
    explicit: Option<std::path::PathBuf>,
) -> Result<std::path::PathBuf> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    let settings = load_settings_or_warn(paths);
    let template = settings
        .audit
        .sinks
        .iter()
        .find_map(|s| match s {
            narwhal_audit::SinkSpec::File(p) => Some(p.clone()),
            _ => None,
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no `file:` sink configured in settings.audit.sinks; pass --path to override"
            )
        })?;
    let resolved = chrono::Utc::now().format(&template).to_string();
    Ok(std::path::PathBuf::from(resolved))
}

/// Seek back through `file` (size: `size` bytes) until we have crossed
/// `n` newlines, then return that offset. `n == 0` yields the EOF
/// offset (the classic `tail -f` starting point).
async fn seek_back_n_lines(file: &mut tokio::fs::File, size: u64, n: usize) -> Result<u64> {
    use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

    if n == 0 {
        return Ok(size);
    }
    // Walk backwards in 4 KiB chunks counting newlines. Switches to
    // returning the start of the file once we exhaust it without
    // hitting the target count — the operator sees the entire log,
    // which is the only sensible fallback for a short file.
    const CHUNK: u64 = 4096;
    let mut pos = size;
    let mut buf = vec![0u8; CHUNK as usize];
    let mut newlines = 0usize;
    while pos > 0 {
        let read_size = pos.min(CHUNK);
        pos -= read_size;
        file.seek(std::io::SeekFrom::Start(pos)).await?;
        let slice = &mut buf[..read_size as usize];
        file.read_exact(slice).await?;
        for (i, b) in slice.iter().enumerate().rev() {
            if *b == b'\n' {
                newlines += 1;
                if newlines > n {
                    // Skip the newline itself so the next read starts
                    // at the first byte of the kept line.
                    return Ok(pos + i as u64 + 1);
                }
            }
        }
    }
    Ok(0)
}

/// Return true when `line` (a serialised audit JSON object) has the
/// `kind` field equal to `expected`. Matches case-insensitively to
/// match the user's lowercase wire convention regardless of how they
/// typed the flag.
fn line_matches_kind(line: &str, expected: &str) -> bool {
    // Cheap text scan beats `serde_json::from_str` on a hot path that
    // may chew through gigabytes of history; the audit wire format
    // pins the discriminant key as `"kind":"<lower_snake>"`.
    let needle = format!("\"kind\":\"{}\"", expected.to_ascii_lowercase());
    line.to_ascii_lowercase().contains(&needle)
}

/// T2-T2-C: headless `schema-diff` runner.
///
/// Opens two connections (source + target) just long enough to walk
/// their full schema catalogues, computes a structural diff via
/// `narwhal_schema_diff::diff`, then renders DDL through the chosen
/// dialect emitter. The result is written to `--out` when provided,
/// otherwise dumped to stdout so the caller can pipe it onwards.
async fn run_schema_diff(
    paths: ConfigPaths,
    args: SchemaDiffArgs,
    global_read_only: bool,
) -> Result<()> {
    // Stderr-only logging keeps stdout clean for `| psql staging`
    // style piping.
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .with(fmt::layer().with_writer(std::io::stderr).with_ansi(false))
        .init();

    let connections_file = match ConnectionsFile::load(&paths.connections_file()) {
        Ok(c) => c,
        Err(error) => {
            anyhow::bail!("loading {}: {error}", paths.connections_file().display());
        }
    };

    let source_cfg = lookup_connection(&connections_file, &args.source)
        .with_context(|| format!("source connection `{}` not found", args.source))?;
    let target_cfg = lookup_connection(&connections_file, &args.target)
        .with_context(|| format!("target connection `{}` not found", args.target))?;

    let schema_map = parse_schema_map(&args.schema_map)?;
    let dialect_name = args
        .dialect
        .clone()
        .unwrap_or_else(|| source_cfg.driver.clone());
    let emitter = narwhal_schema_diff::emit::emitter_by_name(&dialect_name).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown dialect `{dialect_name}` — \
            recognised: postgres, mysql, sqlite, mssql, generic"
        )
    })?;

    let registry = McpDriverRegistry::with_defaults();
    let credentials: Arc<dyn CredentialStore> = Arc::new(KeyringStore::new());
    let settings = load_settings_or_warn(&paths);
    let vault = build_vault_registry(&settings.vault, "schema-diff");

    let source_tables = introspect_for_diff(
        &registry,
        &*credentials,
        &vault,
        source_cfg,
        global_read_only,
    )
    .await
    .with_context(|| format!("introspecting source `{}`", args.source))?;
    let target_tables = introspect_for_diff(
        &registry,
        &*credentials,
        &vault,
        target_cfg,
        global_read_only,
    )
    .await
    .with_context(|| format!("introspecting target `{}`", args.target))?;

    let source_filtered =
        apply_filters(source_tables, args.schema.as_deref(), args.table.as_deref());
    let target_filtered =
        apply_filters(target_tables, args.schema.as_deref(), args.table.as_deref());
    let target_mapped = apply_schema_map(target_filtered, &schema_map);

    let diff = narwhal_schema_diff::diff(&source_filtered, &target_mapped);
    let ddl = emitter
        .emit(&diff)
        .map_err(|e| anyhow::anyhow!("emit: {e}"))?;

    if let Some(path) = args.out.as_ref() {
        std::fs::write(path, &ddl).with_context(|| format!("writing {}", path.display()))?;
        tracing::info!(
            path = %path.display(),
            changes = diff.change_count(),
            tables = diff.tables.len(),
            "schema-diff written"
        );
    } else {
        print!("{ddl}");
    }

    if args.fail_on_drift && !diff.is_empty() {
        // Spell out the failure on stderr so a CI log shows what
        // tripped the gate without forcing the operator to count
        // bytes on stdout.
        eprintln!(
            "schema drift detected: {} table change(s), {} total deltas",
            diff.tables.len(),
            diff.change_count()
        );
        std::process::exit(2);
    }
    Ok(())
}

/// Connection lookup with a small forgiving touch: exact-name match
/// preferred, then case-insensitive fallback so the operator typing
/// `Prod` against an entry named `prod` gets a hit instead of an
/// "unknown connection" surprise.
fn lookup_connection(file: &ConnectionsFile, name: &str) -> Option<ConnectionConfig> {
    file.connections
        .iter()
        .find(|c| c.name == name)
        .or_else(|| {
            file.connections
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(name))
        })
        .cloned()
}

/// Parse the `--schema-map source=target` flag values into a map.
/// Repeats are allowed; the *last* mapping wins, matching how clap's
/// `Vec<String>` arrives at the function.
fn parse_schema_map(raw: &[String]) -> Result<std::collections::HashMap<String, String>> {
    let mut out = std::collections::HashMap::new();
    for entry in raw {
        let (src, tgt) = entry.split_once('=').ok_or_else(|| {
            anyhow::anyhow!("--schema-map expects `source=target`, got `{entry}`")
        })?;
        let src = src.trim();
        let tgt = tgt.trim();
        if src.is_empty() || tgt.is_empty() {
            anyhow::bail!("--schema-map: empty side in `{entry}`");
        }
        out.insert(src.to_owned(), tgt.to_owned());
    }
    Ok(out)
}

/// Open one connection, introspect every user table, close. Pre-
/// connect / SSH / vault / keyring resolution mirror the `exec`
/// subcommand so a connection that works for `narwhal exec` also
/// works for `narwhal schema-diff`.
async fn introspect_for_diff(
    registry: &McpDriverRegistry,
    credentials: &dyn CredentialStore,
    vault: &VaultRegistry,
    mut config: ConnectionConfig,
    global_read_only: bool,
) -> Result<Vec<narwhal_core::TableSchema>> {
    let mut password = resolve_password(credentials, vault, &config).await;

    if global_read_only && !config.params.pre_connect.is_empty() {
        tracing::warn!(
            steps = config.params.pre_connect.len(),
            "schema-diff: skipping pre-connect under --read-only"
        );
    } else if !config.params.pre_connect.is_empty() {
        let pc_vars = narwhal_commands::pre_connect::run_pre_connect(&config.params.pre_connect)
            .await
            .context("running pre-connect steps")?;
        narwhal_commands::pre_connect::substitute_pre_connect(&mut config.params, &pc_vars)
            .context("applying pre-connect substitution")?;
        password = narwhal_commands::pre_connect::substitute_password(password, &pc_vars)
            .context("applying pre-connect password substitution")?;
    }

    let driver = registry
        .get(&config.driver)
        .map_err(|e| anyhow::anyhow!("driver: {e}"))?;
    let mut conn = driver
        .connect(&config, password.as_deref())
        .await
        .context("opening connection")?;

    let catalog = conn.list_all_tables().await.context("listing tables")?;
    let mut tables = Vec::new();
    for (schema, table_list) in catalog {
        for t in table_list {
            // Views and system tables are filtered by the diff
            // crate's own system-schema filter; here we keep
            // user-visible tables. `MaterializedView` is left out
            // for v2.0 — its DDL emission is engine-specific and
            // out of scope per the brief.
            if !matches!(t.kind, narwhal_core::TableKind::Table) {
                continue;
            }
            match conn.describe_table(&schema.name, &t.name).await {
                Ok(ts) => tables.push(ts),
                Err(error) => {
                    // Don't abort the whole diff on one bad table —
                    // a permission glitch on `pg_catalog`-style
                    // hidden tables shouldn't bring down a perfectly
                    // valid migration plan. Surface on stderr so the
                    // operator notices.
                    tracing::warn!(
                        schema = %schema.name,
                        table = %t.name,
                        error = %error,
                        "describe_table failed; skipping"
                    );
                }
            }
        }
    }
    let _ = conn.close().await;
    Ok(tables)
}

/// `--schema` / `--table` filter pass. An entry survives when (a) no
/// schema filter is set or it matches, AND (b) no table filter is set
/// or it matches.
fn apply_filters(
    tables: Vec<narwhal_core::TableSchema>,
    schema: Option<&str>,
    table: Option<&str>,
) -> Vec<narwhal_core::TableSchema> {
    tables
        .into_iter()
        .filter(|t| schema.is_none_or(|s| t.table.schema == s))
        .filter(|t| table.is_none_or(|n| t.table.name == n))
        .collect()
}

/// Rewrite target-side schema names according to the `--schema-map`
/// table. Foreign-key `referenced_schema` is also rewritten so the
/// FK comparison stays sane after the move.
fn apply_schema_map(
    mut tables: Vec<narwhal_core::TableSchema>,
    map: &std::collections::HashMap<String, String>,
) -> Vec<narwhal_core::TableSchema> {
    if map.is_empty() {
        return tables;
    }
    for t in &mut tables {
        if let Some(new_schema) = map.get(&t.table.schema) {
            t.table.schema = new_schema.clone();
        }
        for fk in &mut t.foreign_keys {
            if let Some(ref_schema) = fk.referenced_schema.as_ref() {
                if let Some(new_schema) = map.get(ref_schema) {
                    fk.referenced_schema = Some(new_schema.clone());
                }
            }
        }
    }
    tables
}
