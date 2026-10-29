//! Downloaded subtitles (OpenSubtitles) and the bookkeeping of automatic downloads.

use crate::{Db, Result, now_ms};

/// A subtitle file Flixnet downloaded for a media file.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct DownloadedSubtitle {
    pub id: i64,
    pub file_id: i64,
    pub path: String,
    /// ISO 639-2 (`deu`).
    pub language: Option<String>,
    pub is_sdh: bool,
    pub is_forced: bool,
    pub provider: String,
    pub provider_file_id: Option<String>,
    pub release_name: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewDownloadedSubtitle<'a> {
    pub file_id: i64,
    pub path: &'a str,
    pub language: Option<&'a str>,
    pub is_sdh: bool,
    pub is_forced: bool,
    pub provider: &'a str,
    pub provider_file_id: Option<&'a str>,
    pub release_name: Option<&'a str>,
}

/// Records a downloaded file and adds it as an external subtitle stream of the media
/// file right away (numbered after the existing sidecar streams, from 1000 on), so
/// playback info lists it without a re-probe. Returns the stream index.
pub async fn register(db: &Db, s: &NewDownloadedSubtitle<'_>) -> Result<i64> {
    let mut tx = db.writer().begin().await?;
    sqlx::query(
        "INSERT INTO downloaded_subtitles (file_id, path, language, is_sdh, is_forced, provider,
                                           provider_file_id, release_name, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (file_id, path) DO UPDATE SET
             language = excluded.language, is_sdh = excluded.is_sdh,
             is_forced = excluded.is_forced, provider_file_id = excluded.provider_file_id,
             release_name = excluded.release_name, created_at = excluded.created_at",
    )
    .bind(s.file_id)
    .bind(s.path)
    .bind(s.language)
    .bind(s.is_sdh)
    .bind(s.is_forced)
    .bind(s.provider)
    .bind(s.provider_file_id)
    .bind(s.release_name)
    .bind(now_ms())
    .execute(&mut *tx)
    .await?;
    let existing: Option<(i64,)> = sqlx::query_as(
        "SELECT idx FROM media_streams WHERE file_id = ? AND is_external = 1 AND external_path = ?",
    )
    .bind(s.file_id)
    .bind(s.path)
    .fetch_optional(&mut *tx)
    .await?;
    let idx = match existing {
        Some((idx,)) => idx,
        None => {
            let (next,): (i64,) = sqlx::query_as(
                "SELECT COALESCE(MAX(idx) + 1, 1000) FROM media_streams
                 WHERE file_id = ? AND idx >= 1000",
            )
            .bind(s.file_id)
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO media_streams (file_id, idx, kind, codec, language, is_forced, is_sdh,
                                            is_external, external_path)
                 VALUES (?, ?, 'subtitle', 'srt', ?, ?, ?, 1, ?)",
            )
            .bind(s.file_id)
            .bind(next)
            .bind(s.language)
            .bind(s.is_forced)
            .bind(s.is_sdh)
            .bind(s.path)
            .execute(&mut *tx)
            .await?;
            next
        }
    };
    tx.commit().await?;
    Ok(idx)
}

/// Downloaded subtitles of a media file, oldest first.
pub async fn for_file(db: &Db, file_id: i64) -> Result<Vec<DownloadedSubtitle>> {
    Ok(sqlx::query_as(
        "SELECT id, file_id, path, language, is_sdh, is_forced, provider, provider_file_id,
                release_name, created_at
         FROM downloaded_subtitles WHERE file_id = ? ORDER BY created_at, id",
    )
    .bind(file_id)
    .fetch_all(db.reader())
    .await?)
}

/// Main files of movies and episodes added since `since_ms`, newest first: candidates
/// for automatic downloads.
pub async fn recent_main_files(db: &Db, since_ms: i64, limit: i64) -> Result<Vec<i64>> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT f.id FROM media_files f JOIN items i ON i.id = f.item_id
         WHERE f.role = 'main' AND f.missing_since IS NULL AND f.created_at >= ?
           AND i.kind IN ('movie', 'episode')
         ORDER BY f.created_at DESC, f.id DESC LIMIT ?",
    )
    .bind(since_ms)
    .bind(limit)
    .fetch_all(db.reader())
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Whether an automatic search for this file and language has been made already.
pub async fn attempted(db: &Db, file_id: i64, language: &str) -> Result<bool> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM subtitle_search_attempts WHERE file_id = ? AND language = ?")
            .bind(file_id)
            .bind(language)
            .fetch_optional(db.reader())
            .await?;
    Ok(row.is_some())
}

pub async fn record_attempt(db: &Db, file_id: i64, language: &str, downloaded: bool) -> Result<()> {
    sqlx::query(
        "INSERT INTO subtitle_search_attempts (file_id, language, attempted_at, downloaded)
         VALUES (?, ?, ?, ?)
         ON CONFLICT (file_id, language) DO UPDATE SET
             attempted_at = excluded.attempted_at, downloaded = excluded.downloaded",
    )
    .bind(file_id)
    .bind(language)
    .bind(now_ms())
    .bind(downloaded)
    .execute(db.writer())
    .await?;
    Ok(())
}
