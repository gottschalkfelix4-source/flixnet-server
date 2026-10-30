//! Scene search: time-stamped text chunks of media files (from subtitles or Whisper)
//! with an FTS5 index.
//!
//! Chunks hang on files, items are reached through `media_files.item_id`, so every
//! read applies the viewer's library access and parental controls to the item via
//! [`push_filters`] — a chunk is never returned for an item the viewer cannot see.

use sqlx::{QueryBuilder, Sqlite};

use crate::browse::{ItemFilter, push_filters};
use crate::{Db, Result, now_ms};

/// Where a transcript comes from.
pub const SOURCE_SUBTITLE: &str = "subtitle";
pub const SOURCE_WHISPER: &str = "whisper";

/// One time window of spoken text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewChunk {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

/// An indexed source of a file.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Transcript {
    pub id: i64,
    pub file_id: i64,
    pub source: String,
    pub stream_index: i64,
    pub language: Option<String>,
    pub signature: String,
    pub chunk_count: i64,
    pub created_at: i64,
}

/// The transcripts stored for a file.
pub async fn for_file(db: &Db, file_id: i64) -> Result<Vec<Transcript>> {
    Ok(sqlx::query_as(
        "SELECT id, file_id, source, stream_index, language, signature, chunk_count, created_at
         FROM transcripts WHERE file_id = ? ORDER BY source, stream_index",
    )
    .bind(file_id)
    .fetch_all(db.reader())
    .await?)
}

/// Replaces one source of a file with new chunks (one short transaction; compute the
/// chunks before calling this).
pub async fn store(
    db: &Db,
    file_id: i64,
    source: &str,
    stream_index: i64,
    language: Option<&str>,
    signature: &str,
    chunks: &[NewChunk],
) -> Result<i64> {
    let mut tx = db.writer().begin().await?;
    sqlx::query("DELETE FROM transcripts WHERE file_id = ? AND source = ? AND stream_index = ?")
        .bind(file_id)
        .bind(source)
        .bind(stream_index)
        .execute(&mut *tx)
        .await?;
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO transcripts (file_id, source, stream_index, language, signature,
                                  chunk_count, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(file_id)
    .bind(source)
    .bind(stream_index)
    .bind(language)
    .bind(signature)
    .bind(chunks.len() as i64)
    .bind(now_ms())
    .fetch_one(&mut *tx)
    .await?;
    for batch in chunks.chunks(200) {
        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
            "INSERT INTO transcript_chunks (transcript_id, file_id, start_ms, end_ms, text) ",
        );
        qb.push_values(batch, |mut row, chunk| {
            row.push_bind(id)
                .push_bind(file_id)
                .push_bind(chunk.start_ms)
                .push_bind(chunk.end_ms)
                .push_bind(chunk.text.clone());
        });
        qb.build().execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(id)
}

/// Removes transcripts of a file whose sources are gone (e.g. a deleted sidecar).
pub async fn remove_except(db: &Db, file_id: i64, keep: &[(String, i64)]) -> Result<u64> {
    let mut removed = 0;
    for t in for_file(db, file_id).await? {
        if !keep
            .iter()
            .any(|(source, idx)| *source == t.source && *idx == t.stream_index)
        {
            removed += sqlx::query("DELETE FROM transcripts WHERE id = ?")
                .bind(t.id)
                .execute(db.writer())
                .await?
                .rows_affected();
        }
    }
    Ok(removed)
}

/// A scene search hit.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct ChunkHit {
    pub chunk_id: i64,
    pub file_id: i64,
    pub item_id: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    /// bm25 (lower is better).
    pub rank: f64,
}

/// Full-text search over chunks of items the viewer may see. `fts` is an FTS5 query
/// (build it from user text with a quoting helper, never pass raw input). `filter`
/// carries the viewer's restrictions; `item_id` narrows to one movie/episode or, for
/// a series or season, to its episodes.
pub async fn search(
    db: &Db,
    fts: &str,
    filter: &ItemFilter,
    item_id: Option<i64>,
    limit: i64,
) -> Result<Vec<ChunkHit>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT c.id AS chunk_id, c.file_id, i.id AS item_id, c.start_ms, c.end_ms, c.text,
                bm25(transcript_chunks_fts) AS rank
         FROM transcript_chunks_fts
         JOIN transcript_chunks c ON c.id = transcript_chunks_fts.rowid
         JOIN media_files mf ON mf.id = c.file_id AND mf.missing_since IS NULL
         JOIN items i ON i.id = mf.item_id",
    );
    push_filters(&mut qb, filter);
    qb.push(" AND transcript_chunks_fts MATCH ")
        .push_bind(fts.to_owned());
    if let Some(id) = item_id {
        qb.push(" AND (i.id = ")
            .push_bind(id)
            .push(" OR i.parent_id = ")
            .push_bind(id)
            .push(" OR i.series_id = ")
            .push_bind(id)
            .push(")");
    }
    qb.push(" ORDER BY rank, c.id LIMIT ").push_bind(limit);
    Ok(qb.build_query_as().fetch_all(db.reader()).await?)
}

/// Chunks by id, restricted to what the viewer may see (for semantic hits).
pub async fn visible_chunks(db: &Db, ids: &[i64], filter: &ItemFilter) -> Result<Vec<ChunkHit>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT c.id AS chunk_id, c.file_id, i.id AS item_id, c.start_ms, c.end_ms, c.text,
                0.0 AS rank
         FROM transcript_chunks c
         JOIN media_files mf ON mf.id = c.file_id AND mf.missing_since IS NULL
         JOIN items i ON i.id = mf.item_id",
    );
    push_filters(&mut qb, filter);
    qb.push(" AND c.id IN (");
    {
        let mut list = qb.separated(", ");
        for id in ids {
            list.push_bind(*id);
        }
    }
    qb.push(")");
    Ok(qb.build_query_as().fetch_all(db.reader()).await?)
}

