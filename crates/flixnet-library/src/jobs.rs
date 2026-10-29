//! Background jobs of the library: scans and probes.

use std::path::Path;
use std::time::Duration;

use flixnet_core::events::ServerEvent;
use flixnet_core::jobs::{JobContext, JobHandler, JobOptions, JobQueue, Pool, priority};
use flixnet_core::scheduler::TaskDef;
use flixnet_db::{Db, DbError, libraries, media};
use flixnet_media::{Ffprobe, MediaInfo};
use serde::{Deserialize, Serialize};

use crate::parse::{is_subtitle_file, subtitle_info};
use crate::scan::scan_library;

// ---- Scan one library -------------------------------------------------------------

pub struct ScanLibrary;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanPayload {
    pub library_id: i64,
}

impl JobHandler for ScanLibrary {
    const KIND: &'static str = "library.scan";
    const POOL: Pool = Pool::Io;
    type Payload = ScanPayload;

    async fn run(&self, ctx: JobContext, payload: ScanPayload) -> anyhow::Result<()> {
        // The library may have been deleted after the job was queued.
        if libraries::get(ctx.db(), payload.library_id)
            .await?
            .is_none()
        {
            return Ok(());
        }
        let report = scan_library(ctx.db(), ctx.queue(), payload.library_id).await?;
        if report.added + report.changed + report.missing + report.restored > 0 {
            ctx.events().publish(ServerEvent::LibraryChanged {
                library_id: payload.library_id,
            });
        }
        Ok(())
    }
}

/// Queues a scan of one library (deduplicated: never two scans of the same library).
pub async fn queue_scan(
    queue: &JobQueue,
    library_id: i64,
    prio: i64,
) -> Result<Option<i64>, DbError> {
    queue
        .enqueue::<ScanLibrary>(
            &ScanPayload { library_id },
            JobOptions::priority(prio).dedupe(format!("scan:{library_id}")),
        )
        .await
}

// ---- Scan all libraries (scheduled) -------------------------------------------------

pub struct ScanAllLibraries;

impl JobHandler for ScanAllLibraries {
    const KIND: &'static str = "library.scan_all";
    const POOL: Pool = Pool::Io;
    type Payload = ();

    async fn run(&self, ctx: JobContext, _: ()) -> anyhow::Result<()> {
        for library in libraries::list(ctx.db()).await? {
            queue_scan(ctx.queue(), library.id, priority::BACKFILL).await?;
        }
        Ok(())
    }
}

/// Periodic rescan as a safety net: file watchers miss events on network shares
/// and Unraid's user shares.
pub fn tasks() -> Vec<TaskDef> {
    vec![TaskDef::new::<ScanAllLibraries>(
        "scan_libraries",
        "Alle Bibliotheken auf neue, geänderte und gelöschte Dateien prüfen",
        Duration::from_secs(6 * 60 * 60),
        &(),
    )]
}

// ---- Metadata refresh -------------------------------------------------------------

pub struct RefreshMetadata;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshPayload {
    pub item_id: i64,
}

impl JobHandler for RefreshMetadata {
    const KIND: &'static str = "metadata.refresh";
    const POOL: Pool = Pool::Io;
    type Payload = RefreshPayload;

    async fn run(&self, ctx: JobContext, payload: RefreshPayload) -> anyhow::Result<()> {
        let Some(item) = flixnet_db::items::get(ctx.db(), payload.item_id).await? else {
            return Ok(()); // removed meanwhile
        };
        crate::refresh::refresh_local(ctx.db(), item.id).await?;
        // Movies and series are matched online; seasons/episodes follow their series.
        if matches!(item.kind.as_str(), "movie" | "series")
            && let Some(tmdb) = crate::providers::tmdb_from_settings(ctx.db()).await?
        {
            crate::providers::refresh_remote(ctx.db(), &tmdb, item.id).await?;
        }
        // Provider ids may have changed: user data follows the new identity.
        flixnet_db::userdata::update_identity(ctx.db(), item.id).await?;
        Ok(())
    }
}

