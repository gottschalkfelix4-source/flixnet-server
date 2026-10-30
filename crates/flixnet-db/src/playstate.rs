//! Playback progress: resume points, "watched" on completion, the append-only watch
//! history, and the lists built from them ("Weiterschauen", "Nächste Folge").
//!
//! Everything a user did hangs on the item's identity (see [`crate::userdata`]), so it
//! survives renames and rescans.

use sqlx::{QueryBuilder, Sqlite};

use crate::browse::{ItemFilter, push_filters};
use crate::{Db, Result, now_ms};

/// Below this position a stop leaves no resume point.
pub const MIN_RESUME_MS: i64 = 2 * 60_000;
/// …nor below this share of the runtime (percent).
pub const MIN_RESUME_PERCENT: i64 = 5;
/// From this share on (percent) a stop counts as watched. Until credits detection
/// (milestone 5.1) this stands in for "the credits are running".
pub const PLAYED_PERCENT: i64 = 90;
/// Series nobody touched for this long drop out of "Nächste Folge".
pub const NEXT_UP_MAX_AGE_MS: i64 = 180 * 24 * 60 * 60 * 1000;

/// What a reported position means for the user's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// Too early to remember: no resume point.
    TooEarly,
    /// Worth resuming from this position.
    Resume(i64),
    /// Close enough to the end to count as watched.
    Played,
}

/// Classifies a position against the duration (unknown duration: only the minimum
/// position applies, and nothing ever counts as watched).
pub fn classify(position_ms: i64, duration_ms: Option<i64>) -> Progress {
    let position = position_ms.max(0);
    match duration_ms.filter(|d| *d > 0) {
        Some(duration) if position * 100 >= duration * PLAYED_PERCENT => Progress::Played,
        Some(duration)
            if position < MIN_RESUME_MS || position * 100 < duration * MIN_RESUME_PERCENT =>
        {
            Progress::TooEarly
        }
        None if position < MIN_RESUME_MS => Progress::TooEarly,
        _ => Progress::Resume(position),
    }
}

/// Which share of the runtime a position has passed: 0, or 1–3 for 25/50/75 %.
pub fn quartile(position_ms: i64, duration_ms: Option<i64>) -> u8 {
    match duration_ms.filter(|d| *d > 0) {
        Some(duration) => (position_ms.max(0).saturating_mul(4) / duration).clamp(0, 3) as u8,
        None => 0,
    }
}

/// Duration of an item for the thresholds: its longest main file, else the runtime
/// from metadata.
pub async fn item_duration(db: &Db, item_id: i64) -> Result<Option<i64>> {
    let (duration,): (Option<i64>,) = sqlx::query_as(
        "SELECT COALESCE(
            (SELECT MAX(f.duration_ms) FROM media_files f
             WHERE f.item_id = ?1 AND f.role = 'main' AND f.missing_since IS NULL),
            (SELECT i.runtime_ms FROM items i WHERE i.id = ?1))",
    )
    .bind(item_id)
    .fetch_one(db.reader())
    .await?;
    Ok(duration.filter(|d| *d > 0))
}

/// While playing: keeps the resume point current once the position is worth one.
/// Earlier positions change nothing (an existing resume point stays until the user
/// gets past the threshold again), and so does the end zone until the stop.
pub async fn save_progress(
    db: &Db,
    user_id: i64,
    item_id: i64,
    position_ms: i64,
    duration_ms: Option<i64>,
) -> Result<Progress> {
    let progress = classify(position_ms, duration_ms);
    if let Progress::Resume(position) = progress {
        set_position(db, user_id, item_id, position).await?;
    }
    Ok(progress)
}

/// Playback ended at `position_ms`: watched near the end (position reset), a resume
/// point in between, nothing for very early stops.
pub async fn save_stop(
    db: &Db,
    user_id: i64,
    item_id: i64,
    position_ms: i64,
    duration_ms: Option<i64>,
) -> Result<Progress> {
    let progress = classify(position_ms, duration_ms);
    match progress {
        Progress::Resume(position) => set_position(db, user_id, item_id, position).await?,
        Progress::Played => mark_completed(db, user_id, item_id).await?,
        Progress::TooEarly => {}
    }
    Ok(progress)
}

