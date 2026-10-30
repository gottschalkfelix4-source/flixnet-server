//! Live TV: tuner hosts, channels, XMLTV guide sources, guide channels and programs.
//!
//! Imports and guide refreshes compute everything first and write in short batches;
//! no transaction here waits on the network or ffmpeg.

use crate::{Db, Result, now_ms};

/// Rows per write transaction (imports, guide refreshes, cleanups).
pub const BATCH: usize = 200;

// ---- Tuner hosts -------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct TunerHost {
    pub id: i64,
    /// `m3u` | `xtream` | `hdhomerun`
    pub kind: String,
    pub name: String,
    pub url: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub user_agent: Option<String>,
    pub device_id: Option<String>,
    /// 0 = unlimited
    pub tuner_count: i64,
    pub tuner_count_manual: bool,
    pub enabled: bool,
    pub last_import_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Channels imported from this host.
    pub channel_count: i64,
}

macro_rules! host_columns {
    () => {
        "h.id, h.kind, h.name, h.url, h.username, h.password, h.user_agent, h.device_id,
         h.tuner_count, h.tuner_count_manual, h.enabled, h.last_import_at, h.last_error,
         h.created_at, h.updated_at,
         (SELECT COUNT(*) FROM channels c WHERE c.tuner_host_id = h.id) AS channel_count"
    };
}

#[derive(Debug, Clone, Default)]
pub struct NewTunerHost<'a> {
    pub kind: &'a str,
    pub name: &'a str,
    pub url: &'a str,
    pub username: Option<&'a str>,
    pub password: Option<&'a str>,
    pub user_agent: Option<&'a str>,
    pub device_id: Option<&'a str>,
    /// Set by the admin (`Some`), else learned from the source.
    pub tuner_count: Option<i64>,
}

pub async fn list_hosts(db: &Db) -> Result<Vec<TunerHost>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        host_columns!(),
        " FROM tuner_hosts h ORDER BY h.name, h.id"
    ))
    .fetch_all(db.reader())
    .await?)
}

pub async fn get_host(db: &Db, id: i64) -> Result<Option<TunerHost>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        host_columns!(),
        " FROM tuner_hosts h WHERE h.id = ?"
    ))
    .bind(id)
    .fetch_optional(db.reader())
    .await?)
}

pub async fn create_host(db: &Db, host: &NewTunerHost<'_>) -> Result<i64> {
    let now = now_ms();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO tuner_hosts (kind, name, url, username, password, user_agent, device_id,
             tuner_count, tuner_count_manual, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(host.kind)
    .bind(host.name)
    .bind(host.url)
    .bind(host.username)
    .bind(host.password)
    .bind(host.user_agent)
    .bind(host.device_id)
    .bind(host.tuner_count.unwrap_or(0))
    .bind(host.tuner_count.is_some())
    .bind(now)
    .bind(now)
    .fetch_one(db.writer())
    .await?;
    Ok(id)
}

/// Changes of a tuner host; `None` keeps a field. `Some(None)` clears optional ones,
/// `tuner_count: Some(None)` hands the count back to the source.
#[derive(Debug, Clone, Default)]
pub struct HostUpdate<'a> {
    pub name: Option<&'a str>,
    pub url: Option<&'a str>,
    pub username: Option<Option<&'a str>>,
    pub password: Option<Option<&'a str>>,
    pub user_agent: Option<Option<&'a str>>,
    pub tuner_count: Option<Option<i64>>,
    pub enabled: Option<bool>,
}

