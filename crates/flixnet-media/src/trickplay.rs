//! Trickplay sprite sheets and chapter images.
//!
//! Trickplay: one thumbnail every [`INTERVAL_MS`], [`TILE_WIDTH`] pixels wide, laid out
//! [`COLUMNS`]×[`ROWS`] per JPEG sheet. A player hovering at time `t` shows tile
//! `floor(t / interval)`: sheet `index / per_sheet`, then row-major within the sheet
//! ([`SpriteLayout::position`]). ffmpeg decodes only keyframes (`-skip_frame nokey`):
//! orders of magnitude faster for long films, and a preview a few seconds off is
//! fine while scrubbing.
//!
//! Chapter images: one frame per chapter, a little after its start (chapters often
//! begin on black), at the same width.
//!
//! Every argument is built from typed values and paths chosen by the server.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio_util::sync::CancellationToken;

use crate::process::command;

/// One thumbnail every 10 s.
pub const INTERVAL_MS: i64 = 10_000;
/// Width of a thumbnail in pixels; the height follows the picture's aspect ratio.
pub const TILE_WIDTH: i64 = 320;
/// Tiles per sheet row and per sheet column.
pub const COLUMNS: i64 = 10;
pub const ROWS: i64 = 10;
/// JPEG quality (`-q:v`, 2 = best … 31 = worst).
const JPEG_QUALITY: &str = "4";

/// How a file's thumbnails are laid out on sheets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteLayout {
    pub interval_ms: i64,
    pub tile_width: i64,
    pub tile_height: i64,
    pub columns: i64,
    pub rows: i64,
    /// Thumbnails in total: one per started interval.
    pub thumbnails: i64,
}

/// Where one thumbnail sits: sheet number and pixel offset of the tile in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TilePosition {
    pub sheet: i64,
    pub x: i64,
    pub y: i64,
}

impl SpriteLayout {
    /// The standard layout for a video of this size and length. `None` without a
    /// usable picture size or duration.
    pub fn new(duration_ms: i64, width: i64, height: i64) -> Option<Self> {
        if duration_ms <= 0 || width <= 0 || height <= 0 {
            return None;
        }
        Some(Self {
            interval_ms: INTERVAL_MS,
            tile_width: TILE_WIDTH,
            tile_height: even_height(TILE_WIDTH, width, height),
            columns: COLUMNS,
            rows: ROWS,
            thumbnails: (duration_ms + INTERVAL_MS - 1) / INTERVAL_MS,
        })
    }

    pub fn per_sheet(&self) -> i64 {
        self.columns * self.rows
    }

    pub fn sheets(&self) -> i64 {
        (self.thumbnails + self.per_sheet() - 1) / self.per_sheet()
    }

    /// The tile shown for a position (clamped to the first and last thumbnail).
    pub fn position(&self, time_ms: i64) -> TilePosition {
        let index = (time_ms.max(0) / self.interval_ms).min(self.thumbnails - 1);
        let within = index % self.per_sheet();
        TilePosition {
            sheet: index / self.per_sheet(),
            x: (within % self.columns) * self.tile_width,
            y: (within / self.columns) * self.tile_height,
        }
    }
}

/// `width`-wide size of a `source_width`×`source_height` picture: height rounded to
/// an even number (what encoders want), at least 2.
fn even_height(width: i64, source_width: i64, source_height: i64) -> i64 {
    let exact = (width * source_height) as f64 / source_width as f64;
    (((exact / 2.0).round() as i64) * 2).max(2)
}

/// The picture a thumbnail is taken from.
#[derive(Debug, Clone, Copy)]
pub struct VideoSource<'a> {
    pub input: &'a Path,
    /// Stream index of the video within the file.
    pub stream_index: i64,
    /// HDR (PQ/HLG) pictures are tone-mapped to SDR, so previews are not washed out.
    pub tonemap: bool,
}

/// Scale (and tone-map) to the tile size; JPEG wants full-range 8-bit.
fn picture_filters(width: i64, height: i64, tonemap: bool) -> String {
    let mut filters = format!("scale={width}:{height}");
    if tonemap {
        filters.push(',');
        filters.push_str(crate::hls::TONEMAP);
    }
    filters.push_str(",setsar=1");
    filters
}