async fn set_position(db: &Db, user_id: i64, item_id: i64, position_ms: i64) -> Result<()> {
    let now = now_ms();
    sqlx::query(
        "INSERT INTO user_item_data (user_id, identity_key, position_ms, last_played_at, updated_at)
         SELECT ?, COALESCE(identity_key, content_key), ?, ?, ? FROM items WHERE id = ?
         ON CONFLICT (user_id, identity_key) DO UPDATE SET
             position_ms = excluded.position_ms,
             last_played_at = excluded.last_played_at,
             updated_at = excluded.updated_at",
    )
    .bind(user_id)
    .bind(position_ms)
    .bind(now)
    .bind(now)
    .bind(item_id)
    .execute(db.writer())
    .await?;
    Ok(())
}

/// Watched to the end: played, one more play, resume point cleared.
async fn mark_completed(db: &Db, user_id: i64, item_id: i64) -> Result<()> {
    let now = now_ms();
    sqlx::query(
        "INSERT INTO user_item_data
             (user_id, identity_key, played, play_count, position_ms, last_played_at, updated_at)
         SELECT ?, COALESCE(identity_key, content_key), 1, 1, 0, ?, ? FROM items WHERE id = ?
         ON CONFLICT (user_id, identity_key) DO UPDATE SET
             played = 1,
             play_count = user_item_data.play_count + 1,
             position_ms = 0,
             last_played_at = excluded.last_played_at,
             updated_at = excluded.updated_at",
    )
    .bind(user_id)
    .bind(now)
    .bind(now)
    .bind(item_id)
    .execute(db.writer())
    .await?;
    Ok(())
}

/// One entry of the watch history.
#[derive(Debug, Clone, Default)]
pub struct NewWatchEvent<'a> {
    pub user_id: i64,
    pub item_id: i64,
    /// `start`, `progress` (25/50/75 % passed) or `stop`.
    pub event: &'a str,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub play_method: Option<&'a str>,
    /// Stop events: the item counted as watched.
    pub completed: bool,
    pub session_id: Option<&'a str>,
    pub device_id: Option<i64>,
    pub client: Option<&'a str>,
    pub device_name: Option<&'a str>,
}

/// Appends to the watch history (the item's identity is resolved here).
pub async fn record_event(db: &Db, e: &NewWatchEvent<'_>) -> Result<()> {
    sqlx::query(
        "INSERT INTO watch_events
             (user_id, identity_key, item_id, event, position_ms, duration_ms, play_method,
              completed, session_id, device_id, client, device_name, created_at)
         SELECT ?, COALESCE(identity_key, content_key), id, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
         FROM items WHERE id = ?",
    )
    .bind(e.user_id)
    .bind(e.event)
    .bind(e.position_ms)
    .bind(e.duration_ms)
    .bind(e.play_method)
    .bind(e.completed)
    .bind(e.session_id)
    .bind(e.device_id)
    .bind(e.client)
    .bind(e.device_name)
    .bind(now_ms())
    .bind(e.item_id)
    .execute(db.writer())
    .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct WatchEvent {
    pub id: i64,
    pub user_id: i64,
    pub identity_key: String,
    pub item_id: Option<i64>,
    pub event: String,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub play_method: Option<String>,
    pub completed: bool,
    pub session_id: Option<String>,
    pub device_id: Option<i64>,
    pub client: Option<String>,
    pub device_name: Option<String>,
    pub created_at: i64,
}

/// A user's history, oldest first.
pub async fn events(db: &Db, user_id: i64) -> Result<Vec<WatchEvent>> {
    Ok(sqlx::query_as(
        "SELECT id, user_id, identity_key, item_id, event, position_ms, duration_ms, play_method,
                completed, session_id, device_id, client, device_name, created_at
         FROM watch_events WHERE user_id = ? ORDER BY id",
    )
    .bind(user_id)
    .fetch_all(db.reader())
    .await?)
}

/// What the viewer may see: libraries and parental controls.
#[derive(Debug, Clone, Default)]
pub struct Visibility {
    pub library_ids: Vec<i64>,
    pub restriction: crate::browse::Restriction,
}

impl Visibility {
    fn filter(&self, kinds: &[&str]) -> ItemFilter {
        let mut filter = ItemFilter {
            library_ids: self.library_ids.clone(),
            kinds: kinds.iter().map(|k| (*k).to_owned()).collect(),
            ..Default::default()
        };
        filter.restrict(&self.restriction);
        filter
    }
}

/// "Weiterschauen": movies and episodes with a resume point, most recent first. One
/// entry per identity (the same movie in two libraries shows once).
pub async fn resume_items(
    db: &Db,
    user_id: i64,
    visibility: &Visibility,
    limit: i64,
) -> Result<Vec<i64>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT i.id, i.identity_key FROM items i
         JOIN user_item_data ud ON ud.identity_key = i.identity_key AND ud.user_id = ",
    );
    qb.push_bind(user_id);
    push_filters(&mut qb, &visibility.filter(&["movie", "episode"]));
    qb.push(" AND ud.position_ms > 0 ORDER BY ud.last_played_at DESC, i.id LIMIT ")
        .push_bind(limit.saturating_mul(2));
    let rows: Vec<(i64, Option<String>)> = qb.build_query_as().fetch_all(db.reader()).await?;
    let mut seen = std::collections::HashSet::new();
    Ok(rows
        .into_iter()
        .filter(|(_, identity)| seen.insert(identity.clone()))
        .map(|(id, _)| id)
        .take(limit.max(0) as usize)
        .collect())
}