pub async fn update_host(db: &Db, id: i64, update: &HostUpdate<'_>) -> Result<bool> {
    let done = sqlx::query(
        "UPDATE tuner_hosts SET
            name = COALESCE(?, name),
            url = COALESCE(?, url),
            username = CASE WHEN ? THEN ? ELSE username END,
            password = CASE WHEN ? THEN ? ELSE password END,
            user_agent = CASE WHEN ? THEN ? ELSE user_agent END,
            tuner_count = CASE WHEN ? THEN COALESCE(?, tuner_count) ELSE tuner_count END,
            tuner_count_manual = CASE WHEN ? THEN ? ELSE tuner_count_manual END,
            enabled = COALESCE(?, enabled),
            updated_at = ?
         WHERE id = ?",
    )
    .bind(update.name)
    .bind(update.url)
    .bind(update.username.is_some())
    .bind(update.username.flatten())
    .bind(update.password.is_some())
    .bind(update.password.flatten())
    .bind(update.user_agent.is_some())
    .bind(update.user_agent.flatten())
    .bind(update.tuner_count.is_some())
    .bind(update.tuner_count.flatten())
    .bind(update.tuner_count.is_some())
    .bind(update.tuner_count.is_some_and(|c| c.is_some()))
    .bind(update.enabled)
    .bind(now_ms())
    .bind(id)
    .execute(db.writer())
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Deletes a host and its channels (channels in batches first).
pub async fn delete_host(db: &Db, id: i64) -> Result<bool> {
    loop {
        let removed = sqlx::query(
            "DELETE FROM channels WHERE id IN
                (SELECT id FROM channels WHERE tuner_host_id = ? LIMIT 1000)",
        )
        .bind(id)
        .execute(db.writer())
        .await?
        .rows_affected();
        if removed == 0 {
            break;
        }
    }
    let done = sqlx::query("DELETE FROM tuner_hosts WHERE id = ?")
        .bind(id)
        .execute(db.writer())
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Records the outcome of a channel import. `device_id`/`tuner_count` (learned from
/// the source) are stored when given; a manual tuner count is never overwritten.
pub async fn finish_import(
    db: &Db,
    id: i64,
    error: Option<&str>,
    device_id: Option<&str>,
    tuner_count: Option<i64>,
) -> Result<()> {
    sqlx::query(
        "UPDATE tuner_hosts SET
            last_import_at = ?,
            last_error = ?,
            device_id = COALESCE(?, device_id),
            tuner_count = CASE WHEN tuner_count_manual = 0 AND ? IS NOT NULL THEN ? ELSE tuner_count END,
            updated_at = ?
         WHERE id = ?",
    )
    .bind(now_ms())
    .bind(error)
    .bind(device_id)
    .bind(tuner_count)
    .bind(tuner_count)
    .bind(now_ms())
    .bind(id)
    .execute(db.writer())
    .await?;
    Ok(())
}

// ---- Channels ----------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Channel {
    pub id: i64,
    pub tuner_host_id: i64,
    pub remote_id: String,
    pub number: Option<String>,
    pub sort_order: f64,
    pub name: String,
    pub logo_url: Option<String>,
    pub group_title: Option<String>,
    pub stream_url: String,
    pub tvg_id: Option<String>,
    pub tvg_name: Option<String>,
    pub is_hd: bool,
    pub enabled: bool,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub audio_channels: Option<i64>,
    pub probed_at: Option<i64>,
    pub epg_source_id: Option<i64>,
    pub epg_channel_id: Option<String>,
    /// `auto` | `manual` | `None`
    pub epg_match: Option<String>,
    pub epg_match_score: Option<f64>,
    pub created_at: i64,
    pub updated_at: i64,
}

macro_rules! channel_columns {
    () => {
        "c.id, c.tuner_host_id, c.remote_id, c.number, c.sort_order, c.name, c.logo_url,
         c.group_title, c.stream_url, c.tvg_id, c.tvg_name, c.is_hd, c.enabled, c.video_codec,
         c.audio_codec, c.width, c.height, c.audio_channels, c.probed_at, c.epg_source_id,
         c.epg_channel_id, c.epg_match, c.epg_match_score, c.created_at, c.updated_at"
    };
}

/// A channel as a source lists it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewChannel {
    pub remote_id: String,
    pub number: Option<String>,
    pub sort_order: f64,
    pub name: String,
    pub logo_url: Option<String>,
    pub group_title: Option<String>,
    pub stream_url: String,
    pub tvg_id: Option<String>,
    pub tvg_name: Option<String>,
    pub is_hd: bool,
    /// Codecs announced by the source (HDHomeRun lineup), ffprobe names.
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
}

/// Channels in guide order. `enabled_only` hides disabled ones (viewers).
pub async fn list_channels(db: &Db, enabled_only: bool) -> Result<Vec<Channel>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        channel_columns!(),
        " FROM channels c JOIN tuner_hosts h ON h.id = c.tuner_host_id
          WHERE (? = 0 OR (c.enabled = 1 AND h.enabled = 1))
          ORDER BY c.sort_order, c.name, c.id"
    ))
    .bind(enabled_only)
    .fetch_all(db.reader())
    .await?)
}

