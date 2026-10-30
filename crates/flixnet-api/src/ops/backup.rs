//! Backups: a consistent snapshot of the database (`VACUUM INTO`) plus a copy of the
//! configuration file, written to a backup folder that keeps the newest N.
//!
//! Runs daily as the scheduled task `backup_database` and on demand ("Jetzt sichern").
//! Restoring is a CLI command while the server is stopped:
//! `flixnet-server restore <file.db>`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use flixnet_core::Config;
use flixnet_core::jobs::{JobContext, JobHandler, Pool};
use flixnet_core::scheduler::TaskDef;
use flixnet_db::{Db, settings};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::time::file_stamp;
use crate::AppState;
use crate::auth::AdminUser;
use crate::error::{ApiError, ApiResult, ErrorBody};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_backups, create_backup))
        .routes(routes!(update_backup_settings))
}

const SETTINGS_KEY: &str = "backup";
const DEFAULT_KEEP: u32 = 7;
const PREFIX: &str = "flixnet-backup-";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct BackupSettings {
    directory: Option<String>,
    keep: Option<u32>,
}

impl BackupSettings {
    async fn load(db: &Db) -> flixnet_db::Result<Self> {
        Ok(settings::get(db, SETTINGS_KEY).await?.unwrap_or_default())
    }

    fn directory(&self, config: &Config) -> PathBuf {
        self.directory
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| default_directory(config))
    }

    fn keep(&self) -> u32 {
        self.keep.unwrap_or(DEFAULT_KEEP)
    }
}

pub fn default_directory(config: &Config) -> PathBuf {
    config.paths.config_dir.join("backups")
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BackupFileDto {
    /// File name of the database snapshot.
    pub name: String,
    pub size_bytes: u64,
    /// Unix milliseconds.
    pub created_at: i64,
    /// A copy of the configuration file was saved next to it.
    pub has_config: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BackupOverviewDto {
    /// Folder backups are written to.
    pub directory: String,
    /// Whether `directory` is the default (`<config>/backups`).
    pub is_default_directory: bool,
    /// How many backups are kept.
    pub keep: u32,
    /// Newest first.
    pub files: Vec<BackupFileDto>,
    /// The command that restores a backup (with the server stopped).
    pub restore_command: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateBackupSettings {
    /// Absolute folder; empty resets to the default.
    pub directory: Option<String>,
    /// 1–365.
    pub keep: Option<u32>,
}

/// Backups in `dir`, newest first.
pub fn list_files(dir: &Path) -> Vec<BackupFileDto> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<BackupFileDto> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(PREFIX) || !name.ends_with(".db") {
                return None;
            }
            let meta = entry.metadata().ok()?;
            let created_at = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as i64);
            Some(BackupFileDto {
                has_config: dir.join(name.replace(".db", ".toml")).is_file(),
                name,
                size_bytes: meta.len(),
                created_at,
            })
        })
        .collect();
    // Names carry a sortable UTC timestamp.
    files.sort_by(|a, b| b.name.cmp(&a.name));
    files
}

/// Writes a backup now and removes the oldest beyond the configured number.
pub async fn run_backup(db: &Db, config: &Config) -> anyhow::Result<BackupFileDto> {
    let settings = BackupSettings::load(db).await?;
    let dir = settings.directory(config);
    std::fs::create_dir_all(&dir)?;
    // `flixnet-backup-20261030-142501-123.db`: fixed width, so names sort by time.
    let mut now = flixnet_db::now_ms();
    let mut name = format!("{PREFIX}{}-{:03}.db", file_stamp(now), now % 1000);
    while dir.join(&name).exists() {
        now += 1;
        name = format!("{PREFIX}{}-{:03}.db", file_stamp(now), now % 1000);
    }
    let path = dir.join(&name);
    flixnet_db::backup::snapshot(db, &path).await?;
    if let Some(source) = config.source_file.as_deref().filter(|p| p.is_file()) {
        std::fs::copy(source, dir.join(name.replace(".db", ".toml")))?;
    }

    let files = list_files(&dir);
    for old in files.iter().skip(settings.keep() as usize) {
        let _ = std::fs::remove_file(dir.join(&old.name));
        let _ = std::fs::remove_file(dir.join(old.name.replace(".db", ".toml")));
    }
    tracing::info!(path = %path.display(), "backup written");
    list_files(&dir)
        .into_iter()
        .find(|f| f.name == name)
        .ok_or_else(|| anyhow::anyhow!("backup vanished after writing"))
}

