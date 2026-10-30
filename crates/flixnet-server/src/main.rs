use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use flixnet_api::{AppState, Services, ops};
use flixnet_core::Config;
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_core::maintenance;
use flixnet_core::scheduler::TaskRegistry;
use flixnet_db::Db;
use flixnet_library::jobs as library_jobs;
use flixnet_library::scenes as scene_jobs;
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
    /// Restore a backup (`flixnet-backup-*.db`). Stop the server first.
    Restore {
        /// The backup file.
        file: PathBuf,
        /// Also restore the configuration file saved next to the backup.
        #[arg(long)]
        with_config: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let mut config = Config::load(&cli.config)
        .with_context(|| format!("invalid configuration ({})", cli.config.display()))?;
    config.source_file = Some(cli.config.clone());
    init_tracing(&config.log.level);

    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(config).await,
        Command::Migrate => {
            open_db(&config).await?.close().await;
            tracing::info!("database is up to date");
            Ok(())
        }
        Command::Openapi { output } => write_openapi(output.as_deref()),
        Command::Restore { file, with_config } => restore(&config, &file, with_config).await,
    }
}

/// Replaces the database (and optionally the config file) with a backup.
async fn restore(config: &Config, file: &std::path::Path, with_config: bool) -> anyhow::Result<()> {
    // A running server keeps its port; refuse to swap the database under it.
    if let Err(err) = std::net::TcpListener::bind(config.server.bind)
        && err.kind() == std::io::ErrorKind::AddrInUse
    {
        anyhow::bail!(
            "{} is in use – stop the Flixnet server before restoring a backup",
            config.server.bind
        );
    }
    let db_path = config.paths.config_dir.join("flixnet.db");
    let kept = flixnet_db::backup::restore(file, &db_path)
        .await
        .with_context(|| format!("cannot restore {}", file.display()))?;
    if let Some(kept) = &kept {
        tracing::info!(previous = %kept.display(), "previous database kept");
    }
    if with_config {
        let saved = file.with_extension("toml");
        let target = config
            .source_file
            .clone()
            .unwrap_or_else(|| PathBuf::from("flixnet.toml"));
        if saved.is_file() {
            if target.is_file() {
                let aside = PathBuf::from(format!(
                    "{}.before-restore-{}",
                    target.display(),
                    flixnet_db::now_ms()
                ));
                std::fs::rename(&target, &aside)?;
            }
            std::fs::copy(&saved, &target)
                .with_context(|| format!("cannot write {}", target.display()))?;
            tracing::info!(path = %target.display(), "configuration restored");
        } else {
            tracing::warn!(path = %saved.display(), "the backup has no configuration file");
        }
    }
    // Opening applies migrations the backup may be missing.
    open_db(config).await?.close().await;
    tracing::info!(backup = %file.display(), "backup restored; start the server again");
    Ok(())
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
    // The same lines are kept in memory for the dashboard's log view.
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr())),
        )
        .with(flixnet_api::ops::logs::layer())
        .init();
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let bind = config.server.bind;
    let db = open_db(&config).await?;
    let events = EventBus::new();

    let transcoder = TranscodeManager::new(
        &config.media.ffmpeg,
        config.paths.transcode_dir(),
        TranscodeSettings::default(),
    );
    // Extracting one stream reads the whole file; allow that for long films on slow disks.
    let subtitles = flixnet_media::subtitles::SubtitleCache::new(
        &config.media.ffmpeg,
        config.paths.config_dir.join("cache").join("subtitles"),
        Duration::from_secs(600),
        2,
    );

    // Background work: job workers plus the scheduler for recurring tasks.
    let ffprobe = flixnet_media::Ffprobe::new(&config.media.ffprobe);
    // AI (embeddings, recommendations), shared by its jobs and the API.
    let ai = flixnet_ai::Ai::new(db.clone(), &config);
    if ai.is_stub() {
        tracing::warn!("ai.embedder = stub: test-only embeddings, not for real libraries");
    }
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
        .register(trickplay_jobs::BackfillThumbnails)
        .register(flixnet_library::subtitles::AutoDownloadSubtitles {
            store_dir: config.paths.config_dir.join("subtitles"),
        })
        .register(ops::notify::DeliverNotification {
            http: flixnet_library::http::client(),
        })
        .register(ops::backup::BackupDatabase {
            config: config.clone(),
        })
        .register(ops::trakt::SyncAllTrakt {
            http: flixnet_library::http::client(),
            base_url: config.integrations.trakt_api_url.clone(),
        })
        // Scene search and the optional LLM features.
        .register(scene_jobs::IndexSceneText {
            subtitles: subtitles.clone(),
            transcoder: transcoder.clone(),
        })
        .register(scene_jobs::TranscribeAudio {
            ffmpeg: config.media.ffmpeg.clone(),
            models_dir: config.paths.config_dir.join("models").join("whisper"),
            work_dir: config.paths.transcode_dir().join("whisper"),
            transcoder: transcoder.clone(),
            http: flixnet_library::http::client(),
        })
        .register(scene_jobs::SceneIndexBackfill)
        .register(flixnet_llm::themes::GenerateThemeCollections)
        .register(flixnet_library::photos::AnalyzeClip {
            ffprobe: flixnet_media::Ffprobe::new(&config.media.ffprobe),
            ffmpeg: config.media.ffmpeg.clone(),
            cache_dir: config.paths.config_dir.join("cache"),
        })
        .register(flixnet_library::audiobooks::AnalyzeBook {
            ffprobe: flixnet_media::Ffprobe::new(&config.media.ffprobe),
            ffmpeg: config.media.ffmpeg.clone(),
            cache_dir: config.paths.config_dir.join("cache"),
        })
        .register(flixnet_ai::jobs::EmbedItems { ai: ai.clone() })
        .register(flixnet_ai::jobs::DownloadModel { ai: ai.clone() });
    let jobs = runtime.handle();
    let running_jobs = runtime.start().await?;
    let tasks = TaskRegistry::new(
        maintenance::tasks()
            .into_iter()
            .chain(library_jobs::tasks())
            .chain(livetv_jobs::tasks())
            .chain(segment_jobs::tasks())
            .chain(trickplay_jobs::tasks())
            .chain(flixnet_library::subtitles::tasks())
            .chain(ops::tasks())
            .chain(scene_jobs::tasks())
            .chain(flixnet_llm::themes::tasks())
            .chain(flixnet_ai::jobs::tasks())
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
    // Live transcodes use the encoder hardware detection picks.
    livetv.use_encoders_of(transcoder.clone());

    // AI jobs pause while anyone transcodes; new metadata gets embedded shortly after.
    ai.set_busy_probe(flixnet_api::playback_busy_probe(&transcoder, &livetv));
    let ai_listener = flixnet_ai::jobs::spawn_event_listener(
        &events,
        jobs.queue().clone(),
        Duration::from_secs(if ai.is_stub() { 2 } else { 60 }),
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
        subtitles: subtitles.clone(),
        livetv: livetv.clone(),
    };
    let jellyfin = config.jellyfin.clone();
    let state = AppState::new(config, db.clone(), services).with_ai(ai.clone());
    flixnet_api::start_services(&state).await?;
    let dvr = state.dvr.clone();
    // Load the model and the vectors in the background, so the first search is quick.
    tokio::spawn(async move {
        if let Err(err) = ai.ensure_index().await {
            tracing::warn!(error = %format!("{err:#}"), "loading the vector index failed");
        }
        if let Err(err) = ai.embedder().await {
            tracing::warn!(error = %format!("{err:#}"), "local embedding model unavailable");
        }
    });
    let app = flixnet_api::router(state.clone()).fallback_service(web::router());
    // DLNA runs only once an administrator picked its user (settings).
    state.dlna.apply(&state).await;
    let dlna = state.dlna.clone();

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

    dlna.stop().await;
    ai_listener.abort();
    transcoder.stop_all().await;
    // Running recordings resume into a new part after the next start.
    dvr.shutdown().await;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config(dir: &std::path::Path) -> Config {
        let mut config = Config::default();
        config.paths.config_dir = dir.join("config");
        config.server.bind = "127.0.0.1:0".parse().unwrap();
        config.source_file = Some(dir.join("flixnet.toml"));
        config
    }

    #[tokio::test]
    async fn restore_brings_back_database_and_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        std::fs::write(config.source_file.as_ref().unwrap(), "# vorher\n").unwrap();

        // A backup with one user, taken with the server's own backup code.
        let db = open_db(&config).await.unwrap();
        flixnet_db::users::create_first_admin(&db, "Felix", "x")
            .await
            .unwrap();
        let backup = ops::backup::run_backup(&db, &config).await.unwrap();
        db.close().await;
        let file = ops::backup::default_directory(&config).join(&backup.name);
        assert!(backup.has_config);

        // Meanwhile: everything changed.
        let db = open_db(&config).await.unwrap();
        flixnet_db::users::create(
            &db,
            &flixnet_db::users::NewUser {
                name: "Später",
                password_hash: None,
                pin_hash: None,
                is_admin: false,
                is_hidden: false,
                max_age_rating: None,
            },
        )
        .await
        .unwrap();
        db.close().await;
        std::fs::write(config.source_file.as_ref().unwrap(), "# nachher\n").unwrap();

        restore(&config, &file, true).await.unwrap();
        let db = open_db(&config).await.unwrap();
        let names: Vec<String> = flixnet_db::users::list(&db)
            .await
            .unwrap()
            .into_iter()
            .map(|u| u.name)
            .collect();
        db.close().await;
        assert_eq!(names, ["Felix"]);
        assert_eq!(
            std::fs::read_to_string(config.source_file.as_ref().unwrap()).unwrap(),
            "# vorher\n"
        );
    }

    #[tokio::test]
    async fn restore_refuses_while_the_server_runs() {
        let dir = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config(dir.path());
        config.server.bind = listener.local_addr().unwrap();
        let err = restore(&config, &dir.path().join("x.db"), false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("stop the Flixnet server"), "{err}");
    }
}
