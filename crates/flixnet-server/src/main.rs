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
use flixnet_library::jobs as library_jobs;
use flixnet_library::segments as segment_jobs;
use flixnet_library::trickplay as trickplay_jobs;
use flixnet_livetv::jobs as livetv_jobs;
use flixnet_media::hardware::{HardwareMode, HardwareOptions};
use flixnet_media::transcode::{TranscodeManager, TranscodeSettings};
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
    let ffprobe = flixnet_media::Ffprobe::new(&config.media.ffprobe);
    let runtime = JobRuntime::new(db.clone(), events.clone())
        .register(maintenance::CleanupJobs)
        .register(library_jobs::ScanLibrary)
        .register(library_jobs::ScanAllLibraries)
        .register(library_jobs::RefreshMetadata)
        .register(library_jobs::ProbeFile { ffprobe })
        .register(livetv_jobs::ImportChannels {
            http: flixnet_livetv::http::client(),
        })
        .register(livetv_jobs::RefreshGuide {
            http: flixnet_livetv::http::client(),
            cache_dir: config.paths.config_dir.join("cache").join("epg"),
        })
        .register(segment_jobs::AnalyzeSeason {
            analyzer: flixnet_media::segments::SegmentAnalyzer::new(&config.media.ffmpeg),
        })
        .register(segment_jobs::AnalyzePendingSeasons)
        .register(trickplay_jobs::GenerateTrickplay {
            ffmpeg: config.media.ffmpeg.clone(),
            cache_dir: config.paths.config_dir.join("cache"),
        })
        .register(trickplay_jobs::GenerateChapterImages {
            ffmpeg: config.media.ffmpeg.clone(),
            cache_dir: config.paths.config_dir.join("cache"),
        })
        .register(trickplay_jobs::BackfillThumbnails);
    let jobs = runtime.handle();
    let running_jobs = runtime.start().await?;
    let tasks = TaskRegistry::new(
        maintenance::tasks()
            .into_iter()
            .chain(library_jobs::tasks())
            .chain(livetv_jobs::tasks())
            .chain(segment_jobs::tasks())
            .chain(trickplay_jobs::tasks())
            .collect(),
    );
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

    // Library folder watching (best effort; periodic rescans are the safety net).
    let watcher = if config.media.watch_folders {
        let watcher = flixnet_library::watch::LibraryWatcher::start(
            jobs.queue().clone(),
            Duration::from_secs(30),
        );
        if let Err(err) = watcher.sync(&db).await {
            tracing::warn!(%err, "file watcher unavailable; relying on periodic scans");
        }
        Some(watcher)
    } else {
        None
    };

    let transcoder = TranscodeManager::new(
        &config.media.ffmpeg,
        config.paths.transcode_dir(),
        TranscodeSettings::default(),
    );
    // Testing the GPU takes seconds; transcodes use software until it is done.
    tokio::spawn({
        let transcoder = transcoder.clone();
        let options = hardware_options(&config.media);
        async move { transcoder.detect_hardware(options).await }
    });

    let livetv = flixnet_livetv::LiveTvManager::new(
        &config.media.ffmpeg,
        &config.media.ffprobe,
        config.paths.transcode_dir().join("live"),
        flixnet_livetv::LiveSettings::default(),
    );

    let services = Services {
        events,
        jobs,
        tasks,
        images: flixnet_library::images::ImageCache::new(
            config.paths.config_dir.join("cache").join("images"),
        ),
        url_secret: url_signing_secret(&db).await?,
        watcher,
        transcoder: transcoder.clone(),
        ffprobe: flixnet_media::Ffprobe::new(&config.media.ffprobe),
        // Extracting one stream reads the whole file; allow that for long films on slow disks.
        subtitles: flixnet_media::subtitles::SubtitleCache::new(
            &config.media.ffmpeg,
            config.paths.config_dir.join("cache").join("subtitles"),
            Duration::from_secs(600),
            2,
        ),
        livetv: livetv.clone(),
    };
    let jellyfin = config.jellyfin.clone();
    let state = AppState::new(config, db.clone(), services);
    let app = flixnet_api::router(state.clone()).fallback_service(web::router());

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("cannot listen on {bind}"))?;
    tracing::info!(%bind, version = flixnet_core::VERSION, "Flixnet is listening");

    let shutdown = CancellationToken::new();
    tokio::spawn({
        let shutdown = shutdown.clone();
        async move {
            shutdown_signal().await;
            shutdown.cancel();
        }
    });

    // Jellyfin-compatible API for existing Jellyfin clients, on its own port.
    let jellyfin_server = if jellyfin.enabled {
        let listener = tokio::net::TcpListener::bind(jellyfin.bind)
            .await
            .with_context(|| format!("cannot listen on {} (jellyfin.bind)", jellyfin.bind))?;
        tracing::info!(bind = %jellyfin.bind, "Jellyfin-compatible API is listening");
        let router = flixnet_jellyfin::router(state);
        let shutdown = shutdown.clone();
        Some(tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
        }))
    } else {
        None
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.clone().cancelled_owned())
        .await?;
    shutdown.cancel();
    if let Some(server) = jellyfin_server
        && let Err(err) = server.await?
    {
        tracing::warn!(%err, "Jellyfin-compatible API stopped with an error");
    }

    transcoder.stop_all().await;
    livetv.stop_all().await;
    stop_scheduler.cancel();
    let _ = scheduler.await;
    running_jobs.shutdown(Duration::from_secs(10)).await;
    db.close().await;
    tracing::info!("Flixnet stopped");
    Ok(())
}

fn hardware_options(media: &flixnet_core::config::MediaConfig) -> HardwareOptions {
    use flixnet_core::config::HardwareAcceleration as Configured;
    HardwareOptions {
        mode: match media.hardware_acceleration {
            Configured::Auto => HardwareMode::Auto,
            Configured::None => HardwareMode::None,
            Configured::Qsv => HardwareMode::Qsv,
            Configured::Vaapi => HardwareMode::Vaapi,
            Configured::Nvenc => HardwareMode::Nvenc,
        },
        vaapi_device: media.vaapi_device.clone(),
    }
}

/// Secret for signed media URLs, created once and stored in the database so URLs
/// handed out before a restart stay valid.
async fn url_signing_secret(db: &Db) -> anyhow::Result<Vec<u8>> {
    const KEY: &str = "url_signing_secret";
    if let Some(secret) = flixnet_db::settings::get::<String>(db, KEY).await? {
        return Ok(secret.into_bytes());
    }
    let secret = flixnet_core::auth::new_token().token;
    flixnet_db::settings::set(db, KEY, &secret).await?;
    Ok(secret.into_bytes())
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