/// The episode to continue a series with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeriesNext {
    pub episode_id: i64,
    /// The user's resume point in that episode (0 = from the start).
    pub position_ms: i64,
    /// A watched episode led here (otherwise it is simply the first unwatched one).
    pub after_watched: bool,
}

/// The first unwatched regular episode after the furthest watched one (specials,
/// season 0, are skipped; a multi-episode file counts up to its last episode).
/// Without any watched episode: the first unwatched one.
pub async fn series_next(db: &Db, user_id: i64, series_id: i64) -> Result<Option<SeriesNext>> {
    let anchor: Option<(i64, i64)> = sqlx::query_as(
        "SELECT i.parent_index_number, COALESCE(i.index_end, i.index_number) FROM items i
         JOIN user_item_data ud ON ud.identity_key = i.identity_key AND ud.user_id = ?
         WHERE i.series_id = ? AND i.kind = 'episode' AND ud.played = 1
           AND i.parent_index_number > 0 AND i.index_number IS NOT NULL
         ORDER BY i.parent_index_number DESC, COALESCE(i.index_end, i.index_number) DESC
         LIMIT 1",
    )
    .bind(user_id)
    .bind(series_id)
    .fetch_optional(db.reader())
    .await?;
    let (season, episode) = anchor.unwrap_or((0, 0));
    let next: Option<(i64, i64)> = sqlx::query_as(
        "SELECT i.id, COALESCE(ud.position_ms, 0) FROM items i
         LEFT JOIN user_item_data ud ON ud.identity_key = i.identity_key AND ud.user_id = ?
         WHERE i.series_id = ? AND i.kind = 'episode'
           AND i.parent_index_number > 0 AND i.index_number IS NOT NULL
           AND (i.parent_index_number > ? OR (i.parent_index_number = ? AND i.index_number > ?))
           AND COALESCE(ud.played, 0) = 0
         ORDER BY i.parent_index_number, i.index_number, i.id
         LIMIT 1",
    )
    .bind(user_id)
    .bind(series_id)
    .bind(season)
    .bind(season)
    .bind(episode)
    .fetch_optional(db.reader())
    .await?;
    Ok(next.map(|(episode_id, position_ms)| SeriesNext {
        episode_id,
        position_ms,
        after_watched: anchor.is_some(),
    }))
}

/// The first episode of a series: regular episodes before specials.
pub async fn first_episode(db: &Db, series_id: i64) -> Result<Option<i64>> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT id FROM items WHERE series_id = ? AND kind = 'episode'
         ORDER BY (parent_index_number IS NULL OR parent_index_number = 0),
                  parent_index_number, (index_number IS NULL), index_number, id
         LIMIT 1",
    )
    .bind(series_id)
    .fetch_optional(db.reader())
    .await?;
    Ok(row.map(|(id,)| id))
}

/// "Nächste Folge": for series with a watched episode and activity since `since_ms`,
/// the next unwatched episode, most recently active series first. Episodes with a
/// resume point are left out (they are in "Weiterschauen").
pub async fn next_up(
    db: &Db,
    user_id: i64,
    visibility: &Visibility,
    since_ms: i64,
    limit: i64,
) -> Result<Vec<i64>> {
    next_up_with(db, user_id, visibility, since_ms, limit, false).await
}