/// Queues a metadata refresh for an item (deduplicated while pending).
pub async fn queue_refresh(
    queue: &JobQueue,
    item_id: i64,
    prio: i64,
) -> Result<Option<i64>, DbError> {
    queue
        .enqueue::<RefreshMetadata>(
            &RefreshPayload { item_id },
            JobOptions::priority(prio).dedupe(format!("refresh:{item_id}")),
        )
        .await
}

// ---- Probe one file ---------------------------------------------------------------

pub struct ProbeFile {
    pub ffprobe: Ffprobe,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbePayload {
    pub file_id: i64,
}

impl JobHandler for ProbeFile {
    const KIND: &'static str = "media.probe";
    const POOL: Pool = Pool::Probe;
    type Payload = ProbePayload;

    async fn run(&self, ctx: JobContext, payload: ProbePayload) -> anyhow::Result<()> {
        probe_file(ctx.db(), &self.ffprobe, payload.file_id).await
    }
}

/// Probes a file and stores streams, chapters and sidecar subtitles.
pub async fn probe_file(db: &Db, ffprobe: &Ffprobe, file_id: i64) -> anyhow::Result<()> {
    let Some(file) = media::get_file(db, file_id).await? else {
        return Ok(()); // deleted meanwhile
    };
    if file.missing_since.is_some() {
        return Ok(());
    }
    let path = Path::new(&file.path);
    let info = match ffprobe.probe(path).await {
        Ok(info) => info,
        Err(err) => {
            media::store_probe_error(db, file_id, &err.to_string()).await?;
            return Err(err.into());
        }
    };
    let mut data = to_probe_data(&info);
    data.streams.extend(sidecar_subtitles(path).await);
    media::store_probe(db, file_id, &data).await?;
    Ok(())
}

fn to_probe_data(info: &MediaInfo) -> media::ProbeData {
    media::ProbeData {
        container: info.container.clone(),
        duration_ms: info.duration_ms,
        bitrate: info.bitrate,
        streams: info
            .streams
            .iter()
            .map(|s| media::Stream {
                idx: s.index,
                kind: s.kind.as_str().to_owned(),
                codec: s.codec.clone(),
                profile: s.profile.clone(),
                level: s.level,
                width: s.width,
                height: s.height,
                fps: s.fps,
                bit_depth: s.bit_depth,
                pix_fmt: s.pix_fmt.clone(),
                color_transfer: s.color_transfer.clone(),
                color_primaries: s.color_primaries.clone(),
                hdr: s.hdr.map(str::to_owned),
                channels: s.channels,
                channel_layout: s.channel_layout.clone(),
                sample_rate: s.sample_rate,
                bitrate: s.bitrate,
                language: s.language.clone(),
                title: s.title.clone(),
                is_default: s.is_default,
                is_forced: s.is_forced,
                is_sdh: s.is_sdh,
                is_external: false,
                external_path: None,
            })
            .collect(),
        chapters: info
            .chapters
            .iter()
            .enumerate()
            .map(|(i, c)| media::Chapter {
                idx: i as i64,
                start_ms: c.start_ms,
                end_ms: c.end_ms,
                title: c.title.clone(),
            })
            .collect(),
    }
}

/// Subtitle files next to the video whose name starts with the video's name
/// (`Movie.de.forced.srt`). They become external streams numbered from 1000.
async fn sidecar_subtitles(video: &Path) -> Vec<media::Stream> {
    let (Some(dir), Some(stem)) = (video.parent(), video.file_stem()) else {
        return Vec::new();
    };
    let stem = stem.to_string_lossy().into_owned();
    let mut found: Vec<(std::path::PathBuf, Option<String>, bool, bool)> = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return Vec::new();
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !is_subtitle_file(&path) {
            continue;
        }
        if let Some((language, forced, sdh)) = subtitle_info(&stem, &path) {
            found.push((path, language, forced, sdh));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
        .into_iter()
        .enumerate()
        .map(|(i, (path, language, forced, sdh))| media::Stream {
            idx: 1000 + i as i64,
            kind: "subtitle".to_owned(),
            codec: path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase()),
            language,
            is_forced: forced,
            is_sdh: sdh,
            is_external: true,
            external_path: Some(path.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .collect()
}