pub async fn get_channel(db: &Db, id: i64) -> Result<Option<Channel>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        channel_columns!(),
        " FROM channels c WHERE c.id = ?"
    ))
    .bind(id)
    .fetch_optional(db.reader())
    .await?)
}

/// Writes an import: upserts the given channels (keeping guide matches, codecs and
/// the enabled flag) and removes channels the source no longer lists. Returns
/// (added or updated, removed).
pub async fn sync_channels(db: &Db, host_id: i64, channels: &[NewChannel]) -> Result<(u64, u64)> {
    let now = now_ms();
    let mut upserted = 0;
    for batch in channels.chunks(BATCH) {
        let mut tx = db.writer().begin().await?;
        for ch in batch {
            upserted += sqlx::query(
                "INSERT INTO channels (tuner_host_id, remote_id, number, sort_order, name, logo_url,
                     group_title, stream_url, tvg_id, tvg_name, is_hd, video_codec, audio_codec,
                     created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (tuner_host_id, remote_id) DO UPDATE SET
                     number = excluded.number,
                     sort_order = excluded.sort_order,
                     name = excluded.name,
                     logo_url = excluded.logo_url,
                     group_title = excluded.group_title,
                     -- A new stream URL may carry other codecs: probe again.
                     probed_at = CASE WHEN channels.stream_url = excluded.stream_url
                                      THEN channels.probed_at ELSE NULL END,
                     video_codec = COALESCE(excluded.video_codec,
                         CASE WHEN channels.stream_url = excluded.stream_url
                              THEN channels.video_codec END),
                     audio_codec = COALESCE(excluded.audio_codec,
                         CASE WHEN channels.stream_url = excluded.stream_url
                              THEN channels.audio_codec END),
                     stream_url = excluded.stream_url,
                     tvg_id = excluded.tvg_id,
                     tvg_name = excluded.tvg_name,
                     is_hd = excluded.is_hd,
                     updated_at = excluded.updated_at",
            )
            .bind(host_id)
            .bind(&ch.remote_id)
            .bind(&ch.number)
            .bind(ch.sort_order)
            .bind(&ch.name)
            .bind(&ch.logo_url)
            .bind(&ch.group_title)
            .bind(&ch.stream_url)
            .bind(&ch.tvg_id)
            .bind(&ch.tvg_name)
            .bind(ch.is_hd)
            .bind(&ch.video_codec)
            .bind(&ch.audio_codec)
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        }
        tx.commit().await?;
    }

    // Everything this import did not touch is gone from the source.
    let mut removed = 0;
    loop {
        let done = sqlx::query(
            "DELETE FROM channels WHERE id IN
                (SELECT id FROM channels WHERE tuner_host_id = ? AND updated_at < ? LIMIT 1000)",
        )
        .bind(host_id)
        .bind(now)
        .execute(db.writer())
        .await?
        .rows_affected();
        removed += done;
        if done == 0 {
            break;
        }
    }
    Ok((upserted, removed))
}

