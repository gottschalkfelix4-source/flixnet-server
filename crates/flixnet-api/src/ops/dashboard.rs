//! Server overview for administrators: what plays and how much bandwidth it takes,
//! CPU, memory, storage of the config and transcode folders, and the job queue.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Query, State};
use flixnet_db::{browse, items, jobs, media, stats};
use flixnet_media::decision::{PlayMethod, PlaybackPlan, StreamAction};
use flixnet_media::hardware::EncoderKind;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::bandwidth::Range;
use crate::AppState;
use crate::auth::AdminUser;
use crate::browse::{ItemCardDto, card};
use crate::error::{ApiResult, ErrorBody};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_dashboard))
        .routes(routes!(get_dashboard_stats))
        .routes(routes!(get_dashboard_bandwidth))
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

/// How one stream of a playback is delivered ("1080p (H.264)" → "720p (H.264)").
#[derive(Debug, Serialize, ToSchema)]
pub struct StreamLineDto {
    /// The file's stream, e.g. `1080p (H.264)` or `AC3 5.1`.
    pub source: String,
    /// What the client gets.
    pub target: String,
    /// `direct` (original file), `copy` (remuxed) or `transcode`.
    pub action: String,
    /// GPU encoder of a video transcode (`QuickSync`, `NVENC`, `VA-API`).
    pub hardware: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SessionBandwidthDto {
    pub session_id: String,
    pub user_id: i64,
    pub user_name: String,
    /// What plays (display title).
    pub title: String,
    /// The item, for poster and episode details (`None` if it vanished).
    pub item: Option<ItemCardDto>,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub client: Option<String>,
    pub device_name: Option<String>,
    pub video: Option<StreamLineDto>,
    pub audio: Option<StreamLineDto>,
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
    let transcodes = state.transcoder.sessions().await;
    let mut sessions = Vec::new();
    for session in state.playback.list(flixnet_db::now_ms()) {
        let transcode = session
            .transcode_session_id
            .as_ref()
            .and_then(|id| transcodes.iter().find(|t| &t.id == id));
        let file_id = match transcode {
            Some(t) => Some(t.file_id),
            None => media::files_for_item(&state.db, session.item_id)
                .await?
                .into_iter()
                .find(|f| f.role == "main")
                .map(|f| f.id),
        };
        let (video, audio) = match file_id {
            Some(file) => {
                let streams = media::streams(&state.db, file).await?;
                stream_lines(&streams, transcode.map(|t| (&t.plan, t.encoder)))
            }
            None => (None, None),
        };
        let cards = browse::summaries(&state.db, &[session.item_id], Some(session.user_id))
            .await?
            .into_iter()
            .map(|s| card(&state, s))
            .collect();
        let item = crate::browse::with_series_posters(&state, cards)
            .await?
            .into_iter()
            .next();
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
            user_id: session.user_id,
            user_name: session.user_name,
            title,
            item,
            position_ms: session.position_ms,
            duration_ms: session.duration_ms,
            client: session.client,
            device_name: session.device_name,
            video,
            audio,
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
        transcodes: transcodes.iter().filter(|s| s.transcoding).count(),
        jobs: counts,
    }))
}

/// Display name of a codec as people know it.
fn codec_name(codec: &str) -> String {
    match codec {
        "h264" => "H.264".to_owned(),
        "hevc" => "HEVC".to_owned(),
        "mpeg2video" => "MPEG-2".to_owned(),
        "mpeg4" => "MPEG-4".to_owned(),
        "eac3" => "E-AC3".to_owned(),
        "truehd" => "TrueHD".to_owned(),
        "dts" => "DTS".to_owned(),
        other => other.to_uppercase(),
    }
}

fn channels_name(channels: i64) -> String {
    match channels {
        1 => "Mono".to_owned(),
        2 => "Stereo".to_owned(),
        6 => "5.1".to_owned(),
        8 => "7.1".to_owned(),
        n => format!("{n} Kanäle"),
    }
}

fn resolution(height: Option<i64>) -> String {
    match height {
        Some(h) if h >= 2000 => "4K".to_owned(),
        Some(h) => format!("{h}p"),
        None => "?".to_owned(),
    }
}

fn hardware_name(encoder: EncoderKind) -> Option<String> {
    match encoder {
        EncoderKind::Software => None,
        EncoderKind::Qsv => Some("QuickSync".to_owned()),
        EncoderKind::Nvenc => Some("NVENC".to_owned()),
        EncoderKind::Vaapi => Some("VA-API".to_owned()),
    }
}

