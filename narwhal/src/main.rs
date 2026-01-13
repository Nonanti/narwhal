#![forbid(unsafe_code)]

use std::sync::Arc;

use anyhow::{Context, Result};
use narwhal_app::clipboard::{ArboardClipboard, Clipboard};
use narwhal_app::{App, DriverRegistry as AppDriverRegistry};
use narwhal_config::{ConfigPaths, ConnectionsFile, CredentialStore, KeyringStore, Settings};
use narwhal_history::Journal;
use narwhal_mcp::{DriverRegistry as McpDriverRegistry, McpServer, ServerContext, Workspace};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[tokio::main]
async fn main() -> Result<()> {
    let paths = ConfigPaths::discover().context("resolving user directories")?;
    paths
        .ensure()
        .context("creating configuration directories")?;

    // First positional argument selects the run mode. We deliberately do
    // not pull in a CLI parser yet — there are exactly two modes and a
    // single token is enough to disambiguate them. The default (no args)
    // keeps the historical `narwhal` → TUI behaviour intact.
    let mode = std::env::args().nth(1);

    match mode.as_deref() {
        Some("mcp") => run_mcp(paths).await,
        Some("--help" | "-h") => {
            print_help();
            Ok(())
        }
        Some("--version" | "-V") => {
            println!("narwhal {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(other) => {
            eprintln!("narwhal: unknown subcommand: {other}\n");
            print_help();
            std::process::exit(2);
        }
        None => run_tui(paths).await,
    }
}

fn print_help() {
    println!(
        "\
narwhal {version} — TUI database client

USAGE:
    narwhal               Launch the TUI (default)
    narwhal mcp           Run as a Model Context Protocol server on stdio
    narwhal --help        Show this help
    narwhal --version     Show the version
",
        version = env!("CARGO_PKG_VERSION")
    );
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
