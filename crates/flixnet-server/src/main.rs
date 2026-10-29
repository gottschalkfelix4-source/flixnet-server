use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use flixnet_api::{AppState, Services};
use flixnet_core::Config;
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_core::maintenance;
use flixnet_core::scheduler::TaskRegistry;
use flixnet_db::Db;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

mod web;

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
    /// Print the OpenAPI spec as JSON (to stdout, or to a UTF-8 file with --output).
    Openapi {
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
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
        Command::Openapi { output } => write_openapi(output.as_deref()),
    }
}

fn write_openapi(output: Option<&std::path::Path>) -> anyhow::Result<()> {
    let mut json = flixnet_api::openapi().to_pretty_json()?;
    json.push('\n');
    match output {
        Some(path) => {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, json)
                .with_context(|| format!("cannot write {}", path.display()))?;
            tracing::info!(path = %path.display(), "OpenAPI spec written");
        }
        None => print!("{json}"),
    }
    Ok(())
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
    // Logs go to stderr so stdout stays clean for command output (e.g. `openapi`).
    // Colors only on a real terminal, so `docker logs` / Unraid show plain text.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let bind = config.server.bind;
    let db = open_db(&config).await?;
    let events = EventBus::new();

    // Background work: job workers plus the scheduler for recurring tasks.
    let runtime = JobRuntime::new(db.clone(), events.clone()).register(maintenance::CleanupJobs);
    let jobs = runtime.handle();
    let running_jobs = runtime.start().await?;
    let tasks = TaskRegistry::new(maintenance::tasks());
    let stop_scheduler = CancellationToken::new();
    let scheduler = tasks
        .clone()
        .start(
            db.clone(),
            jobs.queue().clone(),
            Duration::from_secs(30),
            stop_scheduler.clone(),
        )
        .await?;

    let services = Services {
        events,
        jobs,
        tasks,
    };
    let app = flixnet_api::router(AppState::new(config, db.clone(), services))
        .fallback_service(web::router());

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("cannot listen on {bind}"))?;
    tracing::info!(%bind, version = flixnet_core::VERSION, "Flixnet is listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    stop_scheduler.cancel();
    let _ = scheduler.await;
    running_jobs.shutdown(Duration::from_secs(10)).await;
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