/// Source → target of the video and the audio stream. Without a transcode session the
/// original file plays: video and the default audio track as they are.
fn stream_lines(
    streams: &[media::Stream],
    transcode: Option<(&PlaybackPlan, EncoderKind)>,
) -> (Option<StreamLineDto>, Option<StreamLineDto>) {
    let video_stream = transcode
        .and_then(|(plan, _)| plan.video.as_ref())
        .and_then(|v| streams.iter().find(|s| s.idx == v.stream_index))
        .or_else(|| {
            streams
                .iter()
                .find(|s| s.kind == "video" && !matches!(s.codec.as_deref(), Some("mjpeg" | "png")))
        });
    let video = video_stream.map(|s| {
        let source = format!(
            "{} ({})",
            resolution(s.height),
            codec_name(s.codec.as_deref().unwrap_or("?"))
        );
        match transcode.and_then(|(plan, encoder)| plan.video.as_ref().map(|v| (v, encoder))) {
            Some((v, encoder)) if v.action == StreamAction::Transcode => StreamLineDto {
                target: format!("{} ({})", resolution(Some(v.height)), codec_name(&v.codec)),
                source,
                action: "transcode".to_owned(),
                hardware: hardware_name(encoder),
            },
            Some(_) => StreamLineDto {
                target: source.clone(),
                source,
                action: "copy".to_owned(),
                hardware: None,
            },
            None => StreamLineDto {
                target: source.clone(),
                source,
                action: "direct".to_owned(),
                hardware: None,
            },
        }
    });
    let audio_plan = transcode.and_then(|(plan, _)| plan.audio.as_ref());
    let audio_stream = audio_plan
        .and_then(|a| streams.iter().find(|s| s.idx == a.stream_index))
        .or_else(|| {
            let audio = || streams.iter().filter(|s| s.kind == "audio");
            audio().find(|s| s.is_default).or_else(|| audio().next())
        });
    let audio = audio_stream.map(|s| {
        let source = format!(
            "{} {}",
            codec_name(s.codec.as_deref().unwrap_or("?")),
            channels_name(s.channels.unwrap_or(2))
        );
        match audio_plan {
            Some(a) if a.action == StreamAction::Transcode => StreamLineDto {
                target: format!("{} {}", codec_name(&a.codec), channels_name(a.channels)),
                source,
                action: "transcode".to_owned(),
                hardware: None,
            },
            Some(_) => StreamLineDto {
                target: source.clone(),
                source,
                action: "copy".to_owned(),
                hardware: None,
            },
            None => StreamLineDto {
                target: source.clone(),
                source,
                action: "direct".to_owned(),
                hardware: None,
            },
        }
    });
    (video, audio)
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct StatsQuery {
    /// Period in days (1–365, default 30).
    pub days: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TopUserDto {
    pub user_id: i64,
    pub name: String,
    pub plays: i64,
    pub watch_ms: i64,
    pub movies_ms: i64,
    pub tv_ms: i64,
    pub music_ms: i64,
    pub other_ms: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HistoryBucketDto {
    /// Start of the bucket (unix ms).
    pub start: i64,
    pub movies_ms: i64,
    pub tv_ms: i64,
    pub music_ms: i64,
    pub other_ms: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TopItemDto {
    pub item: ItemCardDto,
    pub plays: i64,
    /// How many different users played it.
    pub users: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DashboardStatsDto {
    /// Start and end of the period (unix ms).
    pub since: i64,
    pub until: i64,
    /// Length of one history bucket (a day, a week or 30 days).
    pub bucket_ms: i64,
    pub top_users: Vec<TopUserDto>,
    pub history: Vec<HistoryBucketDto>,
    pub top_movies: Vec<TopItemDto>,
    pub top_series: Vec<TopItemDto>,
    pub top_artists: Vec<TopItemDto>,
}

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Cards for top entries, in their order (entries whose item vanished are dropped).
async fn top_items(state: &AppState, entries: Vec<stats::TopEntry>) -> ApiResult<Vec<TopItemDto>> {
    let ids: Vec<i64> = entries.iter().map(|e| e.id).collect();
    let mut cards: HashMap<i64, ItemCardDto> = browse::summaries(&state.db, &ids, None)
        .await?
        .into_iter()
        .map(|s| (s.id, card(state, s)))
        .collect();
    Ok(entries
        .into_iter()
        .filter_map(|e| {
            cards.remove(&e.id).map(|item| TopItemDto {
                item,
                plays: e.plays,
                users: e.users,
            })
        })
        .collect())
}

/// Viewing statistics: top users, play history, most played (administrators).
#[utoipa::path(
    get,
    path = "/dashboard/stats",
    operation_id = "getDashboardStats",
    tag = "dashboard",
    params(StatsQuery),
    responses(
        (status = OK, description = "Statistics of the period", body = DashboardStatsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn get_dashboard_stats(
    _: AdminUser,
    State(state): State<AppState>,
    Query(query): Query<StatsQuery>,
) -> ApiResult<Json<DashboardStatsDto>> {
    let days = query.days.unwrap_or(30).clamp(1, 365);
    let until = flixnet_db::now_ms();
    let since = until - days * DAY_MS;
    let bucket_ms = match days {
        ..=14 => DAY_MS,
        15..=120 => 7 * DAY_MS,
        _ => 30 * DAY_MS,
    };
    let db = &state.db;
    Ok(Json(DashboardStatsDto {
        since,
        until,
        bucket_ms,
        top_users: stats::top_users(db, since, 8)
            .await?
            .into_iter()
            .map(|u| TopUserDto {
                user_id: u.user_id,
                name: u.name,
                plays: u.plays,
                watch_ms: u.watch_ms,
                movies_ms: u.movies_ms,
                tv_ms: u.tv_ms,
                music_ms: u.music_ms,
                other_ms: u.other_ms,
            })
            .collect(),
        history: stats::history(db, since, until, bucket_ms)
            .await?
            .into_iter()
            .map(|b| HistoryBucketDto {
                start: b.start,
                movies_ms: b.movies_ms,
                tv_ms: b.tv_ms,
                music_ms: b.music_ms,
                other_ms: b.other_ms,
            })
            .collect(),
        top_movies: top_items(&state, stats::top_movies(db, since, 6).await?).await?,
        top_series: top_items(&state, stats::top_series(db, since, 6).await?).await?,
        top_artists: top_items(&state, stats::top_artists(db, since, 6).await?).await?,
    }))
}

#[derive(Debug, Clone, Copy, Default, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BandwidthRange {
    /// Last two minutes, per second.
    #[default]
    Realtime,
    /// Last hour, per minute.
    Hour,
    /// Last 24 hours, per 15 minutes.
    Day,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct BandwidthQuery {
    /// `realtime` (default), `hour` or `day`.
    #[param(inline)]
    pub range: Option<BandwidthRange>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BandwidthPointDto {
    /// End of the slot (unix ms).
    pub at: i64,
    /// Average rate to clients in the home network.
    pub local_bytes_per_second: u64,
    /// Average rate to clients elsewhere.
    pub remote_bytes_per_second: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BandwidthHistoryDto {
    pub slot_seconds: u64,
    pub points: Vec<BandwidthPointDto>,
}

/// Media bandwidth over time, local and remote (administrators).
#[utoipa::path(
    get,
    path = "/dashboard/bandwidth",
    operation_id = "getDashboardBandwidth",
    tag = "dashboard",
    params(BandwidthQuery),
    responses(
        (status = OK, description = "Bandwidth chart", body = BandwidthHistoryDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn get_dashboard_bandwidth(
    _: AdminUser,
    State(state): State<AppState>,
    Query(query): Query<BandwidthQuery>,
) -> Json<BandwidthHistoryDto> {
    let range = match query.range.unwrap_or_default() {
        BandwidthRange::Realtime => Range::RealTime,
        BandwidthRange::Hour => Range::Hour,
        BandwidthRange::Day => Range::Day,
    };
    let (slot_seconds, points) = state.ops.bandwidth.history(range);
    Json(BandwidthHistoryDto {
        slot_seconds,
        points: points
            .into_iter()
            .map(|p| BandwidthPointDto {
                at: p.at,
                local_bytes_per_second: p.local_bytes_per_second,
                remote_bytes_per_second: p.remote_bytes_per_second,
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(idx: i64, kind: &str, codec: &str) -> media::Stream {
        media::Stream {
            idx,
            kind: kind.into(),
            codec: Some(codec.into()),
            ..Default::default()
        }
    }

    #[test]
    fn stream_lines_describe_direct_play_and_transcodes() {
        let streams = vec![
            media::Stream {
                height: Some(1080),
                ..stream(0, "video", "hevc")
            },
            media::Stream {
                channels: Some(6),
                is_default: true,
                ..stream(1, "audio", "ac3")
            },
        ];
        let (video, audio) = stream_lines(&streams, None);
        let (video, audio) = (video.unwrap(), audio.unwrap());
        assert_eq!(
            (video.source.as_str(), video.action.as_str()),
            ("1080p (HEVC)", "direct")
        );
        assert_eq!(
            (audio.source.as_str(), audio.target.as_str()),
            ("AC3 5.1", "AC3 5.1")
        );

        let plan: PlaybackPlan = serde_json::from_value(serde_json::json!({
            "method": "transcode",
            "video": { "stream_index": 0, "action": "transcode", "codec": "h264",
                       "width": 1280, "height": 720, "bitrate": null, "tonemap": false },
            "audio": { "stream_index": 1, "action": "transcode", "codec": "aac", "channels": 2 },
            "subtitle": null,
            "reasons": []
        }))
        .unwrap();
        let (video, audio) = stream_lines(&streams, Some((&plan, EncoderKind::Qsv)));
        let (video, audio) = (video.unwrap(), audio.unwrap());
        assert_eq!(
            (
                video.source.as_str(),
                video.target.as_str(),
                video.action.as_str()
            ),
            ("1080p (HEVC)", "720p (H.264)", "transcode")
        );
        assert_eq!(video.hardware.as_deref(), Some("QuickSync"));
        assert_eq!(
            (audio.target.as_str(), audio.action.as_str()),
            ("AAC Stereo", "transcode")
        );
    }

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
