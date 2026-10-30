//! Trickplay sheets and chapter images of media files (the images live in the cache
//! directory; the database knows what exists).

use crate::{Db, Result, now_ms};

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Trickplay {
    pub file_id: i64,
    pub interval_ms: i64,
    pub tile_width: i64,
    pub tile_height: i64,
    pub tile_columns: i64,
    pub tile_rows: i64,
    pub thumbnails: i64,
    pub sheets: i64,
    pub created_at: i64,
}

pub async fn get(db: &Db, file_id: i64) -> Result<Option<Trickplay>> {
    let row = sqlx::query_as(
        "SELECT file_id, interval_ms, tile_width, tile_height, tile_columns, tile_rows, thumbnails, sheets,
                created_at
         FROM trickplay WHERE file_id = ?",
    )
    .bind(file_id)
    .fetch_optional(db.reader())
    .await?;
    Ok(row)
}

/// Stores (or replaces) a file's sheets; `created_at` is set to now.
pub async fn store(db: &Db, trickplay: &Trickplay) -> Result<()> {
    sqlx::query(
        "INSERT INTO trickplay (file_id, interval_ms, tile_width, tile_height, tile_columns, tile_rows,
                                thumbnails, sheets, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (file_id) DO UPDATE SET
            interval_ms = excluded.interval_ms, tile_width = excluded.tile_width,
            tile_height = excluded.tile_height, tile_columns = excluded.tile_columns, tile_rows = excluded.tile_rows,
            thumbnails = excluded.thumbnails, sheets = excluded.sheets,
            created_at = excluded.created_at",
    )
    .bind(trickplay.file_id)
    .bind(trickplay.interval_ms)
    .bind(trickplay.tile_width)
    .bind(trickplay.tile_height)
    .bind(trickplay.tile_columns)
    .bind(trickplay.tile_rows)
    .bind(trickplay.thumbnails)
    .bind(trickplay.sheets)
    .bind(now_ms())
    .execute(db.writer())
    .await?;
    Ok(())
}

/// Present, probed main files with a video stream but no sheets yet (backfill).
pub async fn files_without_trickplay(db: &Db, limit: i64) -> Result<Vec<i64>> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT f.id FROM media_files f
         WHERE f.role = 'main' AND f.missing_since IS NULL AND f.probed_at IS NOT NULL
           AND f.duration_ms > 0
           AND EXISTS (SELECT 1 FROM media_streams s WHERE s.file_id = f.id AND s.kind = 'video')
           AND NOT EXISTS (SELECT 1 FROM trickplay t WHERE t.file_id = f.id)
         ORDER BY f.id LIMIT ?",
    )
    .bind(limit)
    .fetch_all(db.reader())
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Present files with chapters that have no image yet (backfill).
pub async fn files_without_chapter_images(db: &Db, limit: i64) -> Result<Vec<i64>> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT DISTINCT c.file_id FROM chapters c JOIN media_files f ON f.id = c.file_id
         WHERE c.image_at IS NULL AND f.missing_since IS NULL AND f.role = 'main'
         ORDER BY c.file_id LIMIT ?",
    )
    .bind(limit)
    .fetch_all(db.reader())
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ChapterImage {
    pub idx: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub image_at: Option<i64>,
}

/// A file's chapters with the time their image was made.
pub async fn chapters(db: &Db, file_id: i64) -> Result<Vec<ChapterImage>> {
    let rows = sqlx::query_as(
        "SELECT idx, start_ms, end_ms, image_at FROM chapters WHERE file_id = ? ORDER BY idx",
    )
    .bind(file_id)
    .fetch_all(db.reader())
    .await?;
    Ok(rows)
}

/// Marks chapter images as made (one short transaction, after the images exist).
pub async fn mark_chapter_images(db: &Db, file_id: i64, indexes: &[i64]) -> Result<()> {
    let now = now_ms();
    let mut tx = db.writer().begin().await?;
    for idx in indexes {
        sqlx::query("UPDATE chapters SET image_at = ? WHERE file_id = ? AND idx = ?")
            .bind(now)
            .bind(file_id)
            .bind(idx)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}
