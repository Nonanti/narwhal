#![forbid(unsafe_code)]

use anyhow::{Context, Result};
use narwhal_app::{App, DriverRegistry};
use narwhal_config::ConfigPaths;
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

    let registry = DriverRegistry::with_defaults();
    let app = App::new(registry);

    if let Err(error) = app.run().await {
        tracing::error!(error = %error, "fatal error");
        eprintln!("narwhal: fatal: {error:#}");
        std::process::exit(1);
    }

    Ok(())
}