pub async fn set_channel_enabled(db: &Db, id: i64, enabled: bool) -> Result<bool> {
    let done = sqlx::query("UPDATE channels SET enabled = ? WHERE id = ?")
        .bind(enabled)
        .bind(id)
        .execute(db.writer())
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Stream properties found by probing (for the playback decision).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamProps {
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub audio_channels: Option<i64>,
}

pub async fn set_stream_props(db: &Db, id: i64, props: &StreamProps) -> Result<()> {
    sqlx::query(
        "UPDATE channels SET video_codec = ?, audio_codec = ?, width = ?, height = ?,
             audio_channels = ?, probed_at = ?
         WHERE id = ?",
    )
    .bind(&props.video_codec)
    .bind(&props.audio_codec)
    .bind(props.width)
    .bind(props.height)
    .bind(props.audio_channels)
    .bind(now_ms())
    .bind(id)
    .execute(db.writer())
    .await?;
    Ok(())
}

/// A guide match for one channel.
#[derive(Debug, Clone, PartialEq)]
pub struct EpgMatch {
    pub channel_id: i64,
    pub epg_source_id: Option<i64>,
    pub epg_channel_id: Option<String>,
    /// `auto` | `manual` | `None` (no match found)
    pub kind: Option<&'static str>,
    pub score: Option<f64>,
}

/// Stores guide matches. Automatic ones never replace a manual match.
pub async fn set_epg_matches(db: &Db, matches: &[EpgMatch]) -> Result<()> {
    for batch in matches.chunks(BATCH) {
        let mut tx = db.writer().begin().await?;
        for m in batch {
            sqlx::query(
                "UPDATE channels SET epg_source_id = ?, epg_channel_id = ?, epg_match = ?,
                     epg_match_score = ?
                 WHERE id = ? AND (? = 'manual' OR epg_match IS NULL OR epg_match <> 'manual')",
            )
            .bind(m.epg_source_id)
            .bind(&m.epg_channel_id)
            .bind(m.kind)
            .bind(m.score)
            .bind(m.channel_id)
            .bind(m.kind.unwrap_or(""))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
    }
    Ok(())
}

/// Hands a channel back to the automatic matcher.
pub async fn clear_manual_match(db: &Db, id: i64) -> Result<bool> {
    let done = sqlx::query(
        "UPDATE channels SET epg_source_id = NULL, epg_channel_id = NULL, epg_match = NULL,
             epg_match_score = NULL
         WHERE id = ?",
    )
    .bind(id)
    .execute(db.writer())
    .await?;
    Ok(done.rows_affected() > 0)
}

// ---- Guide sources -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct EpgSource {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub enabled: bool,
    pub generation: i64,
    pub channel_count: i64,
    pub program_count: i64,
    pub last_refresh_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

macro_rules! source_columns {
    () => {
        "id, name, url, enabled, generation, channel_count, program_count, last_refresh_at,
         last_error, created_at, updated_at"
    };
}

pub async fn list_sources(db: &Db) -> Result<Vec<EpgSource>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        source_columns!(),
        " FROM epg_sources ORDER BY name, id"
    ))
    .fetch_all(db.reader())
    .await?)
}

pub async fn get_source(db: &Db, id: i64) -> Result<Option<EpgSource>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        source_columns!(),
        " FROM epg_sources WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db.reader())
    .await?)
}

pub async fn create_source(db: &Db, name: &str, url: &str) -> Result<i64> {
    let now = now_ms();
    Ok(sqlx::query_scalar(
        "INSERT INTO epg_sources (name, url, created_at, updated_at) VALUES (?, ?, ?, ?)
         RETURNING id",
    )
    .bind(name)
    .bind(url)
    .bind(now)
    .bind(now)
    .fetch_one(db.writer())
    .await?)
}

pub async fn update_source(
    db: &Db,
    id: i64,
    name: Option<&str>,
    url: Option<&str>,
    enabled: Option<bool>,
) -> Result<bool> {
    let done = sqlx::query(
        "UPDATE epg_sources SET name = COALESCE(?, name), url = COALESCE(?, url),
             enabled = COALESCE(?, enabled), updated_at = ?
         WHERE id = ?",
    )
    .bind(name)
    .bind(url)
    .bind(enabled)
    .bind(now_ms())
    .bind(id)
    .execute(db.writer())
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Deletes a guide source with its programs (in batches) and unmatches its channels.
pub async fn delete_source(db: &Db, id: i64) -> Result<bool> {
    loop {
        let removed = sqlx::query(
            "DELETE FROM programs WHERE id IN
                (SELECT id FROM programs WHERE epg_source_id = ? LIMIT 2000)",
        )
        .bind(id)
        .execute(db.writer())
        .await?
        .rows_affected();
        if removed == 0 {
            break;
        }
    }
    sqlx::query(
        "UPDATE channels SET epg_source_id = NULL, epg_channel_id = NULL, epg_match = NULL,
             epg_match_score = NULL
         WHERE epg_source_id = ?",
    )
    .bind(id)
    .execute(db.writer())
    .await?;
    let done = sqlx::query("DELETE FROM epg_sources WHERE id = ?")
        .bind(id)
        .execute(db.writer())
        .await?;
    Ok(done.rows_affected() > 0)
}

pub async fn set_source_error(db: &Db, id: i64, error: &str) -> Result<()> {
    sqlx::query(
        "UPDATE epg_sources SET last_error = ?, last_refresh_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(error)
    .bind(now_ms())
    .bind(now_ms())
    .bind(id)
    .execute(db.writer())
    .await?;
    Ok(())
}

// ---- Guide channels ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct EpgChannel {
    pub epg_source_id: i64,
    pub channel_id: String,
    /// JSON array of names.
    pub display_names: String,
    pub icon_url: Option<String>,
}

impl EpgChannel {
    pub fn names(&self) -> Vec<String> {
        serde_json::from_str(&self.display_names).unwrap_or_default()
    }
}

/// Guide channels of all enabled sources (or one source).
pub async fn list_epg_channels(db: &Db, source: Option<i64>) -> Result<Vec<EpgChannel>> {
    Ok(sqlx::query_as(
        "SELECT e.epg_source_id, e.channel_id, e.display_names, e.icon_url
         FROM epg_channels e JOIN epg_sources s ON s.id = e.epg_source_id
         WHERE s.enabled = 1 AND (? IS NULL OR e.epg_source_id = ?)
         ORDER BY e.epg_source_id, e.display_names",
    )
    .bind(source)
    .bind(source)
    .fetch_all(db.reader())
    .await?)
}

/// Program written by a guide refresh.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewProgram {
    pub channel_id: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub title: String,
    pub subtitle: Option<String>,
    pub description: Option<String>,
    pub categories: Vec<String>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub episode_label: Option<String>,
    pub year: Option<i64>,
    pub icon_url: Option<String>,
    pub rating: Option<String>,
    pub is_new: bool,
    /// Series identity from the guide (dd_progid, Schedules Direct).
    pub series_id: Option<String>,
}

