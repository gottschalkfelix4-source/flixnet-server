//! Photo libraries: pictures and short clips, grouped into albums by folder.
//!
//! The scan reads EXIF (date, camera, exposure, GPS) and the picture size directly
//! (no ffprobe), outside any transaction, then writes in batches. The picture file
//! itself is the item's image, so the image cache makes the thumbnails. Clips are
//! probed in a background job that also takes a frame as their thumbnail.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::Context;
use exif::{In, Reader, Tag, Value};
use flixnet_core::jobs::{JobContext, JobHandler, JobOptions, JobQueue, Pool, priority};
use flixnet_db::photos::{self, PhotoMeta};
use flixnet_db::{Db, DbError, items, libraries, media};
use flixnet_media::Ffprobe;
use flixnet_media::trickplay::{VideoSource, chapter_image_path, generate_chapter_images};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::assets::{self, extension};
use crate::scan::{FoundFile, MISSING_GRACE_MS, ScanReport, sort_title};

/// Picture formats the image cache can decode (HEIC needs a decoder we do not ship).
pub const PHOTO_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif"];
/// Rows written per transaction.
const BATCH: usize = 200;

pub fn is_photo_file(path: &Path) -> bool {
    extension(path).is_some_and(|e| PHOTO_EXTENSIONS.contains(&e.as_str()))
}

/// Pictures and video clips.
pub fn is_photo_library_file(path: &Path) -> bool {
    is_photo_file(path) || crate::parse::is_video_file(path)
}

// ---- EXIF -------------------------------------------------------------------------

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Unix ms of a wall-clock time with an optional UTC offset in minutes (no offset:
/// read as UTC, so the timeline shows the camera's local time).
fn unix_ms(date: (i64, i64, i64), time: (i64, i64, i64), offset_minutes: i64) -> Option<i64> {
    let (year, month, day) = date;
    let (hour, minute, second) = time;
    if !(1900..=2200).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second - offset_minutes * 60;
    Some(secs * 1000)
}

/// `2023-07-14T10:20:30.000000Z` (ffprobe's `creation_time`) → unix ms.
pub fn parse_iso_datetime(text: &str) -> Option<i64> {
    let text = text.trim();
    let date = text.get(..10)?;
    let time = text.get(11..19)?;
    let part = |s: &str, range: std::ops::Range<usize>| s.get(range)?.parse::<i64>().ok();
    let ms = unix_ms(
        (part(date, 0..4)?, part(date, 5..7)?, part(date, 8..10)?),
        (part(time, 0..2)?, part(time, 3..5)?, part(time, 6..8)?),
        0,
    )?;
    // 1970-01-01 is what cameras without a clock write.
    (ms > 86_400_000).then_some(ms)
}

fn ascii(field: &exif::Field) -> Option<String> {
    match &field.value {
        Value::Ascii(parts) => parts
            .first()
            .map(|p| {
                String::from_utf8_lossy(p)
                    .trim()
                    .trim_end_matches('\0')
                    .to_owned()
            })
            .filter(|s| !s.is_empty()),
        _ => None,
    }
}

fn rational(field: &exif::Field, index: usize) -> Option<f64> {
    match &field.value {
        Value::Rational(values) => values
            .get(index)
            .filter(|r| r.denom != 0)
            .map(|r| r.to_f64()),
        Value::SRational(values) => values
            .get(index)
            .filter(|r| r.denom != 0)
            .map(|r| r.to_f64()),
        _ => None,
    }
}

/// Degrees from the three GPS rationals (degrees, minutes, seconds) and the reference.
fn gps(exif: &exif::Exif, value: Tag, reference: Tag, negative: &str) -> Option<f64> {
    let field = exif.get_field(value, In::PRIMARY)?;
    let degrees = rational(field, 0)?
        + rational(field, 1).unwrap_or(0.0) / 60.0
        + rational(field, 2).unwrap_or(0.0) / 3600.0;
    let sign = match exif.get_field(reference, In::PRIMARY).and_then(ascii) {
        Some(r) if r.eq_ignore_ascii_case(negative) => -1.0,
        _ => 1.0,
    };
    let degrees = sign * degrees;
    (degrees.is_finite() && degrees.abs() <= 180.0).then_some((degrees * 1e6).round() / 1e6)
}

/// "1/250" for short exposures, "2.5" seconds for long ones.
fn exposure_text(seconds: f64) -> Option<String> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    Some(if seconds < 1.0 {
        format!("1/{}", (1.0 / seconds).round())
    } else {
        let rounded = (seconds * 10.0).round() / 10.0;
        format!("{rounded}")
    })
}