/// [`next_up`], optionally keeping next episodes that have a resume point (Jellyfin's
/// `Shows/NextUp` lists them by default).
pub async fn next_up_with(
    db: &Db,
    user_id: i64,
    visibility: &Visibility,
    since_ms: i64,
    limit: i64,
    include_resumable: bool,
) -> Result<Vec<i64>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT i.series_id, MAX(ud.last_played_at) AS last_at FROM items i
         JOIN user_item_data ud ON ud.identity_key = i.identity_key AND ud.user_id = ",
    );
    qb.push_bind(user_id);
    push_filters(&mut qb, &visibility.filter(&["episode"]));
    qb.push(" AND i.series_id IS NOT NULL AND (ud.played = 1 OR ud.position_ms > 0)")
        .push(" AND ud.last_played_at >= ")
        .push_bind(since_ms)
        .push(" GROUP BY i.series_id ORDER BY last_at DESC LIMIT ")
        // Some series have nothing left to watch; look a little further.
        .push_bind(limit.saturating_mul(3));
    let series: Vec<(i64, Option<i64>)> = qb.build_query_as().fetch_all(db.reader()).await?;

    let mut out = Vec::new();
    for (series_id, _) in series {
        if out.len() as i64 >= limit {
            break;
        }
        if let Some(next) = series_next(db, user_id, series_id).await?
            && next.after_watched
            && (include_resumable || next.position_ms == 0)
            && crate::browse::is_visible(
                db,
                next.episode_id,
                &visibility.library_ids,
                &visibility.restriction,
            )
            .await?
        {
            out.push(next.episode_id);
        }
    }
    Ok(out)
}

/// The episode after this one in broadcast order, watched or not (for autoplay).
/// Specials only lead to further specials; a multi-episode file continues after its
/// last episode.
pub async fn next_episode(db: &Db, episode_id: i64) -> Result<Option<i64>> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT i.id FROM items cur
         JOIN items i ON i.series_id = cur.series_id AND i.kind = 'episode'
                     AND i.index_number IS NOT NULL AND i.id != cur.id
         WHERE cur.id = ? AND cur.kind = 'episode'
           AND ((cur.parent_index_number = 0 AND i.parent_index_number = 0)
                OR (cur.parent_index_number > 0 AND i.parent_index_number > 0))
           AND (i.parent_index_number > cur.parent_index_number
                OR (i.parent_index_number = cur.parent_index_number
                    AND i.index_number > COALESCE(cur.index_end, cur.index_number)))
         ORDER BY i.parent_index_number, i.index_number, i.id
         LIMIT 1",
    )
    .bind(episode_id)
    .fetch_optional(db.reader())
    .await?;
    Ok(row.map(|(id,)| id))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    #[test]
    fn early_stops_leave_no_resume_point() {
        let film = Some(120 * MIN);
        assert_eq!(classify(0, film), Progress::TooEarly);
        assert_eq!(classify(MIN, film), Progress::TooEarly, "under 2 minutes");
        // 2:30 of 120 minutes is only 2 %.
        assert_eq!(classify(150_000, film), Progress::TooEarly, "under 5 %");
        assert_eq!(classify(6 * MIN + 1, film), Progress::Resume(6 * MIN + 1));
        // A 22-minute episode: 2 minutes are 9 %.
        assert_eq!(classify(2 * MIN, Some(22 * MIN)), Progress::Resume(2 * MIN));
    }

    #[test]
    fn the_end_zone_counts_as_watched() {
        let film = Some(100 * MIN);
        assert_eq!(classify(89 * MIN, film), Progress::Resume(89 * MIN));
        assert_eq!(classify(90 * MIN, film), Progress::Played);
        assert_eq!(classify(100 * MIN, film), Progress::Played);
        assert_eq!(classify(130 * MIN, film), Progress::Played, "past the end");
    }

    #[test]
    fn without_a_duration_only_the_minimum_applies() {
        assert_eq!(classify(MIN, None), Progress::TooEarly);
        assert_eq!(classify(500 * MIN, None), Progress::Resume(500 * MIN));
        assert_eq!(classify(-5, Some(0)), Progress::TooEarly);
    }

    #[test]
    fn quartiles() {
        let d = Some(100 * MIN);
        assert_eq!(quartile(24 * MIN, d), 0);
        assert_eq!(quartile(25 * MIN, d), 1);
        assert_eq!(quartile(74 * MIN, d), 2);
        assert_eq!(quartile(75 * MIN, d), 3);
        assert_eq!(quartile(200 * MIN, d), 3);
        assert_eq!(quartile(50 * MIN, None), 0);
    }
}