/// The next generation number for a refresh of `source` (rows written with it stay
/// invisible until [`finish_refresh`]).
pub async fn next_generation(db: &Db, source: i64) -> Result<i64> {
    let current: Option<i64> =
        sqlx::query_scalar("SELECT MAX(generation) FROM programs WHERE epg_source_id = ?")
            .bind(source)
            .fetch_one(db.reader())
            .await?;
    let active: i64 = sqlx::query_scalar("SELECT generation FROM epg_sources WHERE id = ?")
        .bind(source)
        .fetch_optional(db.reader())
        .await?
        .unwrap_or(0);
    Ok(current.unwrap_or(0).max(active) + 1)
}

/// Writes one batch of programs of a refresh (at most [`BATCH`] per transaction).
pub async fn insert_programs(
    db: &Db,
    source: i64,
    generation: i64,
    programs: &[NewProgram],
) -> Result<()> {
    for batch in programs.chunks(BATCH) {
        let mut tx = db.writer().begin().await?;
        for p in batch {
            sqlx::query(
                "INSERT INTO programs (epg_source_id, generation, channel_id, start_ms, end_ms,
                     title, subtitle, description, categories, season, episode, episode_label,
                     year, icon_url, rating, is_new, series_id)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(source)
            .bind(generation)
            .bind(&p.channel_id)
            .bind(p.start_ms)
            .bind(p.end_ms)
            .bind(&p.title)
            .bind(&p.subtitle)
            .bind(&p.description)
            .bind(serde_json::to_string(&p.categories)?)
            .bind(p.season)
            .bind(p.episode)
            .bind(&p.episode_label)
            .bind(p.year)
            .bind(&p.icon_url)
            .bind(&p.rating)
            .bind(p.is_new)
            .bind(&p.series_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
    }
    Ok(())
}

/// Guide channel of a refresh: `(id, display names, icon)`.
pub type NewEpgChannel = (String, Vec<String>, Option<String>);

/// Completes a refresh: replaces the source's guide channels, makes `generation` the
/// visible one and deletes older programs in batches.
pub async fn finish_refresh(
    db: &Db,
    source: i64,
    generation: i64,
    channels: &[NewEpgChannel],
) -> Result<()> {
    sqlx::query("DELETE FROM epg_channels WHERE epg_source_id = ?")
        .bind(source)
        .execute(db.writer())
        .await?;
    for batch in channels.chunks(BATCH) {
        let mut tx = db.writer().begin().await?;
        for (id, names, icon) in batch {
            sqlx::query(
                "INSERT INTO epg_channels (epg_source_id, channel_id, display_names, icon_url)
                 VALUES (?, ?, ?, ?)
                 ON CONFLICT (epg_source_id, channel_id) DO UPDATE SET
                     display_names = excluded.display_names, icon_url = excluded.icon_url",
            )
            .bind(source)
            .bind(id)
            .bind(serde_json::to_string(names)?)
            .bind(icon)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
    }
    let programs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM programs WHERE epg_source_id = ? AND generation = ?",
    )
    .bind(source)
    .bind(generation)
    .fetch_one(db.reader())
    .await?;
    sqlx::query(
        "UPDATE epg_sources SET generation = ?, channel_count = ?, program_count = ?,
             last_refresh_at = ?, last_error = NULL, updated_at = ?
         WHERE id = ?",
    )
    .bind(generation)
    .bind(channels.len() as i64)
    .bind(programs)
    .bind(now_ms())
    .bind(now_ms())
    .bind(source)
    .execute(db.writer())
    .await?;
    delete_stale_programs(db, source, generation).await
}

/// Removes programs of other generations than `keep` (old or aborted refreshes).
pub async fn delete_stale_programs(db: &Db, source: i64, keep: i64) -> Result<()> {
    loop {
        let removed = sqlx::query(
            "DELETE FROM programs WHERE id IN
                (SELECT id FROM programs WHERE epg_source_id = ? AND generation <> ? LIMIT 2000)",
        )
        .bind(source)
        .bind(keep)
        .execute(db.writer())
        .await?
        .rows_affected();
        if removed == 0 {
            return Ok(());
        }
    }
}

// ---- Programs ----------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Program {
    pub id: i64,
    /// The Flixnet channel showing it.
    pub channel: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub title: String,
    pub subtitle: Option<String>,
    pub description: Option<String>,
    /// JSON array.
    pub categories: String,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub episode_label: Option<String>,
    pub year: Option<i64>,
    pub icon_url: Option<String>,
    pub rating: Option<String>,
    pub is_new: bool,
    pub series_id: Option<String>,
}

impl Program {
    pub fn category_list(&self) -> Vec<String> {
        serde_json::from_str(&self.categories).unwrap_or_default()
    }
}

macro_rules! program_columns {
    () => {
        "p.id, c.id AS channel, p.start_ms, p.end_ms, p.title, p.subtitle, p.description,
         p.categories, p.season, p.episode, p.episode_label, p.year, p.icon_url, p.rating,
         p.is_new, p.series_id"
    };
}

/// Visible programs joined to the channels that show them.
macro_rules! program_join {
    () => {
        " FROM channels c
          JOIN epg_sources s ON s.id = c.epg_source_id AND s.enabled = 1
          JOIN programs p ON p.epg_source_id = c.epg_source_id
                         AND p.channel_id = c.epg_channel_id
                         AND p.generation = s.generation "
    };
}

/// Programs of the given channels overlapping `[from, to)`, by channel and start.
pub async fn programs_between(
    db: &Db,
    channel_ids: &[i64],
    from_ms: i64,
    to_ms: i64,
) -> Result<Vec<Program>> {
    if channel_ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        program_columns!(),
        program_join!(),
        "WHERE c.id IN (SELECT value FROM json_each(?))
           AND p.start_ms < ? AND p.end_ms > ?
         ORDER BY c.id, p.start_ms"
    ))
    .bind(serde_json::to_string(channel_ids)?)
    .bind(to_ms)
    .bind(from_ms)
    .fetch_all(db.reader())
    .await?)
}

pub async fn get_program(db: &Db, id: i64) -> Result<Option<Program>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        program_columns!(),
        program_join!(),
        "WHERE p.id = ? ORDER BY c.enabled DESC, c.sort_order LIMIT 1"
    ))
    .bind(id)
    .fetch_optional(db.reader())
    .await?)
}

/// Full-text search over programs that have not ended, soonest first. `fts` is an
/// FTS5 query (see [`crate::browse::fts_query`]).
pub async fn search_programs(db: &Db, fts: &str, now: i64, limit: i64) -> Result<Vec<Program>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        program_columns!(),
        program_join!(),
        "JOIN tuner_hosts h ON h.id = c.tuner_host_id AND h.enabled = 1
         WHERE p.id IN (SELECT rowid FROM programs_fts WHERE programs_fts MATCH ?)
           AND p.end_ms > ? AND c.enabled = 1
         ORDER BY p.start_ms, c.sort_order
         LIMIT ?"
    ))
    .bind(fts)
    .bind(now)
    .bind(limit)
    .fetch_all(db.reader())
    .await?)
}
