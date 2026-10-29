//! Media segments (intro, credits, recap), the audio fingerprints they are detected
//! from, and the per-season work list of the analysis.

use crate::{Db, Result, now_ms};

/// Kinds of segments, as stored.
pub const KINDS: [&str; 3] = ["intro", "credits", "recap"];

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Segment {
    pub file_id: i64,
    pub kind: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub source: String,
    pub confidence: f64,
    pub computed_at: i64,
}

/// A detected segment to store.
#[derive(Debug, Clone, PartialEq)]
pub struct NewSegment {
    pub file_id: i64,
    pub kind: &'static str,
    pub start_ms: i64,
    pub end_ms: i64,
    pub source: &'static str,
    pub confidence: f64,
}

/// Segments of one file, in playback order.
pub async fn for_file(db: &Db, file_id: i64) -> Result<Vec<Segment>> {
    let rows = sqlx::query_as(
        "SELECT file_id, kind, start_ms, end_ms, source, confidence, computed_at
         FROM media_segments WHERE file_id = ? ORDER BY start_ms",
    )
    .bind(file_id)
    .fetch_all(db.reader())
    .await?;
    Ok(rows)
}

/// Segments of the present main files of an item.
pub async fn for_item(db: &Db, item_id: i64) -> Result<Vec<Segment>> {
    let rows = sqlx::query_as(
        "SELECT s.file_id, s.kind, s.start_ms, s.end_ms, s.source, s.confidence, s.computed_at
         FROM media_segments s JOIN media_files f ON f.id = s.file_id
         WHERE f.item_id = ? AND f.role = 'main' AND f.missing_since IS NULL
         ORDER BY f.part, f.path, s.start_ms",
    )
    .bind(item_id)
    .fetch_all(db.reader())
    .await?;
    Ok(rows)
}

/// Replaces the detected segments of the given files in one short transaction:
/// `files` lists every file that was analyzed (their old detections go), `found` the
/// new ones. Manually set segments are never touched.
pub async fn replace_detected(db: &Db, files: &[i64], found: &[NewSegment]) -> Result<()> {
    let now = now_ms();
    let mut tx = db.writer().begin().await?;
    for file_id in files {
        sqlx::query("DELETE FROM media_segments WHERE file_id = ? AND source <> 'manual'")
            .bind(file_id)
            .execute(&mut *tx)
            .await?;
    }
    for s in found {
        sqlx::query(
            "INSERT INTO media_segments (file_id, kind, start_ms, end_ms, source, confidence, computed_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (file_id, kind) DO NOTHING",
        )
        .bind(s.file_id)
        .bind(s.kind)
        .bind(s.start_ms)
        .bind(s.end_ms)
        .bind(s.source)
        .bind(s.confidence)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Sets a segment by hand; it wins over (and survives) detection.
pub async fn set_manual(
    db: &Db,
    file_id: i64,
    kind: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO media_segments (file_id, kind, start_ms, end_ms, source, confidence, computed_at)
         VALUES (?, ?, ?, ?, 'manual', 1.0, ?)
         ON CONFLICT (file_id, kind) DO UPDATE SET
             start_ms = excluded.start_ms, end_ms = excluded.end_ms, source = 'manual',
             confidence = 1.0, computed_at = excluded.computed_at",
    )
    .bind(file_id)
    .bind(kind)
    .bind(start_ms)
    .bind(end_ms)
    .bind(now_ms())
    .execute(db.writer())
    .await?;
    Ok(())
}

/// Removes a segment (detected or manual); returns whether there was one.
pub async fn delete(db: &Db, file_id: i64, kind: &str) -> Result<bool> {
    let result = sqlx::query("DELETE FROM media_segments WHERE file_id = ? AND kind = ?")
        .bind(file_id)
        .bind(kind)
        .execute(db.writer())
        .await?;
    Ok(result.rows_affected() > 0)
}

// ---- Fingerprints -------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFingerprint {
    pub stream_index: i64,
    pub start_ms: i64,
    pub items: Vec<u32>,
}

/// The cached fingerprint of a file region (`head` | `tail`).
pub async fn fingerprint(db: &Db, file_id: i64, region: &str) -> Result<Option<StoredFingerprint>> {
    let row: Option<(i64, i64, Vec<u8>)> = sqlx::query_as(
        "SELECT stream_index, start_ms, items FROM audio_fingerprints WHERE file_id = ? AND region = ?",
    )
    .bind(file_id)
    .bind(region)
    .fetch_optional(db.reader())
    .await?;
    Ok(
        row.map(|(stream_index, start_ms, bytes)| StoredFingerprint {
            stream_index,
            start_ms,
            items: bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes(*c))
                .collect(),
        }),
    )
}

pub async fn store_fingerprint(
    db: &Db,
    file_id: i64,
    region: &str,
    print: &StoredFingerprint,
) -> Result<()> {
    let bytes: Vec<u8> = print.items.iter().flat_map(|i| i.to_le_bytes()).collect();
    sqlx::query(
        "INSERT INTO audio_fingerprints (file_id, region, stream_index, start_ms, items, computed_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT (file_id, region) DO UPDATE SET
             stream_index = excluded.stream_index, start_ms = excluded.start_ms,
             items = excluded.items, computed_at = excluded.computed_at",
    )
    .bind(file_id)
    .bind(region)
    .bind(print.stream_index)
    .bind(print.start_ms)
    .bind(bytes)
    .bind(now_ms())
    .execute(db.writer())
    .await?;
    Ok(())
}