/// What EXIF says about a picture (`None`: no readable EXIF). `taken_at` is 0 when
/// EXIF has no date.
pub fn read_exif(bytes: &[u8]) -> Option<PhotoMeta> {
    let exif = Reader::new()
        .read_from_container(&mut Cursor::new(bytes))
        .ok()?;
    let field = |tag| exif.get_field(tag, In::PRIMARY);
    let taken_at = [Tag::DateTimeOriginal, Tag::DateTimeDigitized, Tag::DateTime]
        .into_iter()
        .zip([
            Tag::OffsetTimeOriginal,
            Tag::OffsetTimeDigitized,
            Tag::OffsetTime,
        ])
        .find_map(|(tag, offset_tag)| {
            let Value::Ascii(parts) = &field(tag)?.value else {
                return None;
            };
            let mut dt = exif::DateTime::from_ascii(parts.first()?).ok()?;
            if let Some(Value::Ascii(offset)) = field(offset_tag).map(|f| &f.value)
                && let Some(offset) = offset.first()
            {
                let _ = dt.parse_offset(offset);
            }
            unix_ms(
                (dt.year.into(), dt.month.into(), dt.day.into()),
                (dt.hour.into(), dt.minute.into(), dt.second.into()),
                dt.offset.map_or(0, i64::from),
            )
        })
        .unwrap_or(0);
    let make = field(Tag::Make).and_then(ascii);
    let model = field(Tag::Model).and_then(ascii);
    let uint = |tag| field(tag).and_then(|f| f.value.get_uint(0)).map(i64::from);
    Some(PhotoMeta {
        taken_at,
        width: uint(Tag::PixelXDimension),
        height: uint(Tag::PixelYDimension),
        orientation: uint(Tag::Orientation).filter(|o| (1..=8).contains(o)),
        camera_make: make,
        camera_model: model,
        lens: field(Tag::LensModel).and_then(ascii),
        f_number: field(Tag::FNumber)
            .and_then(|f| rational(f, 0))
            .map(|f| (f * 10.0).round() / 10.0),
        exposure: field(Tag::ExposureTime)
            .and_then(|f| rational(f, 0))
            .and_then(exposure_text),
        iso: uint(Tag::PhotographicSensitivity),
        focal_length: field(Tag::FocalLength)
            .and_then(|f| rational(f, 0))
            .map(|f| (f * 10.0).round() / 10.0),
        latitude: gps(&exif, Tag::GPSLatitude, Tag::GPSLatitudeRef, "S"),
        longitude: gps(&exif, Tag::GPSLongitude, Tag::GPSLongitudeRef, "W"),
    })
}

/// EXIF plus the decoded size of a picture file (blocking; run off the runtime). The
/// date falls back to the modification time; an unreadable picture is an error.
pub fn picture_meta(path: &Path, mtime_ms: i64) -> anyhow::Result<PhotoMeta> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut meta = read_exif(&bytes).unwrap_or_default();
    let (width, height) = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()?
        .into_dimensions()
        .context("not a readable picture")?;
    // Rotated pictures (orientation 5–8) show with swapped sides.
    let rotated = meta.orientation.is_some_and(|o| o >= 5);
    let (width, height) = if rotated {
        (height, width)
    } else {
        (width, height)
    };
    meta.width = Some(i64::from(width));
    meta.height = Some(i64::from(height));
    if meta.taken_at <= 0 {
        meta.taken_at = mtime_ms;
    }
    Ok(meta)
}

// ---- Scan -------------------------------------------------------------------------

/// The folder a file is in, relative to the library folder (`None`: the root).
fn album_of(relative: &Path) -> Option<String> {
    let parent = relative.parent()?;
    let text = parent.to_string_lossy().replace('\\', "/");
    (!text.is_empty()).then_some(text)
}