fn base_args() -> Vec<OsString> {
    ["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]
        .into_iter()
        .map(OsString::from)
        .collect()
}

/// Sprite sheets `0.jpg`, `1.jpg`, … into `out_dir`.
pub fn sprite_args(
    source: &VideoSource<'_>,
    layout: &SpriteLayout,
    out_dir: &Path,
) -> Vec<OsString> {
    let mut args = base_args();
    // Keyframes only (see the module docs); two decoder threads keep it a background job.
    args.extend(["-skip_frame", "nokey", "-threads", "2", "-i"].map(OsString::from));
    args.push(source.input.as_os_str().to_owned());
    let fps = format!("fps=1000/{}:eof_action=pass", layout.interval_ms);
    let filters = format!(
        "{fps},{},tile={}x{}",
        picture_filters(layout.tile_width, layout.tile_height, source.tonemap),
        layout.columns,
        layout.rows
    );
    args.extend(
        [
            "-map",
            &format!("0:{}", source.stream_index),
            "-an",
            "-sn",
            "-dn",
            "-vf",
            &filters,
            "-q:v",
            JPEG_QUALITY,
            "-f",
            "image2",
            "-start_number",
            "0",
        ]
        .map(OsString::from),
    );
    args.push(out_dir.join("%d.jpg").into_os_string());
    args
}

/// One frame at `time_ms`, `width` pixels wide (`height` keeps the aspect ratio).
pub fn frame_args(
    source: &VideoSource<'_>,
    time_ms: i64,
    width: i64,
    height: i64,
    output: &Path,
) -> Vec<OsString> {
    let mut args = base_args();
    // Input seeking: fast (from the keyframe before) and exact (decodes up to `time`).
    args.extend(["-ss", &seconds(time_ms.max(0)), "-i"].map(OsString::from));
    args.push(source.input.as_os_str().to_owned());
    args.extend(
        [
            "-map",
            &format!("0:{}", source.stream_index),
            "-an",
            "-sn",
            "-dn",
            "-frames:v",
            "1",
            "-vf",
            &picture_filters(width, height, source.tonemap),
            "-q:v",
            JPEG_QUALITY,
            "-f",
            "image2",
            "-update",
            "1",
        ]
        .map(OsString::from),
    );
    args.push(output.as_os_str().to_owned());
    args
}

/// `12345` ms → `12.345`.
fn seconds(ms: i64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

/// Where in a chapter its image is taken: a little after the start (chapters often
/// begin on black), at most 3 s in and never past the chapter's middle.
pub fn chapter_image_time(start_ms: i64, end_ms: i64) -> i64 {
    let length = (end_ms - start_ms).max(0);
    start_ms + (length / 10).min(3_000)
}

/// Size of chapter images for a picture of this size.
pub fn chapter_image_size(width: i64, height: i64) -> (i64, i64) {
    (
        TILE_WIDTH,
        even_height(TILE_WIDTH, width.max(1), height.max(1)),
    )
}

/// Cache layout under `<config_dir>/cache`.
pub fn sheet_dir(cache_dir: &Path, file_id: i64) -> PathBuf {
    cache_dir.join("trickplay").join(file_id.to_string())
}

pub fn sheet_path(cache_dir: &Path, file_id: i64, sheet: i64) -> PathBuf {
    sheet_dir(cache_dir, file_id).join(format!("{sheet}.jpg"))
}

pub fn chapter_dir(cache_dir: &Path, file_id: i64) -> PathBuf {
    cache_dir.join("chapters").join(file_id.to_string())
}

pub fn chapter_image_path(cache_dir: &Path, file_id: i64, chapter: i64) -> PathBuf {
    chapter_dir(cache_dir, file_id).join(format!("{chapter}.jpg"))
}

#[derive(Debug, thiserror::Error)]
pub enum ThumbnailError {
    #[error("ffmpeg could not be started: {0}")]
    Spawn(std::io::Error),
    #[error("ffmpeg failed: {0}")]
    Failed(String),
    #[error("cancelled")]
    Cancelled,
    #[error("file system: {0}")]
    Io(#[from] std::io::Error),
}

/// Runs ffmpeg to completion; killed (and an error) when `cancel` fires.
async fn run(
    ffmpeg: &Path,
    args: Vec<OsString>,
    cancel: &CancellationToken,
) -> Result<(), ThumbnailError> {
    let child = command(ffmpeg)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(ThumbnailError::Spawn)?;
    tokio::select! {
        output = child.wait_with_output() => {
            let output = output.map_err(ThumbnailError::Spawn)?;
            if output.status.success() {
                Ok(())
            } else {
                let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                Err(ThumbnailError::Failed(if message.is_empty() {
                    output.status.to_string()
                } else {
                    message
                }))
            }
        }
        // Dropping the child kills ffmpeg (`kill_on_drop`).
        () = cancel.cancelled() => Err(ThumbnailError::Cancelled),
    }
}

/// Renders the sheets of `source` into `<cache>/trickplay/{file_id}` and returns how
/// many there are. Works in a scratch directory that replaces the old sheets only
/// when complete, so players never see half-written sets. HDR pictures fall back to
/// no tone mapping when this ffmpeg cannot (no zimg).
pub async fn generate_sprites(
    ffmpeg: &Path,
    source: &VideoSource<'_>,
    layout: &SpriteLayout,
    cache_dir: &Path,
    file_id: i64,
    cancel: &CancellationToken,
) -> Result<i64, ThumbnailError> {
    let target = sheet_dir(cache_dir, file_id);
    let scratch = cache_dir
        .join("trickplay")
        .join(format!("{file_id}.partial"));
    let _ = tokio::fs::remove_dir_all(&scratch).await;
    tokio::fs::create_dir_all(&scratch).await?;
    let result = async {
        match run(ffmpeg, sprite_args(source, layout, &scratch), cancel).await {
            Err(ThumbnailError::Failed(err)) if source.tonemap => {
                tracing::debug!(%err, "tone-mapped thumbnails failed; trying without");
                let plain = VideoSource {
                    tonemap: false,
                    ..*source
                };
                run(ffmpeg, sprite_args(&plain, layout, &scratch), cancel).await
            }
            other => other,
        }?;
        let mut sheets = 0;
        while tokio::fs::try_exists(scratch.join(format!("{sheets}.jpg"))).await? {
            sheets += 1;
        }
        if sheets == 0 {
            return Err(ThumbnailError::Failed(
                "no thumbnails were written".to_owned(),
            ));
        }
        let _ = tokio::fs::remove_dir_all(&target).await;
        tokio::fs::rename(&scratch, &target).await?;
        Ok(sheets)
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_dir_all(&scratch).await;
    }
    result
}

/// Renders one image per chapter start (`(index, start_ms, end_ms)`) into
/// `<cache>/chapters/{file_id}`; returns the indexes that got an image. A chapter
/// whose frame cannot be decoded is skipped rather than failing the others.
pub async fn generate_chapter_images(
    ffmpeg: &Path,
    source: &VideoSource<'_>,
    size: (i64, i64),
    chapters: &[(i64, i64, i64)],
    cache_dir: &Path,
    file_id: i64,
    cancel: &CancellationToken,
) -> Result<Vec<i64>, ThumbnailError> {
    let dir = chapter_dir(cache_dir, file_id);
    tokio::fs::create_dir_all(&dir).await?;
    let mut done = Vec::new();
    for &(index, start_ms, end_ms) in chapters {
        let output = chapter_image_path(cache_dir, file_id, index);
        let partial = dir.join(format!("{index}.partial.jpg"));
        let time = chapter_image_time(start_ms, end_ms);
        let mut result = run(
            ffmpeg,
            frame_args(source, time, size.0, size.1, &partial),
            cancel,
        )
        .await;
        if source.tonemap && matches!(result, Err(ThumbnailError::Failed(_))) {
            let plain = VideoSource {
                tonemap: false,
                ..*source
            };
            result = run(
                ffmpeg,
                frame_args(&plain, time, size.0, size.1, &partial),
                cancel,
            )
            .await;
        }
        match result {
            Ok(()) if tokio::fs::try_exists(&partial).await? => {
                tokio::fs::rename(&partial, &output).await?;
                done.push(index);
            }
            Ok(()) | Err(ThumbnailError::Failed(_)) => {
                tracing::debug!(file = file_id, chapter = index, "no chapter image");
                let _ = tokio::fs::remove_file(&partial).await;
            }
            Err(err) => {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(err);
            }
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(args: &[OsString]) -> String {
        args.iter()
            .map(|a| a.to_string_lossy().replace('\\', "/"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn layout_of_a_film() {
        // 2 h 1080p: 720 thumbnails, 320×180, 8 sheets (the last one partly filled).
        let layout = SpriteLayout::new(7_200_000, 1920, 1080).unwrap();
        assert_eq!(
            (layout.tile_width, layout.tile_height, layout.thumbnails),
            (320, 180, 720)
        );
        assert_eq!(layout.per_sheet(), 100);
        assert_eq!(layout.sheets(), 8);
        // A started interval gets its own thumbnail.
        assert_eq!(SpriteLayout::new(65_000, 640, 360).unwrap().thumbnails, 7);
        assert_eq!(SpriteLayout::new(10_000, 640, 360).unwrap().thumbnails, 1);
        // Cinemascope and odd sizes: even heights.
        assert_eq!(SpriteLayout::new(1, 1920, 800).unwrap().tile_height, 134);
        assert_eq!(SpriteLayout::new(1, 720, 576).unwrap().tile_height, 256);
        assert_eq!(SpriteLayout::new(1, 4000, 10).unwrap().tile_height, 2);
        assert!(SpriteLayout::new(0, 1920, 1080).is_none());
        assert!(SpriteLayout::new(1000, 0, 1080).is_none());
    }

    #[test]
    fn sprite_math_finds_the_tile_of_a_position() {
        let layout = SpriteLayout::new(7_200_000, 1920, 1080).unwrap();
        let at = |ms| layout.position(ms);
        assert_eq!(
            at(0),
            TilePosition {
                sheet: 0,
                x: 0,
                y: 0
            }
        );
        assert_eq!(
            at(9_999),
            TilePosition {
                sheet: 0,
                x: 0,
                y: 0
            }
        );
        // Thumbnail 13: row 1, column 3.
        assert_eq!(
            at(135_000),
            TilePosition {
                sheet: 0,
                x: 960,
                y: 180
            }
        );
        // Thumbnail 99 is the last of sheet 0, 100 the first of sheet 1.
        assert_eq!(
            at(995_000),
            TilePosition {
                sheet: 0,
                x: 2880,
                y: 1620
            }
        );
        assert_eq!(
            at(1_000_000),
            TilePosition {
                sheet: 1,
                x: 0,
                y: 0
            }
        );
        // Past the end and before the start: clamped.
        assert_eq!(
            at(99_999_999),
            TilePosition {
                sheet: 7,
                x: 2880,
                y: 180
            }
        );
        assert_eq!(at(-5), at(0));
    }

    #[test]
    fn sprite_arguments() {
        let layout = SpriteLayout::new(60_000, 1280, 720).unwrap();
        let source = VideoSource {
            input: Path::new("/media/Film (2020)/film.mkv"),
            stream_index: 1,
            tonemap: false,
        };
        let args = sprite_args(
            &source,
            &layout,
            Path::new("/config/cache/trickplay/7.partial"),
        );
        assert_eq!(
            joined(&args),
            "-hide_banner -loglevel error -nostdin -y -skip_frame nokey -threads 2 \
             -i /media/Film (2020)/film.mkv -map 0:1 -an -sn -dn \
             -vf fps=1000/10000:eof_action=pass,scale=320:180,setsar=1,tile=10x10 \
             -q:v 4 -f image2 -start_number 0 /config/cache/trickplay/7.partial/%d.jpg"
        );
        // The path stays one argument, spaces and all.
        assert!(args.iter().any(|a| a == "/media/Film (2020)/film.mkv"));

        let hdr = joined(&sprite_args(
            &VideoSource {
                tonemap: true,
                ..source
            },
            &layout,
            Path::new("out"),
        ));
        assert!(hdr.contains("scale=320:180,zscale=t=linear"), "{hdr}");
        assert!(hdr.contains("format=yuv420p,setsar=1,tile=10x10"), "{hdr}");
    }

    #[test]
    fn chapter_image_arguments() {
        let source = VideoSource {
            input: Path::new("in.mkv"),
            stream_index: 0,
            tonemap: false,
        };
        let args = joined(&frame_args(
            &source,
            61_500,
            320,
            134,
            Path::new("out/3.jpg"),
        ));
        assert_eq!(
            args,
            "-hide_banner -loglevel error -nostdin -y -ss 61.500 -i in.mkv -map 0:0 -an -sn -dn \
             -frames:v 1 -vf scale=320:134,setsar=1 -q:v 4 -f image2 -update 1 out/3.jpg"
        );
        // A bit into the chapter: 10 % of it, at most 3 s.
        assert_eq!(chapter_image_time(0, 10_000), 1_000);
        assert_eq!(chapter_image_time(60_000, 600_000), 63_000);
        assert_eq!(chapter_image_time(5_000, 5_000), 5_000);
        assert_eq!(chapter_image_size(1920, 1080), (320, 180));
    }

    #[test]
    fn cache_paths() {
        let cache = Path::new("/config/cache");
        assert_eq!(
            sheet_path(cache, 7, 2),
            Path::new("/config/cache/trickplay/7/2.jpg")
        );
        assert_eq!(
            chapter_image_path(cache, 7, 0),
            Path::new("/config/cache/chapters/7/0.jpg")
        );
    }
}
