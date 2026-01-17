#![forbid(unsafe_code)]
#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro,
    clippy::print_stdout,
    clippy::print_stderr
)]

use std::io::Write as _;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use narwhal_app::clipboard::{ArboardClipboard, Clipboard};
use narwhal_app::export::{write_format, ExportFormat};
use narwhal_app::{App, DriverRegistry as AppDriverRegistry};
use narwhal_config::{ConfigPaths, ConnectionsFile, CredentialStore, KeyringStore, Settings};
use narwhal_core::{Connection, ConnectionConfig};
use narwhal_history::Journal;
use narwhal_mcp::{DriverRegistry as McpDriverRegistry, McpServer, ServerContext, Workspace};
use secrecy::ExposeSecret;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

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
}

#[derive(Debug, Subcommand)]
enum Mode {
    /// Run as a Model Context Protocol server on stdio.
    Mcp,
    /// Execute one SQL statement and print the result. Pipes-friendly.
    Exec(ExecArgs),
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
        Some(Mode::Mcp) => run_mcp(paths).await,
        Some(Mode::Exec(args)) => run_exec(paths, args).await,
        None => run_tui(paths).await,
    }
}

/// TUI mode (default): logs go to a daily-rotating file because the
/// terminal is owned by the UI in raw mode.
async fn run_tui(paths: ConfigPaths) -> Result<()> {
    let file_appender = tracing_appender::rolling::daily(paths.log_dir(), "narwhal.log");
    let (writer, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(fmt::layer().with_writer(writer).with_ansi(false))
        .init();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting narwhal");

    // L20: log instead of silently swallowing a malformed settings file
    // — a user-visible warning beats falling back to defaults blind.
    let settings = match Settings::load(&paths.settings_file()) {
        Ok(s) => s,
        Err(error) => {
            tracing::warn!(
                path = %paths.settings_file().display(),
                error = %error,
                "falling back to default settings"
            );
            Settings::default()
        }
    };
    let connections = match ConnectionsFile::load(&paths.connections_file()) {
        Ok(c) => c,
        Err(error) => {
            tracing::warn!(
                path = %paths.connections_file().display(),
                error = %error,
                "falling back to empty connections file"
            );
            ConnectionsFile::default()
        }
    };
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
    let app = App::with_services(registry, connections, history, credentials, clipboard)
        .with_connections_path(paths.connections_file())
        .with_last_used_path(paths.last_used_file())
        .with_settings(settings)
        .with_plugins_dir(&paths.plugins_dir());

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
async fn run_mcp(paths: ConfigPaths) -> Result<()> {
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

    let connections = ConnectionsFile::load(&paths.connections_file())
        .with_context(|| {
            format!(
                "loading connections file: {}",
                paths.connections_file().display()
            )
        })
        .unwrap_or_else(|error| {
            // Empty connections file is a legitimate first-run state; we
            // log and keep going so `list_connections` simply returns [].
            tracing::warn!(error = %error, "falling back to empty connections file");
            ConnectionsFile::default()
        });

    let drivers = Arc::new(McpDriverRegistry::with_defaults());
    let credentials: Arc<dyn CredentialStore> = Arc::new(KeyringStore::new());
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
    let mut ctx = ServerContext::new(drivers, Arc::new(connections), credentials);
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
async fn run_exec(paths: ConfigPaths, args: ExecArgs) -> Result<()> {
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
            "unknown format `{}` — choose one of: csv, json, tsv, table",
            args.format
        )
    })?;
    if matches!(format, ExportFormat::Insert) {
        // `insert` needs a source table that the CLI cannot know about
        // — refuse here so the failure surfaces as a friendly error
        // instead of a deep ExportError::NoSourceTable later.
        anyhow::bail!("`insert` is not supported in exec mode — use the TUI's `:export` command");
    }

    let connections_file = ConnectionsFile::load(&paths.connections_file()).with_context(|| {
        format!(
            "loading connections file: {}",
            paths.connections_file().display()
        )
    })?;
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

    let password = resolve_password(&*credentials, &config).await;
    let driver = registry
        .get(&config.driver)
        .map_err(|e| anyhow::anyhow!("driver: {e}"))?;
    let mut conn: Box<dyn Connection> = driver
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
    let read_only = !args.write;
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

/// Credential resolution chain shared with the MCP path: keyring first,
/// then `~/.pgpass` / env-var fallback. Failures are not fatal — drivers
/// that accept passwordless auth simply receive `None`.
async fn resolve_password(
    credentials: &dyn CredentialStore,
    config: &ConnectionConfig,
) -> Option<String> {
    if let Ok(Some(secret)) = credentials.get(config.id).await {
        return Some(secret.expose_secret().to_string());
    }
    narwhal_config::pgpass::resolve_password(&config.driver, &config.params)
}