async fn overview(state: &AppState) -> ApiResult<BackupOverviewDto> {
    let settings = BackupSettings::load(&state.db).await?;
    let dir = settings.directory(&state.config);
    let files = tokio::task::spawn_blocking({
        let dir = dir.clone();
        move || list_files(&dir)
    })
    .await?;
    Ok(BackupOverviewDto {
        directory: dir.display().to_string(),
        is_default_directory: settings.directory.is_none(),
        keep: settings.keep(),
        restore_command: format!(
            "flixnet-server restore {}",
            dir.join(files.first().map_or("<datei>.db", |f| f.name.as_str()))
                .display()
        ),
        files,
    })
}

/// Backup folder, retention and existing backups (administrators).
#[utoipa::path(
    get,
    path = "/backups",
    operation_id = "listBackups",
    tag = "backups",
    responses(
        (status = OK, description = "Backup overview", body = BackupOverviewDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn list_backups(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<BackupOverviewDto>> {
    overview(&state).await.map(Json)
}

/// Back up now ("Jetzt sichern").
#[utoipa::path(
    post,
    path = "/backups",
    operation_id = "createBackup",
    tag = "backups",
    responses(
        (status = OK, description = "The new backup", body = BackupFileDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn create_backup(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<BackupFileDto>> {
    run_backup(&state.db, &state.config)
        .await
        .map(Json)
        .map_err(|err| {
            tracing::error!(err = %format!("{err:#}"), "backup failed");
            ApiError::new(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "backup_failed",
                format!("Die Sicherung ist fehlgeschlagen: {err}"),
            )
        })
}

/// Change the backup folder and how many backups are kept.
#[utoipa::path(
    put,
    path = "/settings/backup",
    operation_id = "updateBackupSettings",
    tag = "backups",
    request_body = UpdateBackupSettings,
    responses(
        (status = OK, description = "Backup overview", body = BackupOverviewDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid folder or number", body = ErrorBody),
    )
)]
async fn update_backup_settings(
    _: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<UpdateBackupSettings>,
) -> ApiResult<Json<BackupOverviewDto>> {
    let mut current = BackupSettings::load(&state.db).await?;
    if let Some(dir) = body.directory {
        let dir = dir.trim();
        if dir.is_empty() {
            current.directory = None;
        } else {
            if !Path::new(dir).is_absolute() {
                return Err(ApiError::validation(
                    "Bitte einen absoluten Ordnerpfad angeben.",
                ));
            }
            std::fs::create_dir_all(dir).map_err(|e| {
                ApiError::validation(format!("Der Ordner lässt sich nicht anlegen: {e}"))
            })?;
            current.directory = Some(dir.to_owned());
        }
    }
    if let Some(keep) = body.keep {
        if !(1..=365).contains(&keep) {
            return Err(ApiError::validation(
                "Es werden zwischen 1 und 365 Sicherungen aufbewahrt.",
            ));
        }
        current.keep = Some(keep);
    }
    settings::set(&state.db, SETTINGS_KEY, &current).await?;
    overview(&state).await.map(Json)
}

/// The scheduled backup.
pub struct BackupDatabase {
    pub config: Config,
}

impl JobHandler for BackupDatabase {
    const KIND: &'static str = "maintenance.backup";
    const POOL: Pool = Pool::Io;
    const MAX_ATTEMPTS: i64 = 2;
    type Payload = ();

    async fn run(&self, ctx: JobContext, _: ()) -> anyhow::Result<()> {
        run_backup(ctx.db(), &self.config).await.map(|_| ())
    }
}

pub fn tasks() -> Vec<TaskDef> {
    vec![TaskDef::new::<BackupDatabase>(
        "backup_database",
        "Datenbank und Konfiguration sichern",
        Duration::from_secs(24 * 60 * 60),
        &(),
    )]
}
