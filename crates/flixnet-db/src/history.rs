//! Watch history from outside: what a scan just added (for notifications), the
//! library index external ids are matched against (Plex/Jellyfin import, Trakt), and
//! writing imported watch state.
//!
//! Imported state is merged, never blindly overwritten: "watched" and favorites only
//! get added, play counts take the maximum and the more recent side wins the resume
//! point. Everything is written by identity, like local playback.

use crate::{Db, Result, now_ms};

/// Rows per write transaction.
const BATCH: usize = 200;

/// A movie or episode a scan added.
#[derive(Debug, Clone, PartialEq)]
pub struct AddedItem {
    pub item_id: i64,
    pub kind: String,
    /// Display title ("Dark – S01E02 Lügen" for episodes, "Alpha (2020)" for movies).
    pub title: String,
}

/// Movies and episodes of a library added at or after `since`: how many, and the
/// first `limit` of them.
pub async fn items_added_since(
    db: &Db,
    library_id: i64,
    since: i64,
    limit: i64,
) -> Result<(i64, Vec<AddedItem>)> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM items
         WHERE library_id = ? AND date_added >= ? AND kind IN ('movie', 'episode')",
    )
    .bind(library_id)
    .bind(since)
    .fetch_one(db.reader())
    .await?;
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        i64,
        String,
        String,
        Option<i64>,
        Option<String>,
        Option<i64>,
        Option<i64>,
    )> = sqlx::query_as(
        "SELECT i.id, i.kind, i.title, i.year, s.title, i.parent_index_number, i.index_number
             FROM items i LEFT JOIN items s ON s.id = i.series_id
             WHERE i.library_id = ? AND i.date_added >= ? AND i.kind IN ('movie', 'episode')
             ORDER BY i.id LIMIT ?",
    )
    .bind(library_id)
    .bind(since)
    .bind(limit)
    .fetch_all(db.reader())
    .await?;
    let items = rows
        .into_iter()
        .map(
            |(item_id, kind, title, year, series, season, episode)| AddedItem {
                title: label(&kind, title, year, series, season, episode),
                item_id,
                kind,
            },
        )
        .collect();
    Ok((count, items))
}

fn label(
    kind: &str,
    title: String,
    year: Option<i64>,
    series: Option<String>,
    season: Option<i64>,
    episode: Option<i64>,
) -> String {
    match (kind, series) {
        ("episode", Some(series)) => match (season, episode) {
            (Some(s), Some(e)) => format!("{series} – S{s:02}E{e:02} {title}"),
            _ => format!("{series} – {title}"),
        },
        _ => match year {
            Some(year) => format!("{title} ({year})"),
            None => title,
        },
    }
}

/// Display title of an item ("Dark – S01E02 Lügen", "Alpha (2020)").
pub async fn item_label(db: &Db, item_id: i64) -> Result<Option<String>> {
    #[allow(clippy::type_complexity)]
    let row: Option<(
        String,
        String,
        Option<i64>,
        Option<String>,
        Option<i64>,
        Option<i64>,
    )> = sqlx::query_as(
        "SELECT i.kind, i.title, i.year, s.title, i.parent_index_number, i.index_number
             FROM items i LEFT JOIN items s ON s.id = i.series_id WHERE i.id = ?",
    )
    .bind(item_id)
    .fetch_optional(db.reader())
    .await?;
    Ok(row.map(|(kind, title, year, series, season, episode)| {
        label(&kind, title, year, series, season, episode)
    }))
}

/// A movie, series or episode as the matcher sees it.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct IndexItem {
    pub id: i64,
    pub kind: String,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<i64>,
    pub series_id: Option<i64>,
    /// Episodes: season number.
    pub season: Option<i64>,
    /// Episodes: episode number (first one of a multi-episode file).
    pub episode: Option<i64>,
    /// Episodes: last episode number of a multi-episode file.
    pub episode_end: Option<i64>,
    /// Identity user data hangs on.
    pub identity: String,
}

/// Every movie, series and episode, plus all provider ids `(item, provider, value)`.
pub async fn match_index(db: &Db) -> Result<(Vec<IndexItem>, Vec<(i64, String, String)>)> {
    let items = sqlx::query_as(
        "SELECT id, kind, title, original_title, year, series_id,
                parent_index_number AS season, index_number AS episode, index_end AS episode_end,
                COALESCE(identity_key, content_key) AS identity
         FROM items WHERE kind IN ('movie', 'series', 'episode') ORDER BY id",
    )
    .fetch_all(db.reader())
    .await?;
    let ids = sqlx::query_as(
        "SELECT p.item_id, p.provider, p.value FROM provider_ids p
         JOIN items i ON i.id = p.item_id WHERE i.kind IN ('movie', 'series', 'episode')",
    )
    .fetch_all(db.reader())
    .await?;
    Ok((items, ids))
}

