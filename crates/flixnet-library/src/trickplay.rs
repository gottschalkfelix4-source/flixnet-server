//! Background jobs for trickplay sheets and chapter images (cpu pool, low priority).
//!
//! Both are queued after a file was probed and by a daily backfill. ffmpeg runs
//! outside any transaction; only the finished result is written (one short write).

use std::path::PathBuf;
use std::time::Duration;

use flixnet_core::jobs::{JobContext, JobHandler, JobOptions, JobQueue, Pool, priority};
use flixnet_core::scheduler::TaskDef;
use flixnet_db::{Db, DbError, media, trickplay};
use flixnet_media::trickplay::{
    SpriteLayout, VideoSource, chapter_image_size, generate_chapter_images, generate_sprites,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePayload {
    pub file_id: i64,
}

/// Embedded cover art shows up as a video stream; it is not the picture.
const COVER_CODECS: &[&str] = &["mjpeg", "png", "bmp", "gif", "webp"];

/// The file's main video stream, if it is a present main file.
async fn video_of(
    db: &Db,
    file_id: i64,
) -> anyhow::Result<Option<(media::MediaFile, media::Stream)>> {
    let Some(file) = media::get_file(db, file_id).await? else {
        return Ok(None); // deleted meanwhile
    };
    if file.missing_since.is_some() || file.role != "main" || file.probed_at.is_none() {
        return Ok(None);
    }
    let video = media::streams(db, file_id).await?.into_iter().find(|s| {
        s.kind == "video" && !COVER_CODECS.contains(&s.codec.as_deref().unwrap_or_default())
    });
    Ok(video.map(|v| (file, v)))
}

fn source<'a>(file: &'a media::MediaFile, video: &media::Stream) -> VideoSource<'a> {
    VideoSource {
        input: std::path::Path::new(&file.path),
        stream_index: video.idx,
        tonemap: video.hdr.as_deref().is_some_and(|h| h != "sdr"),
    }
}

// ---- Trickplay ----------------------------------------------------------------------

pub struct GenerateTrickplay {
    pub ffmpeg: PathBuf,
    /// `<config_dir>/cache`.
    pub cache_dir: PathBuf,
}

impl JobHandler for GenerateTrickplay {
    const KIND: &'static str = "media.trickplay";
    const POOL: Pool = Pool::Cpu;
    type Payload = FilePayload;

    async fn run(&self, ctx: JobContext, payload: FilePayload) -> anyhow::Result<()> {
        let db = ctx.db();
        let Some((file, video)) = video_of(db, payload.file_id).await? else {
            return Ok(());
        };
        if trickplay::get(db, file.id).await?.is_some() {
            return Ok(()); // made since the job was queued
        }
        let Some(layout) = SpriteLayout::new(
            file.duration_ms.unwrap_or(0),
            video.width.unwrap_or(0),
            video.height.unwrap_or(0),
        ) else {
            return Ok(());
        };
        ctx.progress(0.0).await;
        let sheets = generate_sprites(
            &self.ffmpeg,
            &source(&file, &video),
            &layout,
            &self.cache_dir,
            file.id,
            &ctx.cancel_token(),
        )
        .await?;
        trickplay::store(
            db,
            &trickplay::Trickplay {
                file_id: file.id,
                interval_ms: layout.interval_ms,
                tile_width: layout.tile_width,
                tile_height: layout.tile_height,
                tile_columns: layout.columns,
                tile_rows: layout.rows,
                // ffmpeg may end a tile early (e.g. a last frame it drops); never point
                // past what the sheets hold.
                thumbnails: layout.thumbnails.min(sheets * layout.per_sheet()),
                sheets,
                created_at: 0,
            },
        )
        .await?;
        Ok(())
    }
}

// ---- Chapter images -----------------------------------------------------------------

pub struct GenerateChapterImages {
    pub ffmpeg: PathBuf,
    /// `<config_dir>/cache`.
    pub cache_dir: PathBuf,
}

impl JobHandler for GenerateChapterImages {
    const KIND: &'static str = "media.chapter_images";
    const POOL: Pool = Pool::Cpu;
    type Payload = FilePayload;

    async fn run(&self, ctx: JobContext, payload: FilePayload) -> anyhow::Result<()> {
        let db = ctx.db();
        let Some((file, video)) = video_of(db, payload.file_id).await? else {
            return Ok(());
        };
        let missing: Vec<(i64, i64, i64)> = trickplay::chapters(db, file.id)
            .await?
            .into_iter()
            .filter(|c| c.image_at.is_none())
            .map(|c| (c.idx, c.start_ms, c.end_ms))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let size = chapter_image_size(video.width.unwrap_or(16), video.height.unwrap_or(9));
        let done = generate_chapter_images(
            &self.ffmpeg,
            &source(&file, &video),
            size,
            &missing,
            &self.cache_dir,
            file.id,
            &ctx.cancel_token(),
        )
        .await?;
        trickplay::mark_chapter_images(db, file.id, &done).await?;
        Ok(())
    }
}

/// Queues sheets and chapter images of a file (deduplicated while pending).
pub async fn queue_thumbnails(queue: &JobQueue, file_id: i64, prio: i64) -> Result<(), DbError> {
    queue
        .enqueue::<GenerateTrickplay>(
            &FilePayload { file_id },
            JobOptions::priority(prio).dedupe(format!("trickplay:{file_id}")),
        )
        .await?;
    queue
        .enqueue::<GenerateChapterImages>(
            &FilePayload { file_id },
            JobOptions::priority(prio).dedupe(format!("chapter-images:{file_id}")),
        )
        .await?;
    Ok(())
}

// ---- Backfill (scheduled) -----------------------------------------------------------

/// Files probed before thumbnails existed, or whose jobs failed for good earlier.
pub struct BackfillThumbnails;

/// Files queued per backfill run; the next run picks up the rest.
const BACKFILL_BATCH: i64 = 500;

impl JobHandler for BackfillThumbnails {
    const KIND: &'static str = "media.thumbnails_backfill";
    const POOL: Pool = Pool::Io;
    type Payload = ();

    async fn run(&self, ctx: JobContext, _: ()) -> anyhow::Result<()> {
        for file_id in trickplay::files_without_trickplay(ctx.db(), BACKFILL_BATCH).await? {
            ctx.queue()
                .enqueue::<GenerateTrickplay>(
                    &FilePayload { file_id },
                    JobOptions::priority(priority::BACKFILL).dedupe(format!("trickplay:{file_id}")),
                )
                .await?;
        }
        for file_id in trickplay::files_without_chapter_images(ctx.db(), BACKFILL_BATCH).await? {
            ctx.queue()
                .enqueue::<GenerateChapterImages>(
                    &FilePayload { file_id },
                    JobOptions::priority(priority::BACKFILL)
                        .dedupe(format!("chapter-images:{file_id}")),
                )
                .await?;
        }
        Ok(())
    }
}

pub fn tasks() -> Vec<TaskDef> {
    vec![TaskDef::new::<BackfillThumbnails>(
        "thumbnails",
        "Fehlende Vorschaubilder (Trickplay) und Kapitelbilder erzeugen",
        Duration::from_secs(24 * 60 * 60),
        &(),
    )]
}
