//! Viewing statistics for the admin dashboard, from the append-only `watch_events`.
//!
//! A "play" is one playback session (all events with the same `session_id`; events
//! without one — imports, music listens — count on their own). Its watch time is how
//! far the position moved within the session, or the full runtime for a single
//! completed event.

use crate::{Db, Result};

/// One user's activity in the period.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct UserStat {
    pub user_id: i64,
    pub name: String,
    pub plays: i64,
    pub watch_ms: i64,
    pub movies_ms: i64,
    pub tv_ms: i64,
    pub music_ms: i64,
    pub other_ms: i64,
}

/// Watch time of one time bucket, per media kind.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct HistoryBucket {
    /// Start of the bucket (unix ms).
    pub start: i64,
    pub movies_ms: i64,
    pub tv_ms: i64,
    pub music_ms: i64,
    pub other_ms: i64,
}

/// Something played often: a movie, a series (its episodes) or an artist (their tracks).
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct TopEntry {
    pub id: i64,
    pub plays: i64,
    pub users: i64,
}

/// The plays since `since` with their kind, the series/artist they belong to and
/// their watch time. Shared by every query below.
macro_rules! plays_cte {
    () => {
        "WITH plays AS (
            SELECT p.user_id, p.item_id, p.started, i.kind, i.series_id,
                   MAX(0, MIN(p.watched, COALESCE(i.runtime_ms, p.watched))) AS watched
            FROM (
                SELECT e.user_id,
                       MAX(e.item_id) AS item_id,
                       MIN(e.created_at) AS started,
                       CASE WHEN COUNT(*) = 1 AND MAX(e.completed) = 1
                            THEN COALESCE(MAX(e.duration_ms), MAX(e.position_ms))
                            ELSE MAX(e.position_ms) - MIN(e.position_ms) END AS watched
                FROM watch_events e
                WHERE e.created_at >= ?1
                GROUP BY e.user_id, COALESCE(e.session_id, 'event:' || e.id)
            ) p
            JOIN items i ON i.id = p.item_id
        )"
    };
}

/// Users by watch time, most active first.
pub async fn top_users(db: &Db, since: i64, limit: i64) -> Result<Vec<UserStat>> {
    let rows = sqlx::query_as(concat!(
        plays_cte!(),
        " SELECT u.id AS user_id, u.name,
                 COUNT(*) AS plays,
                 SUM(p.watched) AS watch_ms,
                 SUM(CASE WHEN p.kind = 'movie' THEN p.watched ELSE 0 END) AS movies_ms,
                 SUM(CASE WHEN p.kind = 'episode' THEN p.watched ELSE 0 END) AS tv_ms,
                 SUM(CASE WHEN p.kind = 'track' THEN p.watched ELSE 0 END) AS music_ms,
                 SUM(CASE WHEN p.kind NOT IN ('movie', 'episode', 'track') THEN p.watched ELSE 0 END)
                     AS other_ms
          FROM plays p JOIN users u ON u.id = p.user_id
          GROUP BY u.id
          ORDER BY watch_ms DESC, plays DESC, u.name
          LIMIT ?2"
    ))
    .bind(since)
    .bind(limit)
    .fetch_all(db.reader())
    .await?;
    Ok(rows)
}

/// Watch time per bucket of `bucket_ms` from `since` up to and including `until`;
/// empty buckets are included so charts get a continuous axis.
pub async fn history(
    db: &Db,
    since: i64,
    until: i64,
    bucket_ms: i64,
) -> Result<Vec<HistoryBucket>> {
    let bucket_ms = bucket_ms.max(60_000);
    let rows: Vec<HistoryBucket> = sqlx::query_as(concat!(
        plays_cte!(),
        " SELECT ?1 + ((p.started - ?1) / ?2) * ?2 AS start,
                 SUM(CASE WHEN p.kind = 'movie' THEN p.watched ELSE 0 END) AS movies_ms,
                 SUM(CASE WHEN p.kind = 'episode' THEN p.watched ELSE 0 END) AS tv_ms,
                 SUM(CASE WHEN p.kind = 'track' THEN p.watched ELSE 0 END) AS music_ms,
                 SUM(CASE WHEN p.kind NOT IN ('movie', 'episode', 'track') THEN p.watched ELSE 0 END)
                     AS other_ms
          FROM plays p
          GROUP BY start
          ORDER BY start"
    ))
    .bind(since)
    .bind(bucket_ms)
    .fetch_all(db.reader())
    .await?;
    let mut out = Vec::new();
    let mut start = since;
    // The last bucket contains `until` itself (plays logged right now count).
    while start <= until {
        out.push(
            rows.iter()
                .find(|r| r.start == start)
                .cloned()
                .unwrap_or(HistoryBucket {
                    start,
                    movies_ms: 0,
                    tv_ms: 0,
                    music_ms: 0,
                    other_ms: 0,
                }),
        );
        start += bucket_ms;
    }
    Ok(out)
}

/// Most played movies.
pub async fn top_movies(db: &Db, since: i64, limit: i64) -> Result<Vec<TopEntry>> {
    let rows = sqlx::query_as(concat!(
        plays_cte!(),
        " SELECT p.item_id AS id, COUNT(*) AS plays, COUNT(DISTINCT p.user_id) AS users
          FROM plays p WHERE p.kind = 'movie'
          GROUP BY p.item_id ORDER BY plays DESC, users DESC, id LIMIT ?2"
    ))
    .bind(since)
    .bind(limit)
    .fetch_all(db.reader())
    .await?;
    Ok(rows)
}

/// Most played series (all their episodes together).
pub async fn top_series(db: &Db, since: i64, limit: i64) -> Result<Vec<TopEntry>> {
    let rows = sqlx::query_as(concat!(
        plays_cte!(),
        " SELECT p.series_id AS id, COUNT(*) AS plays, COUNT(DISTINCT p.user_id) AS users
          FROM plays p WHERE p.kind = 'episode' AND p.series_id IS NOT NULL
          GROUP BY p.series_id ORDER BY plays DESC, users DESC, id LIMIT ?2"
    ))
    .bind(since)
    .bind(limit)
    .fetch_all(db.reader())
    .await?;
    Ok(rows)
}

/// Most played artists (tracks count for their album artist).
pub async fn top_artists(db: &Db, since: i64, limit: i64) -> Result<Vec<TopEntry>> {
    let rows = sqlx::query_as(concat!(
        plays_cte!(),
        " SELECT p.series_id AS id, COUNT(*) AS plays, COUNT(DISTINCT p.user_id) AS users
          FROM plays p WHERE p.kind = 'track' AND p.series_id IS NOT NULL
          GROUP BY p.series_id ORDER BY plays DESC, users DESC, id LIMIT ?2"
    ))
    .bind(since)
    .bind(limit)
    .fetch_all(db.reader())
    .await?;
    Ok(rows)
}