/// One chunk of a recap's source text.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct ItemChunk {
    pub item_id: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

/// Chunks of an item's main files in time order, from its best transcript (one
/// source only, so languages are not mixed), ending at or before `before_ms`
/// (`None` = the whole item). Chunks that reach past `before_ms` are left out.
pub async fn item_chunks(db: &Db, item_id: i64, before_ms: Option<i64>) -> Result<Vec<ItemChunk>> {
    let transcript: Option<(i64,)> = sqlx::query_as(
        "SELECT t.id FROM transcripts t
         JOIN media_files mf ON mf.id = t.file_id
         WHERE mf.item_id = ? AND mf.role = 'main' AND mf.missing_since IS NULL
           AND t.chunk_count > 0
         ORDER BY CASE WHEN t.language IN ('deu', 'ger', 'de') THEN 0 ELSE 1 END,
                  CASE t.source WHEN 'subtitle' THEN 0 ELSE 1 END,
                  COALESCE(mf.part, 0), t.stream_index
         LIMIT 1",
    )
    .bind(item_id)
    .fetch_optional(db.reader())
    .await?;
    let Some((transcript_id,)) = transcript else {
        return Ok(Vec::new());
    };
    Ok(sqlx::query_as(
        "SELECT ? AS item_id, start_ms, end_ms, text FROM transcript_chunks
         WHERE transcript_id = ? AND end_ms <= ?
         ORDER BY start_ms, id",
    )
    .bind(item_id)
    .bind(transcript_id)
    .bind(before_ms.unwrap_or(i64::MAX))
    .fetch_all(db.reader())
    .await?)
}

/// Stream index of the marker row a subtitle index run leaves behind (no chunks), so
/// files are not picked up again until they are probed again.
pub const RUN_MARKER: i64 = -1;

/// Present, probed main files (oldest first) that still need a subtitle index run:
/// never indexed, or indexed before the file was last probed.
pub async fn files_to_index(db: &Db, limit: i64) -> Result<Vec<i64>> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT mf.id FROM media_files mf
         WHERE mf.role = 'main' AND mf.missing_since IS NULL AND mf.probed_at IS NOT NULL
           AND mf.item_id IS NOT NULL
           AND EXISTS (SELECT 1 FROM media_streams s WHERE s.file_id = mf.id AND s.kind = 'subtitle')
           AND NOT EXISTS (SELECT 1 FROM transcripts t WHERE t.file_id = mf.id
                             AND t.source = 'subtitle' AND t.stream_index = -1
                             AND t.created_at >= mf.probed_at)
         ORDER BY mf.id LIMIT ?",
    )
    .bind(limit)
    .fetch_all(db.reader())
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Files in libraries with Whisper enabled that have no spoken text indexed yet and
/// no Whisper run (oldest first). Files with text subtitles are skipped by the job.
pub async fn files_to_transcribe(db: &Db, limit: i64) -> Result<Vec<i64>> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT mf.id FROM media_files mf
         JOIN libraries l ON l.id = mf.library_id AND l.transcribe_audio = 1
         WHERE mf.role = 'main' AND mf.missing_since IS NULL AND mf.probed_at IS NOT NULL
           AND mf.item_id IS NOT NULL
           AND EXISTS (SELECT 1 FROM media_streams s WHERE s.file_id = mf.id AND s.kind = 'audio')
           AND NOT EXISTS (SELECT 1 FROM transcripts t WHERE t.file_id = mf.id
                             AND (t.chunk_count > 0 OR t.source = 'whisper'))
         ORDER BY mf.id LIMIT ?",
    )
    .bind(limit)
    .fetch_all(db.reader())
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Whether Whisper transcription is enabled for a library.
pub async fn library_transcribes(db: &Db, library_id: i64) -> Result<bool> {
    let row: Option<(bool,)> =
        sqlx::query_as("SELECT transcribe_audio FROM libraries WHERE id = ?")
            .bind(library_id)
            .fetch_optional(db.reader())
            .await?;
    Ok(row.is_some_and(|(on,)| on))
}

/// Ids of the libraries with Whisper transcription enabled.
pub async fn transcribing_libraries(db: &Db) -> Result<Vec<i64>> {
    let rows: Vec<(i64,)> =
        sqlx::query_as("SELECT id FROM libraries WHERE transcribe_audio = 1 ORDER BY id")
            .fetch_all(db.reader())
            .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Enables or disables Whisper transcription for a library; false if it does not exist.
pub async fn set_library_transcribes(db: &Db, library_id: i64, enabled: bool) -> Result<bool> {
    let done = sqlx::query("UPDATE libraries SET transcribe_audio = ? WHERE id = ?")
        .bind(enabled)
        .bind(library_id)
        .execute(db.writer())
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Index size for the settings page.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct Stats {
    pub files: i64,
    pub chunks: i64,
    pub whisper_files: i64,
}

pub async fn stats(db: &Db) -> Result<Stats> {
    Ok(sqlx::query_as(
        "SELECT (SELECT COUNT(DISTINCT file_id) FROM transcripts WHERE chunk_count > 0) AS files,
                (SELECT COUNT(*) FROM transcript_chunks) AS chunks,
                (SELECT COUNT(DISTINCT file_id) FROM transcripts WHERE source = 'whisper') AS whisper_files",
    )
    .fetch_one(db.reader())
    .await?)
}