/// Watch state of one item as another server (or Trakt) knows it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ImportedState {
    pub item_id: i64,
    pub identity: String,
    pub played: bool,
    pub play_count: i64,
    pub position_ms: i64,
    pub last_played_at: Option<i64>,
    pub favorite: bool,
    /// 0–10.
    pub rating: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImportWrite {
    /// Items whose user data was written.
    pub items: usize,
    /// History entries added (one per watched item and source, never twice).
    pub history: usize,
}

/// Merges imported state into a user's data and adds one history entry per watched
/// item (`source` marks the entries, e.g. `plex`, so a repeated import adds none).
pub async fn apply_import(
    db: &Db,
    user_id: i64,
    source: &str,
    client: &str,
    states: &[ImportedState],
) -> Result<ImportWrite> {
    let marker = format!("import:{source}");
    let mut out = ImportWrite::default();
    for chunk in states.chunks(BATCH) {
        let now = now_ms();
        let mut tx = db.writer().begin().await?;
        for s in chunk {
            sqlx::query(
                "INSERT INTO user_item_data
                     (user_id, identity_key, played, play_count, position_ms, last_played_at,
                      favorite, rating, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (user_id, identity_key) DO UPDATE SET
                     played = MAX(user_item_data.played, excluded.played),
                     play_count = MAX(user_item_data.play_count, excluded.play_count),
                     position_ms = CASE
                         WHEN COALESCE(excluded.last_played_at, 0)
                              > COALESCE(user_item_data.last_played_at, 0)
                         THEN excluded.position_ms ELSE user_item_data.position_ms END,
                     last_played_at = NULLIF(MAX(COALESCE(user_item_data.last_played_at, 0),
                                                 COALESCE(excluded.last_played_at, 0)), 0),
                     favorite = MAX(user_item_data.favorite, excluded.favorite),
                     rating = COALESCE(user_item_data.rating, excluded.rating),
                     updated_at = excluded.updated_at",
            )
            .bind(user_id)
            .bind(&s.identity)
            .bind(s.played)
            .bind(if s.played {
                s.play_count.max(1)
            } else {
                s.play_count.max(0)
            })
            // A watched item may carry a resume point again (started a second time),
            // as local playback keeps it.
            .bind(s.position_ms.max(0))
            .bind(s.last_played_at)
            .bind(s.favorite)
            .bind(s.rating)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            out.items += 1;
            if s.played {
                let added = sqlx::query(
                    "INSERT INTO watch_events
                         (user_id, identity_key, item_id, event, position_ms, completed,
                          session_id, client, created_at)
                     SELECT ?1, ?2, ?3, 'stop', 0, 1, ?4, ?5, ?6
                     WHERE NOT EXISTS (SELECT 1 FROM watch_events
                                       WHERE user_id = ?1 AND identity_key = ?2 AND session_id = ?4)",
                )
                .bind(user_id)
                .bind(&s.identity)
                .bind(s.item_id)
                .bind(&marker)
                .bind(client)
                .bind(s.last_played_at.unwrap_or(now))
                .execute(&mut *tx)
                .await?;
                out.history += added.rows_affected() as usize;
            }
        }
        tx.commit().await?;
    }
    Ok(out)
}

/// A user's watched items and ratings (for pushing to Trakt).
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct LocalState {
    pub item_id: i64,
    pub kind: String,
    pub played: bool,
    pub last_played_at: Option<i64>,
    pub rating: Option<f64>,
}

/// Movies and episodes the user watched or rated (one item per identity).
pub async fn local_states(db: &Db, user_id: i64) -> Result<Vec<LocalState>> {
    Ok(sqlx::query_as(
        "SELECT MIN(i.id) AS item_id, i.kind, ud.played, ud.last_played_at, ud.rating
         FROM user_item_data ud JOIN items i ON i.identity_key = ud.identity_key
         WHERE ud.user_id = ? AND i.kind IN ('movie', 'episode')
           AND (ud.played = 1 OR ud.rating IS NOT NULL)
         GROUP BY ud.identity_key",
    )
    .bind(user_id)
    .fetch_all(db.reader())
    .await?)
}

/// Sets a user's rating of an item (0–10, `None` clears it).
pub async fn set_rating(db: &Db, user_id: i64, identity: &str, rating: Option<f64>) -> Result<()> {
    sqlx::query(
        "INSERT INTO user_item_data (user_id, identity_key, rating, updated_at) VALUES (?, ?, ?, ?)
         ON CONFLICT (user_id, identity_key) DO UPDATE SET
             rating = excluded.rating, updated_at = excluded.updated_at",
    )
    .bind(user_id)
    .bind(identity)
    .bind(rating)
    .bind(now_ms())
    .execute(db.writer())
    .await?;
    Ok(())
}
