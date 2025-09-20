use anyhow::{Context, Result};
use narwhal_app::{App, DriverRegistry};
use narwhal_config::ConfigPaths;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[tokio::main]
async fn main() -> Result<()> {
    let paths = ConfigPaths::discover().context("could not resolve config paths")?;
    paths.ensure().context("could not create config directories")?;

    // Logs go to disk — stdout/stderr are owned by ratatui in raw mode.
    let file_appender = tracing_appender::rolling::daily(paths.log_dir(), "narwhal.log");
    let (writer, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(fmt::layer().with_writer(writer).with_ansi(false))
        .init();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "narwhal starting");

    let registry = DriverRegistry::with_defaults();
    let app = App::new(registry);
    if let Err(err) = app.run().await {
        tracing::error!(error = %err, "fatal");
        eprintln!("narwhal: fatal: {err:#}");
        std::process::exit(1);
    }

    Ok(())
}
