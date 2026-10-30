//! Server overview for administrators: what plays and how much bandwidth it takes,
//! CPU, memory, storage of the config and transcode folders, and the job queue.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use flixnet_db::{items, jobs, media};
use flixnet_media::decision::PlayMethod;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AdminUser;
use crate::error::{ApiResult, ErrorBody};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_dashboard))
}

/// When this process started (unix ms).
pub static STARTED_AT: LazyLock<i64> = LazyLock::new(flixnet_db::now_ms);

/// Folder sizes are walked at most this often.
const DIR_SIZE_TTL: Duration = Duration::from_secs(300);

/// CPU usage needs two samples, so the `System` lives across requests.
pub struct SystemProbe {
    system: Mutex<Option<sysinfo::System>>,
    dir_sizes: Mutex<HashMap<PathBuf, (Instant, u64)>>,
}

impl Default for SystemProbe {
    fn default() -> Self {
        LazyLock::force(&STARTED_AT);
        Self {
            system: Mutex::new(None),
            dir_sizes: Mutex::new(HashMap::new()),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct SystemSample {
    cpu_percent: f32,
    memory_total: u64,
    memory_used: u64,
    process_memory: u64,
    process_cpu_percent: f32,
}

impl SystemProbe {
    fn sample(&self) -> SystemSample {
        let mut guard = self.system.lock().expect("system lock");
        if guard.is_none() {
            *guard = sysinfo::System::new().ok();
        }
        let Some(system) = guard.as_mut() else {
            return SystemSample::default();
        };
        system.refresh_cpu_usage();
        system.refresh_memory();
        let mut sample = SystemSample {
            cpu_percent: system.global_cpu_usage(),
            memory_total: system.total_memory(),
            memory_used: system.used_memory(),
            ..Default::default()
        };
        if let Ok(pid) = sysinfo::get_current_pid() {
            system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
            if let Some(process) = system.process(pid) {
                sample.process_memory = process.memory();
                sample.process_cpu_percent = process.cpu_usage();
            }
        }
        sample
    }

    fn dir_size(&self, dir: &Path) -> u64 {
        if let Some((at, size)) = self.dir_sizes.lock().expect("dir lock").get(dir)
            && at.elapsed() < DIR_SIZE_TTL
        {
            return *size;
        }
        let size = walk_size(dir);
        self.dir_sizes
            .lock()
            .expect("dir lock")
            .insert(dir.to_path_buf(), (Instant::now(), size));
        size
    }
}

/// Total size of the files below `dir` (symlinks are not followed).
fn walk_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    let mut visited = 0usize;
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > 1_000_000 {
                return total;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file()
                && let Ok(meta) = entry.metadata()
            {
                total += meta.len();
            }
        }
    }
    total
}

/// Drops Windows' `\\?\` prefix so paths compare with mount points.
fn plain(path: &Path) -> String {
    let s = path.to_string_lossy();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_owned()
}

/// The disk a folder lives on: (total, available) bytes.
fn disk_of(disks: &sysinfo::Disks, dir: &Path) -> Option<(u64, u64)> {
    let canonical = std::fs::canonicalize(dir).ok()?;
    let target = PathBuf::from(plain(&canonical));
    disks
        .into_iter()
        .filter(|d| target.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| (d.total_space(), d.available_space()))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct StorageDto {
    /// "Konfiguration" or "Transcoding".
    pub label: String,
    pub path: String,
    /// Bytes used by the folder itself.
    pub used_bytes: u64,
    /// The disk the folder is on (`None` if unknown).
    pub disk_total_bytes: Option<u64>,
    pub disk_available_bytes: Option<u64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SessionBandwidthDto {
    pub session_id: String,
    pub user_name: String,
    /// What plays (display title).
    pub title: String,
    pub play_method: PlayMethod,
    pub is_paused: bool,
    /// Bytes served for this session's stream so far.
    pub bytes: u64,
    /// Current rate (average over the last seconds).
    pub bytes_per_second: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct JobCountsDto {
    pub queued: i64,
    pub running: i64,
    pub failed: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DashboardDto {
    pub version: String,
    /// Unix milliseconds.
    pub started_at: i64,
    /// Whole machine (or container), percent of all cores.
    pub cpu_percent: f32,
    pub memory_total_bytes: u64,
    pub memory_used_bytes: u64,
    /// Flixnet itself.
    pub process_memory_bytes: u64,
    pub process_cpu_percent: f32,
    pub storage: Vec<StorageDto>,
    /// All media traffic since the start.
    pub bandwidth_total_bytes: u64,
    pub bandwidth_bytes_per_second: u64,
    pub sessions: Vec<SessionBandwidthDto>,
    /// Running ffmpeg transcodes.
    pub transcodes: usize,
    pub jobs: JobCountsDto,
}

/// Server overview (administrators).
#[utoipa::path(
    get,
    path = "/dashboard",
    operation_id = "getDashboard",
    tag = "dashboard",
    responses(
        (status = OK, description = "Current server state", body = DashboardDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn get_dashboard(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<DashboardDto>> {
    let probe = state.ops.system.clone();
    let config_dir = state.config.paths.config_dir.clone();
    let transcode_dir = state.config.paths.transcode_dir();
    let (sample, storage) = tokio::task::spawn_blocking(move || {
        let sample = probe.sample();
        let disks = sysinfo::Disks::new_with_refreshed_list().ok();
        let storage = [
            ("Konfiguration", config_dir),
            ("Transcoding", transcode_dir),
        ]
        .into_iter()
        .map(|(label, dir)| {
            let disk = disks.as_ref().and_then(|d| disk_of(d, &dir));
            StorageDto {
                label: label.to_owned(),
                used_bytes: probe.dir_size(&dir),
                path: dir.display().to_string(),
                disk_total_bytes: disk.map(|d| d.0),
                disk_available_bytes: disk.map(|d| d.1),
            }
        })
        .collect::<Vec<_>>();
        (sample, storage)
    })
    .await?;

    let meter = &state.ops.bandwidth;
    let mut sessions = Vec::new();
    for session in state.playback.list(flixnet_db::now_ms()) {
        let usage = match &session.transcode_session_id {
            Some(id) => meter.stream(&format!("transcode:{id}")),
            None => {
                let mut sum = super::bandwidth::Usage::default();
                for file in media::files_for_item(&state.db, session.item_id).await? {
                    let usage = meter.stream(&format!("file:{}", file.id));
                    sum.bytes += usage.bytes;
                    sum.bytes_per_second += usage.bytes_per_second;
                }
                sum
            }
        };
        let title = match flixnet_db::history::item_label(&state.db, session.item_id).await? {
            Some(title) => title,
            None => items::get(&state.db, session.item_id)
                .await?
                .map_or_else(|| "Unbekannter Eintrag".to_owned(), |i| i.title),
        };
        sessions.push(SessionBandwidthDto {
            session_id: session.id,
            user_name: session.user_name,
            title,
            play_method: session.play_method,
            is_paused: session.is_paused,
            bytes: usage.bytes,
            bytes_per_second: usage.bytes_per_second,
        });
    }

    let mut counts = JobCountsDto {
        queued: 0,
        running: 0,
        failed: 0,
    };
    for c in jobs::counts(&state.db).await? {
        match c.state.as_str() {
            "queued" => counts.queued += c.count,
            "running" => counts.running += c.count,
            "failed" => counts.failed += c.count,
            _ => {}
        }
    }
    let total = meter.total();
    Ok(Json(DashboardDto {
        version: flixnet_core::VERSION.to_owned(),
        started_at: *STARTED_AT,
        cpu_percent: sample.cpu_percent,
        memory_total_bytes: sample.memory_total,
        memory_used_bytes: sample.memory_used,
        process_memory_bytes: sample.process_memory,
        process_cpu_percent: sample.process_cpu_percent,
        storage,
        bandwidth_total_bytes: total.bytes,
        bandwidth_bytes_per_second: total.bytes_per_second,
        sessions,
        transcodes: state
            .transcoder
            .sessions()
            .await
            .iter()
            .filter(|s| s.transcoding)
            .count(),
        jobs: counts,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_sizes_add_up() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("a/one"), [0u8; 100]).unwrap();
        std::fs::write(dir.path().join("a/b/two"), [0u8; 23]).unwrap();
        assert_eq!(walk_size(dir.path()), 123);
        assert_eq!(walk_size(&dir.path().join("missing")), 0);
    }
}