fn title_of(relative: &Path) -> String {
    relative
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// What the scan read from one new or changed file (computed before writing).
struct Read {
    file: FoundFile,
    meta: PhotoMeta,
    error: Option<String>,
    is_clip: bool,
}

fn read_file(file: FoundFile) -> Read {
    let is_clip = !is_photo_file(&file.path);
    if is_clip {
        let meta = PhotoMeta {
            taken_at: file.mtime_ms,
            ..Default::default()
        };
        return Read {
            file,
            meta,
            error: None,
            is_clip,
        };
    }
    match picture_meta(&file.path, file.mtime_ms) {
        Ok(meta) => Read {
            file,
            meta,
            error: None,
            is_clip,
        },
        Err(err) => {
            let meta = PhotoMeta {
                taken_at: file.mtime_ms,
                ..Default::default()
            };
            Read {
                error: Some(format!("{err:#}")),
                file,
                meta,
                is_clip,
            }
        }
    }
}

/// Scans a photo library: albums from folders, pictures with their EXIF details.
pub async fn scan_photos(db: &Db, queue: &JobQueue, library_id: i64) -> anyhow::Result<ScanReport> {
    let roots = libraries::paths(db, library_id).await?;
    let diff = assets::diff(db, library_id, &roots, is_photo_library_file).await?;
    let mut report = ScanReport {
        unchanged: diff.unchanged,
        unreachable: diff.unreachable.clone(),
        ..Default::default()
    };

    // 1. Read EXIF and sizes off the runtime (no transaction open).
    let new_files = diff.new_files.clone();
    let changed: Vec<(i64, Option<i64>, FoundFile)> = diff.changed.clone();
    let (new_reads, changed_reads) = tokio::task::spawn_blocking(move || {
        let new: Vec<Read> = new_files.into_iter().map(read_file).collect();
        let changed: Vec<(i64, Option<i64>, Read)> = changed
            .into_iter()
            .map(|(id, item, file)| (id, item, read_file(file)))
            .collect();
        (new, changed)
    })
    .await?;

    // 2. Write in short batches.
    let mut clips = Vec::new();
    let mut albums: HashMap<String, i64> = HashMap::new();
    for chunk in new_reads.chunks(BATCH) {
        let mut tx = db.writer().begin().await?;
        for read in chunk {
            let relative = read.file.relative.to_string_lossy().replace('\\', "/");
            let parent_id = match album_of(&read.file.relative) {
                Some(album) => Some(match albums.get(&album) {
                    Some(id) => *id,
                    None => {
                        let title = album.rsplit('/').next().unwrap_or(&album).to_owned();
                        let id = items::upsert(
                            &mut tx,
                            &items::NewItem {
                                library_id,
                                kind: "photo_album",
                                parent_id: None,
                                series_id: None,
                                title: &title,
                                sort_title: &sort_title(&album),
                                year: None,
                                index_number: None,
                                index_end: None,
                                parent_index_number: None,
                                premiere_date: None,
                                content_key: &format!("album:{album}"),
                            },
                        )
                        .await?;
                        albums.insert(album, id);
                        id
                    }
                }),
                None => None,
            };
            let kind = if read.is_clip { "video" } else { "photo" };
            let title = title_of(&read.file.relative);
            let item_id = items::upsert(
                &mut tx,
                &items::NewItem {
                    library_id,
                    kind,
                    parent_id,
                    series_id: None,
                    title: &title,
                    sort_title: &sort_title(&title),
                    year: None,
                    index_number: None,
                    index_end: None,
                    parent_index_number: None,
                    premiere_date: None,
                    content_key: &format!("{kind}:{relative}"),
                },
            )
            .await?;
            let path = read.file.path.to_string_lossy();
            let file_id = media::insert_file(
                &mut tx,
                &media::NewFile {
                    library_id,
                    item_id: Some(item_id),
                    path: &path,
                    relative_path: &relative,
                    size: read.file.size as i64,
                    mtime: read.file.mtime_ms,
                    role: "main",
                    extra_kind: None,
                    part: None,
                    parsed_json: None,
                },
            )
            .await?;
            write_details(&mut tx, item_id, file_id, read).await?;
            if read.is_clip {
                clips.push(file_id);
            }
            report.added += 1;
        }
        tx.commit().await?;
    }
    for chunk in changed_reads.chunks(BATCH) {
        let mut tx = db.writer().begin().await?;
        for (file_id, item_id, read) in chunk {
            media::mark_changed(&mut tx, *file_id, read.file.size as i64, read.file.mtime_ms)
                .await?;
            if let Some(item_id) = item_id {
                write_details(&mut tx, *item_id, *file_id, read).await?;
            }
            if read.is_clip {
                clips.push(*file_id);
            }
            report.changed += 1;
        }
        tx.commit().await?;
    }
    assets::apply_presence(db, &diff).await?;
    report.restored = diff.restored.len();
    report.missing = diff.missing.len();

    // 3. Expired files, then pictures and albums nothing points to anymore.
    let now = flixnet_db::now_ms();
    let removed_files =
        media::delete_missing_before(db, library_id, now - MISSING_GRACE_MS).await?;
    if removed_files > 0 || report.changed > 0 {
        report.removed = photos::prune(db, library_id).await?;
    }

    // 4. Clips: probe and thumbnail in the background.
    for file_id in clips {
        if queue_clip(queue, file_id).await?.is_some() {
            report.probes_queued += 1;
        }
    }
    libraries::mark_scanned(db, library_id).await?;
    tracing::info!(library = library_id, ?report, "photo library scanned");
    Ok(report)
}

async fn write_details(
    conn: &mut flixnet_db::SqliteConnection,
    item_id: i64,
    file_id: i64,
    read: &Read,
) -> anyhow::Result<()> {
    photos::upsert_meta(conn, item_id, &read.meta).await?;
    photos::set_item_date(conn, item_id, read.meta.taken_at).await?;
    if !read.is_clip {
        let path = read.file.path.to_string_lossy();
        if read.error.is_none() {
            photos::set_picture(conn, item_id, &path).await?;
        }
        let container = extension(&read.file.path).unwrap_or_default();
        photos::mark_file_read(conn, file_id, &container, read.error.as_deref()).await?;
    }
    Ok(())
}

// ---- Clips --------------------------------------------------------------------------

/// Probes a clip and takes a frame as its thumbnail.
pub struct AnalyzeClip {
    pub ffprobe: Ffprobe,
    pub ffmpeg: PathBuf,
    /// `<config_dir>/cache`.
    pub cache_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipPayload {
    pub file_id: i64,
}

pub async fn queue_clip(queue: &JobQueue, file_id: i64) -> Result<Option<i64>, DbError> {
    queue
        .enqueue::<AnalyzeClip>(
            &ClipPayload { file_id },
            JobOptions::priority(priority::NEW).dedupe(format!("photo-clip:{file_id}")),
        )
        .await
}

/// Thumbnail width of clips.
const CLIP_THUMB_WIDTH: i64 = 640;

impl JobHandler for AnalyzeClip {
    const KIND: &'static str = "photos.clip";
    const POOL: Pool = Pool::Probe;
    type Payload = ClipPayload;

    async fn run(&self, ctx: JobContext, payload: ClipPayload) -> anyhow::Result<()> {
        analyze_clip(
            ctx.db(),
            &self.ffprobe,
            &self.ffmpeg,
            &self.cache_dir,
            payload.file_id,
            &ctx.cancel_token(),
        )
        .await
    }
}

/// Probes a clip, reads its recording date and takes a frame as its thumbnail.
pub async fn analyze_clip(
    db: &Db,
    ffprobe: &Ffprobe,
    ffmpeg: &Path,
    cache_dir: &Path,
    file_id: i64,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    crate::jobs::probe_file(db, ffprobe, file_id).await?;
    let Some(file) = media::get_file(db, file_id).await? else {
        return Ok(());
    };
    let Some(item_id) = file.item_id else {
        return Ok(());
    };
    let streams = media::streams(db, file.id).await?;
    let Some(video) = streams.iter().find(|s| s.kind == "video") else {
        return Ok(());
    };
    // The recording date the camera wrote, else the file time (kept from the scan).
    let taken_at = match ffprobe.tags(Path::new(&file.path)).await {
        Ok(tags) => tags
            .get("creation_time")
            .and_then(|t| parse_iso_datetime(t)),
        Err(err) => {
            tracing::debug!(file = file.id, %err, "clip tags unavailable");
            None
        }
    };
    let (width, height) = (video.width.unwrap_or(0), video.height.unwrap_or(0));
    let thumb_height = if width > 0 && height > 0 {
        (CLIP_THUMB_WIDTH * height / width / 2 * 2).max(2)
    } else {
        360
    };
    let source = VideoSource {
        input: Path::new(&file.path),
        stream_index: video.idx,
        tonemap: video.hdr.as_deref().is_some_and(|h| h != "sdr"),
    };
    let duration = file.duration_ms.unwrap_or(0);
    let done = generate_chapter_images(
        ffmpeg,
        &source,
        (CLIP_THUMB_WIDTH, thumb_height),
        &[(0, 0, duration)],
        cache_dir,
        file.id,
        cancel,
    )
    .await?;
    let mut meta = PhotoMeta {
        taken_at: taken_at.unwrap_or(file.mtime),
        width: video.width,
        height: video.height,
        ..Default::default()
    };
    if meta.taken_at <= 0 {
        meta.taken_at = flixnet_db::now_ms();
    }
    let mut tx = db.writer().begin().await?;
    photos::upsert_meta(&mut tx, item_id, &meta).await?;
    photos::set_item_date(&mut tx, item_id, meta.taken_at).await?;
    if done.contains(&0) {
        let thumb = chapter_image_path(cache_dir, file.id, 0);
        photos::set_picture(&mut tx, item_id, &thumb.to_string_lossy()).await?;
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use exif::experimental::Writer;
    use exif::{Field, Rational};

    /// A small JPEG with the given EXIF fields in an APP1 segment.
    fn jpeg_with_exif(fields: &[Field]) -> Vec<u8> {
        let mut writer = Writer::new();
        for field in fields {
            writer.push_field(field);
        }
        let mut tiff = Cursor::new(Vec::new());
        writer.write(&mut tiff, false).unwrap();
        let tiff = tiff.into_inner();

        let mut jpeg = Cursor::new(Vec::new());
        image::RgbImage::new(8, 6)
            .write_to(&mut jpeg, image::ImageFormat::Jpeg)
            .unwrap();
        let jpeg = jpeg.into_inner();
        let mut app1 = vec![0xFF, 0xE1];
        let length = (2 + 6 + tiff.len()) as u16;
        app1.extend(length.to_be_bytes());
        app1.extend(b"Exif\0\0");
        app1.extend(&tiff);
        let mut out = jpeg[..2].to_vec(); // SOI
        out.extend(app1);
        out.extend(&jpeg[2..]);
        out
    }

    fn ascii(tag: Tag, text: &str) -> Field {
        Field {
            tag,
            ifd_num: In::PRIMARY,
            value: Value::Ascii(vec![text.as_bytes().to_vec()]),
        }
    }

    fn rationals(tag: Tag, values: &[(u32, u32)]) -> Field {
        Field {
            tag,
            ifd_num: In::PRIMARY,
            value: Value::Rational(
                values
                    .iter()
                    .map(|&(num, denom)| Rational { num, denom })
                    .collect(),
            ),
        }
    }

    #[test]
    fn exif_date_camera_exposure_and_gps_are_read() {
        let bytes = jpeg_with_exif(&[
            ascii(Tag::Make, "Canon"),
            ascii(Tag::Model, "Canon EOS R6"),
            ascii(Tag::LensModel, "RF24-105mm F4 L IS USM"),
            ascii(Tag::DateTimeOriginal, "2023:07:14 18:30:05"),
            ascii(Tag::OffsetTimeOriginal, "+02:00"),
            rationals(Tag::FNumber, &[(40, 10)]),
            rationals(Tag::ExposureTime, &[(1, 250)]),
            rationals(Tag::FocalLength, &[(50, 1)]),
            Field {
                tag: Tag::PhotographicSensitivity,
                ifd_num: In::PRIMARY,
                value: Value::Short(vec![400]),
            },
            Field {
                tag: Tag::Orientation,
                ifd_num: In::PRIMARY,
                value: Value::Short(vec![6]),
            },
            ascii(Tag::GPSLatitudeRef, "N"),
            rationals(Tag::GPSLatitude, &[(52, 1), (31, 1), (12, 1)]),
            ascii(Tag::GPSLongitudeRef, "W"),
            rationals(Tag::GPSLongitude, &[(13, 1), (24, 1), (0, 1)]),
        ]);
        let meta = read_exif(&bytes).expect("EXIF");
        // 18:30:05 at UTC+2 → 16:30:05 UTC.
        let expected = (days_from_civil(2023, 7, 14) * 86_400 + 16 * 3600 + 30 * 60 + 5) * 1000;
        assert_eq!(meta.taken_at, expected);
        assert_eq!(meta.camera_make.as_deref(), Some("Canon"));
        assert_eq!(meta.camera_model.as_deref(), Some("Canon EOS R6"));
        assert_eq!(meta.lens.as_deref(), Some("RF24-105mm F4 L IS USM"));
        assert_eq!(meta.f_number, Some(4.0));
        assert_eq!(meta.exposure.as_deref(), Some("1/250"));
        assert_eq!(meta.focal_length, Some(50.0));
        assert_eq!(meta.iso, Some(400));
        assert_eq!(meta.orientation, Some(6));
        assert_eq!(meta.latitude, Some(52.52));
        assert_eq!(meta.longitude, Some(-13.4));
    }

    #[test]
    fn pictures_without_exif_fall_back_to_the_file_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ohne.png");
        image::RgbImage::new(30, 20).save(&path).unwrap();
        assert!(read_exif(&std::fs::read(&path).unwrap()).is_none());
        let meta = picture_meta(&path, 1_700_000_000_000).unwrap();
        assert_eq!(meta.taken_at, 1_700_000_000_000);
        assert_eq!((meta.width, meta.height), (Some(30), Some(20)));

        // Orientation 6 (rotated 90°): the picture shows portrait.
        let rotated = dir.path().join("hoch.jpg");
        let bytes = jpeg_with_exif(&[Field {
            tag: Tag::Orientation,
            ifd_num: In::PRIMARY,
            value: Value::Short(vec![6]),
        }]);
        std::fs::write(&rotated, bytes).unwrap();
        let meta = picture_meta(&rotated, 5).unwrap();
        assert_eq!((meta.width, meta.height), (Some(6), Some(8)));
        assert_eq!(meta.taken_at, 5);

        std::fs::write(dir.path().join("kaputt.jpg"), b"not a picture").unwrap();
        assert!(picture_meta(&dir.path().join("kaputt.jpg"), 5).is_err());
    }

    #[test]
    fn dates_parse_and_reject_nonsense() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(
            parse_iso_datetime("2023-07-14T10:20:30.000000Z"),
            Some((days_from_civil(2023, 7, 14) * 86_400 + 10 * 3600 + 20 * 60 + 30) * 1000)
        );
        assert_eq!(parse_iso_datetime("1970-01-01T00:00:00Z"), None);
        assert_eq!(parse_iso_datetime("gestern"), None);
        assert_eq!(unix_ms((2023, 13, 1), (0, 0, 0), 0), None);
        assert_eq!(exposure_text(2.5).as_deref(), Some("2.5"));
        assert_eq!(exposure_text(0.0), None);
    }

    #[test]
    fn photo_library_files() {
        assert!(is_photo_library_file(Path::new("a/B.JPG")));
        assert!(is_photo_library_file(Path::new("clip.mp4")));
        assert!(is_photo_library_file(Path::new("x.webp")));
        assert!(!is_photo_library_file(Path::new("notes.txt")));
        assert!(!is_photo_library_file(Path::new("raw.heic")));
        assert_eq!(
            album_of(Path::new("2023/Urlaub/a.jpg")).as_deref(),
            Some("2023/Urlaub")
        );
        assert_eq!(album_of(Path::new("a.jpg")), None);
    }
}
