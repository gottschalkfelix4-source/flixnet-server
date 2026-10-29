//! Media files, their streams and chapters.

use sqlx::SqliteConnection;

use crate::{Db, Result, now_ms};

/// What the scanner needs to diff the file system against the database.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct FileState {
    pub id: i64,
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    pub missing_since: Option<i64>,
}

pub async fn file_states(db: &Db, library_id: i64) -> Result<Vec<FileState>> {
    let rows = sqlx::query_as(
        "SELECT id, path, size, mtime, missing_since FROM media_files WHERE library_id = ?",
    )
    .bind(library_id)
    .fetch_all(db.reader())
    .await?;
    Ok(rows)
}

#[derive(Debug, Clone)]
pub struct NewFile<'a> {
    pub library_id: i64,
    pub item_id: Option<i64>,
    pub path: &'a str,
    pub relative_path: &'a str,
    pub size: i64,
    pub mtime: i64,
    pub role: &'a str,
    pub extra_kind: Option<&'a str>,
    pub part: Option<i64>,
    pub parsed_json: Option<&'a str>,
}

pub async fn insert_file(conn: &mut SqliteConnection, file: &NewFile<'_>) -> Result<i64> {
    let now = now_ms();
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO media_files (library_id, item_id, path, relative_path, size, mtime, role,
                                  extra_kind, part, parsed_json, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(file.library_id)
    .bind(file.item_id)
    .bind(file.path)
    .bind(file.relative_path)
    .bind(file.size)
    .bind(file.mtime)
    .bind(file.role)
    .bind(file.extra_kind)
    .bind(file.part)
    .bind(file.parsed_json)
    .bind(now)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;
    Ok(id)
}

/// Content changed on disk: store the new size/mtime and require a new probe.
pub async fn mark_changed(
    conn: &mut SqliteConnection,
    id: i64,
    size: i64,
    mtime: i64,
) -> Result<()> {
    sqlx::query(
        "UPDATE media_files SET size = ?, mtime = ?, probed_at = NULL, probe_error = NULL,
                                missing_since = NULL, updated_at = ?
         WHERE id = ?",
    )
    .bind(size)
    .bind(mtime)
    .bind(now_ms())
    .bind(id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// A file was moved or renamed: keep the row (and so its item and history).
pub async fn move_file(
    conn: &mut SqliteConnection,
    id: i64,
    path: &str,
    relative_path: &str,
) -> Result<()> {
    sqlx::query(
        "UPDATE media_files SET path = ?, relative_path = ?, missing_since = NULL, updated_at = ?
         WHERE id = ?",
    )
    .bind(path)
    .bind(relative_path)
    .bind(now_ms())
    .bind(id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// A file that was missing is back (unchanged).
pub async fn mark_present(conn: &mut SqliteConnection, id: i64) -> Result<()> {
    sqlx::query("UPDATE media_files SET missing_since = NULL WHERE id = ?")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

pub async fn mark_missing(conn: &mut SqliteConnection, id: i64, since: i64) -> Result<()> {
    sqlx::query("UPDATE media_files SET missing_since = ? WHERE id = ? AND missing_since IS NULL")
        .bind(since)
        .bind(id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Deletes files missing for longer than the grace period. Returns how many.
pub async fn delete_missing_before(db: &Db, library_id: i64, cutoff: i64) -> Result<u64> {
    let result = sqlx::query(
        "DELETE FROM media_files WHERE library_id = ? AND missing_since IS NOT NULL AND missing_since < ?",
    )
    .bind(library_id)
    .bind(cutoff)
    .execute(db.writer())
    .await?;
    Ok(result.rows_affected())
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct MediaFile {
    pub id: i64,
    pub library_id: i64,
    pub item_id: Option<i64>,
    pub path: String,
    pub relative_path: String,
    pub size: i64,
    pub mtime: i64,
    pub role: String,
    pub extra_kind: Option<String>,
    pub part: Option<i64>,
    pub container: Option<String>,
    pub duration_ms: Option<i64>,
    pub bitrate: Option<i64>,
    pub probed_at: Option<i64>,
    pub probe_error: Option<String>,
    pub missing_since: Option<i64>,
}

macro_rules! file_columns {
    () => {
        "id, library_id, item_id, path, relative_path, size, mtime, role, extra_kind, part,
         container, duration_ms, bitrate, probed_at, probe_error, missing_since"
    };
}

pub async fn get_file(db: &Db, id: i64) -> Result<Option<MediaFile>> {
    let file = sqlx::query_as(concat!(
        "SELECT ",
        file_columns!(),
        " FROM media_files WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db.reader())
    .await?;
    Ok(file)
}

pub async fn files_for_item(db: &Db, item_id: i64) -> Result<Vec<MediaFile>> {
    let files = sqlx::query_as(concat!(
        "SELECT ",
        file_columns!(),
        " FROM media_files WHERE item_id = ? AND missing_since IS NULL
          ORDER BY role, part, path"
    ))
    .bind(item_id)
    .fetch_all(db.reader())
    .await?;
    Ok(files)
}

/// A present main file of the item or, for series/seasons, of one of its episodes
/// (used to locate folders for NFO and artwork lookup).
pub async fn representative_file(db: &Db, item_id: i64) -> Result<Option<MediaFile>> {
    let file = sqlx::query_as(concat!(
        "SELECT ",
        file_columns!(),
        " FROM media_files
          WHERE missing_since IS NULL AND role = 'main'
            AND item_id IN (SELECT id FROM items WHERE id = ?1 OR parent_id = ?1 OR series_id = ?1)
          ORDER BY path LIMIT 1"
    ))
    .bind(item_id)
    .fetch_optional(db.reader())
    .await?;
    Ok(file)
}

#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct Stream {
    pub idx: i64,
    pub kind: String,
    pub codec: Option<String>,
    pub profile: Option<String>,
    pub level: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub fps: Option<f64>,
    pub bit_depth: Option<i64>,
    pub pix_fmt: Option<String>,
    pub color_transfer: Option<String>,
    pub color_primaries: Option<String>,
    pub hdr: Option<String>,
    pub channels: Option<i64>,
    pub channel_layout: Option<String>,
    pub sample_rate: Option<i64>,
    pub bitrate: Option<i64>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_default: bool,
    pub is_forced: bool,
    pub is_sdh: bool,
    pub is_external: bool,
    pub external_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Chapter {
    pub idx: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProbeData {
    pub container: Option<String>,
    pub duration_ms: Option<i64>,
    pub bitrate: Option<i64>,
    pub streams: Vec<Stream>,
    pub chapters: Vec<Chapter>,
}

/// Replaces a file's streams/chapters with fresh probe results (one transaction).
pub async fn store_probe(db: &Db, file_id: i64, data: &ProbeData) -> Result<()> {
    let mut tx = db.writer().begin().await?;
    sqlx::query("DELETE FROM media_streams WHERE file_id = ?")
        .bind(file_id)
        .execute(&mut *tx)
        .await?;
    // A re-probe means the file changed: its keyframe index is stale.
    sqlx::query("DELETE FROM keyframes WHERE file_id = ?")
        .bind(file_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM chapters WHERE file_id = ?")
        .bind(file_id)
        .execute(&mut *tx)
        .await?;
    for s in &data.streams {
        sqlx::query(
            "INSERT INTO media_streams (file_id, idx, kind, codec, profile, level, width, height, fps,
                bit_depth, pix_fmt, color_transfer, color_primaries, hdr, channels, channel_layout,
                sample_rate, bitrate, language, title, is_default, is_forced, is_sdh, is_external,
                external_path)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(file_id)
        .bind(s.idx)
        .bind(&s.kind)
        .bind(&s.codec)
        .bind(&s.profile)
        .bind(s.level)
        .bind(s.width)
        .bind(s.height)
        .bind(s.fps)
        .bind(s.bit_depth)
        .bind(&s.pix_fmt)
        .bind(&s.color_transfer)
        .bind(&s.color_primaries)
        .bind(&s.hdr)
        .bind(s.channels)
        .bind(&s.channel_layout)
        .bind(s.sample_rate)
        .bind(s.bitrate)
        .bind(&s.language)
        .bind(&s.title)
        .bind(s.is_default)
        .bind(s.is_forced)
        .bind(s.is_sdh)
        .bind(s.is_external)
        .bind(&s.external_path)
        .execute(&mut *tx)
        .await?;
    }
    for c in &data.chapters {
        sqlx::query(
            "INSERT INTO chapters (file_id, idx, start_ms, end_ms, title) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(file_id)
        .bind(c.idx)
        .bind(c.start_ms)
        .bind(c.end_ms)
        .bind(&c.title)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE media_files SET container = ?, duration_ms = ?, bitrate = ?, probed_at = ?,
                                probe_error = NULL, updated_at = ?
         WHERE id = ?",
    )
    .bind(&data.container)
    .bind(data.duration_ms)
    .bind(data.bitrate)
    .bind(now_ms())
    .bind(now_ms())
    .bind(file_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn store_probe_error(db: &Db, file_id: i64, error: &str) -> Result<()> {
    sqlx::query("UPDATE media_files SET probe_error = ?, updated_at = ? WHERE id = ?")
        .bind(error)
        .bind(now_ms())
        .bind(file_id)
        .execute(db.writer())
        .await?;
    Ok(())
}

pub async fn streams(db: &Db, file_id: i64) -> Result<Vec<Stream>> {
    let rows = sqlx::query_as(
        "SELECT idx, kind, codec, profile, level, width, height, fps, bit_depth, pix_fmt,
                color_transfer, color_primaries, hdr, channels, channel_layout, sample_rate,
                bitrate, language, title, is_default, is_forced, is_sdh, is_external, external_path
         FROM media_streams WHERE file_id = ? ORDER BY idx",
    )
    .bind(file_id)
    .fetch_all(db.reader())
    .await?;
    Ok(rows)
}

pub async fn chapters(db: &Db, file_id: i64) -> Result<Vec<Chapter>> {
    let rows = sqlx::query_as(
        "SELECT idx, start_ms, end_ms, title FROM chapters WHERE file_id = ? ORDER BY idx",
    )
    .bind(file_id)
    .fetch_all(db.reader())
    .await?;
    Ok(rows)
}

/// Cached keyframe times (ms) of a video stream, if computed.
pub async fn keyframes(db: &Db, file_id: i64, stream_index: i64) -> Result<Option<Vec<i64>>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT times_ms FROM keyframes WHERE file_id = ? AND stream_index = ?")
            .bind(file_id)
            .bind(stream_index)
            .fetch_optional(db.reader())
            .await?;
    Ok(row.map(|(times,)| times.split(',').filter_map(|t| t.parse().ok()).collect()))
}

pub async fn store_keyframes(
    db: &Db,
    file_id: i64,
    stream_index: i64,
    times_ms: &[i64],
) -> Result<()> {
    let times = times_ms
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    sqlx::query(
        "INSERT INTO keyframes (file_id, stream_index, times_ms, computed_at) VALUES (?, ?, ?, ?)
         ON CONFLICT (file_id, stream_index) DO UPDATE
         SET times_ms = excluded.times_ms, computed_at = excluded.computed_at",
    )
    .bind(file_id)
    .bind(stream_index)
    .bind(times)
    .bind(now_ms())
    .execute(db.writer())
    .await?;
    Ok(())
}