// ---- What to analyze ----------------------------------------------------------------

/// A probed main file of an episode, as the season analysis needs it.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct EpisodeFile {
    pub file_id: i64,
    pub item_id: i64,
    pub path: String,
    pub duration_ms: Option<i64>,
    pub season_number: Option<i64>,
    pub episode_number: Option<i64>,
}

/// Probed, present main files of the episodes of one season (or of a series'
/// episodes without season), in episode order.
pub async fn season_files(db: &Db, season_id: i64) -> Result<Vec<EpisodeFile>> {
    let rows = sqlx::query_as(
        "SELECT f.id AS file_id, i.id AS item_id, f.path, f.duration_ms,
                i.parent_index_number AS season_number, i.index_number AS episode_number
         FROM items i JOIN media_files f ON f.item_id = i.id
         WHERE i.parent_id = ? AND i.kind = 'episode' AND f.role = 'main'
           AND f.missing_since IS NULL AND f.probed_at IS NOT NULL
         ORDER BY i.index_number IS NULL, i.index_number, f.part, f.path",
    )
    .bind(season_id)
    .fetch_all(db.reader())
    .await?;
    Ok(rows)
}

/// The season (episode parent) a file belongs to, if it is an episode in a library
/// with intro detection switched on.
pub async fn season_of_file(db: &Db, file_id: i64) -> Result<Option<i64>> {
    let row: Option<(Option<i64>,)> = sqlx::query_as(
        "SELECT i.parent_id FROM media_files f
         JOIN items i ON i.id = f.item_id
         JOIN libraries l ON l.id = f.library_id
         WHERE f.id = ? AND f.role = 'main' AND i.kind = 'episode' AND l.intro_detection = 1",
    )
    .bind(file_id)
    .fetch_optional(db.reader())
    .await?;
    Ok(row.and_then(|(parent,)| parent))
}

/// Seasons (episode parents) below an item: the item itself if it is a season, all of
/// a series' seasons, the season of an episode, or every season of a library
/// (`library_id`). Only libraries with intro detection switched on count.
pub async fn seasons_below(db: &Db, item_id: i64) -> Result<Vec<i64>> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT DISTINCT e.parent_id FROM items e
         JOIN libraries l ON l.id = e.library_id
         WHERE e.kind = 'episode' AND e.parent_id IS NOT NULL AND l.intro_detection = 1
           AND (e.id = ?1 OR e.parent_id = ?1 OR e.series_id = ?1)
         ORDER BY e.parent_id",
    )
    .bind(item_id)
    .fetch_all(db.reader())
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Every season of a library (when intro detection is on there).
pub async fn library_seasons(db: &Db, library_id: i64) -> Result<Vec<i64>> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT DISTINCT e.parent_id FROM items e
         JOIN libraries l ON l.id = e.library_id
         WHERE e.library_id = ? AND e.kind = 'episode' AND e.parent_id IS NOT NULL
           AND l.intro_detection = 1
         ORDER BY e.parent_id",
    )
    .bind(library_id)
    .fetch_all(db.reader())
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Seasons with probed episode files that have not been fingerprinted yet (backfill).
pub async fn seasons_pending(db: &Db) -> Result<Vec<i64>> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT DISTINCT i.parent_id FROM media_files f
         JOIN items i ON i.id = f.item_id
         JOIN libraries l ON l.id = f.library_id
         WHERE i.kind = 'episode' AND i.parent_id IS NOT NULL AND f.role = 'main'
           AND f.missing_since IS NULL AND f.probed_at IS NOT NULL AND l.intro_detection = 1
           AND NOT EXISTS (SELECT 1 FROM audio_fingerprints a
                           WHERE a.file_id = f.id AND a.region = 'head')
         ORDER BY i.parent_id",
    )
    .fetch_all(db.reader())
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

// ---- Preferences --------------------------------------------------------------------

/// A personal preference (JSON value); `None` if never set.
pub async fn preference<T: serde::de::DeserializeOwned>(
    db: &Db,
    user_id: i64,
    key: &str,
) -> Result<Option<T>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value_json FROM user_preferences WHERE user_id = ? AND key = ?")
            .bind(user_id)
            .bind(key)
            .fetch_optional(db.reader())
            .await?;
    row.map(|(json,)| serde_json::from_str(&json))
        .transpose()
        .map_err(Into::into)
}

pub async fn set_preference<T: serde::Serialize + ?Sized>(
    db: &Db,
    user_id: i64,
    key: &str,
    value: &T,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO user_preferences (user_id, key, value_json, updated_at) VALUES (?, ?, ?, ?)
         ON CONFLICT (user_id, key) DO UPDATE SET value_json = excluded.value_json,
                                                  updated_at = excluded.updated_at",
    )
    .bind(user_id)
    .bind(key)
    .bind(serde_json::to_string(value)?)
    .bind(now_ms())
    .execute(db.writer())
    .await?;
    Ok(())
}
