#![forbid(unsafe_code)]

use std::sync::Arc;

use anyhow::{Context, Result};
use narwhal_app::{App, DriverRegistry};
use narwhal_config::{ConfigPaths, ConnectionsFile, KeyringStore, Settings};
use narwhal_history::Journal;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[tokio::main]
async fn main() -> Result<()> {
    let paths = ConfigPaths::discover().context("resolving user directories")?;
    paths
        .ensure()
        .context("creating configuration directories")?;

    // Logs are written to disk because the terminal is owned by the UI in
    // raw mode. The non-blocking guard must remain alive for the duration
    // of the process.
    let file_appender = tracing_appender::rolling::daily(paths.log_dir(), "narwhal.log");
    let (writer, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(fmt::layer().with_writer(writer).with_ansi(false))
        .init();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting narwhal");

    let _settings = Settings::load(&paths.settings_file()).unwrap_or_default();
    let connections = ConnectionsFile::load(&paths.connections_file()).unwrap_or_default();
    let history = match Journal::open(paths.history_file()).await {
        Ok(j) => Some(Arc::new(j)),
        Err(error) => {
            tracing::warn!(error = %error, "history journal disabled");
            None
        }
    };

    let registry = DriverRegistry::with_defaults();
    let credentials: Arc<dyn narwhal_config::CredentialStore> = Arc::new(KeyringStore::new());
    let app = App::with_credentials(registry, connections, history, credentials)
        .with_connections_path(paths.connections_file());

    if let Err(error) = app.run().await {
        tracing::error!(error = %error, "fatal error");
        eprintln!("narwhal: fatal: {error:#}");
        std::process::exit(1);
    }

    Ok(())
}
