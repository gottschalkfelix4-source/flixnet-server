use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use flixnet_api::AppState;
use flixnet_core::Config;
use flixnet_db::Db;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "flixnet-server", version, about = "Flixnet media server")]
struct Cli {
    /// Path to the TOML configuration file (optional; defaults and env vars still apply).
    #[arg(long, env = "FLIXNET_CONFIG", default_value = "flixnet.toml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the media server (default).
    Serve,
    /// Apply pending database migrations and exit.
    Migrate,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = Config::load(&cli.config)
        .with_context(|| format!("invalid configuration ({})", cli.config.display()))?;
    init_tracing(&config.log.level);

    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(config).await,
        Command::Migrate => {
            open_db(&config).await?.close().await;
            tracing::info!("database is up to date");
            Ok(())
        }
    }
}

/// Opens `<config_dir>/flixnet.db`, creating the directory and applying migrations.
async fn open_db(config: &Config) -> anyhow::Result<Db> {
    let dir = &config.paths.config_dir;
    std::fs::create_dir_all(dir)
        .with_context(|| format!("cannot create config dir {}", dir.display()))?;
    let path = dir.join("flixnet.db");
    let db = Db::open(&path)
        .await
        .with_context(|| format!("cannot open database {}", path.display()))?;
    tracing::info!(path = %path.display(), "database ready");
    Ok(db)
}

fn init_tracing(default_level: &str) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let bind = config.server.bind;
    let db = open_db(&config).await?;
    let app = flixnet_api::router(AppState::new(config, db.clone()));

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("cannot listen on {bind}"))?;
    tracing::info!(%bind, version = flixnet_core::VERSION, "Flixnet is listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    db.close().await;
    tracing::info!("Flixnet stopped");
    Ok(())
}

/// Resolves on Ctrl+C, or on SIGTERM (sent by `docker stop`) on Unix.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
