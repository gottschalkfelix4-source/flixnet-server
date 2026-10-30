//! DVR: recording rules and recordings, plus the per-profile Live TV switch.
//!
//! The planner computes everything first and writes in short batches; the recorder
//! only updates single rows. No transaction here waits on ffmpeg or the disk.

use crate::{Db, Result, now_ms};

/// Rows per write transaction.
const BATCH: usize = 200;

// ---- Rules -------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct RecordingRule {
    pub id: i64,
    /// `program` | `series`
    pub kind: String,
    pub user_id: Option<i64>,
    pub title: String,
    pub series_id: Option<String>,
    pub channel_id: Option<i64>,
    pub program_start_ms: Option<i64>,
    pub new_only: bool,
    pub window_start_min: Option<i64>,
    pub window_end_min: Option<i64>,
    pub utc_offset_min: i64,
    pub padding_before_s: i64,
    pub padding_after_s: i64,
    pub priority: i64,
    pub keep_last: Option<i64>,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

macro_rules! rule_columns {
    () => {
        "id, kind, user_id, title, series_id, channel_id, program_start_ms, new_only,
         window_start_min, window_end_min, utc_offset_min, padding_before_s, padding_after_s,
         priority, keep_last, enabled, created_at, updated_at"
    };
}

#[derive(Debug, Clone, Default)]
pub struct NewRule<'a> {
    pub kind: &'a str,
    pub user_id: Option<i64>,
    pub title: &'a str,
    pub series_id: Option<&'a str>,
    pub channel_id: Option<i64>,
    pub program_start_ms: Option<i64>,
    pub new_only: bool,
    pub window: Option<(i64, i64)>,
    pub utc_offset_min: i64,
    pub padding_before_s: i64,
    pub padding_after_s: i64,
    pub priority: i64,
    pub keep_last: Option<i64>,
}

pub async fn create_rule(db: &Db, rule: &NewRule<'_>) -> Result<i64> {
    let now = now_ms();
    Ok(sqlx::query_scalar(
        "INSERT INTO recording_rules (kind, user_id, title, series_id, channel_id,
             program_start_ms, new_only, window_start_min, window_end_min, utc_offset_min,
             padding_before_s, padding_after_s, priority, keep_last, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(rule.kind)
    .bind(rule.user_id)
    .bind(rule.title)
    .bind(rule.series_id)
    .bind(rule.channel_id)
    .bind(rule.program_start_ms)
    .bind(rule.new_only)
    .bind(rule.window.map(|w| w.0))
    .bind(rule.window.map(|w| w.1))
    .bind(rule.utc_offset_min)
    .bind(rule.padding_before_s)
    .bind(rule.padding_after_s)
    .bind(rule.priority)
    .bind(rule.keep_last)
    .bind(now)
    .bind(now)
    .fetch_one(db.writer())
    .await?)
}

pub async fn list_rules(db: &Db) -> Result<Vec<RecordingRule>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        rule_columns!(),
        " FROM recording_rules ORDER BY kind DESC, title COLLATE NOCASE, id"
    ))
    .fetch_all(db.reader())
    .await?)
}

pub async fn get_rule(db: &Db, id: i64) -> Result<Option<RecordingRule>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        rule_columns!(),
        " FROM recording_rules WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db.reader())
    .await?)
}

/// Changes of a rule; `None` keeps a field, `Some(None)` clears optional ones.
#[derive(Debug, Clone, Default)]
pub struct RulePatch {
    pub channel_id: Option<Option<i64>>,
    pub new_only: Option<bool>,
    pub window: Option<Option<(i64, i64)>>,
    pub utc_offset_min: Option<i64>,
    pub padding_before_s: Option<i64>,
    pub padding_after_s: Option<i64>,
    pub priority: Option<i64>,
    pub keep_last: Option<Option<i64>>,
    pub enabled: Option<bool>,
}

pub async fn update_rule(db: &Db, id: i64, patch: &RulePatch) -> Result<bool> {
    let done = sqlx::query(
        "UPDATE recording_rules SET
            channel_id = CASE WHEN ? THEN ? ELSE channel_id END,
            new_only = COALESCE(?, new_only),
            window_start_min = CASE WHEN ? THEN ? ELSE window_start_min END,
            window_end_min = CASE WHEN ? THEN ? ELSE window_end_min END,
            utc_offset_min = COALESCE(?, utc_offset_min),
            padding_before_s = COALESCE(?, padding_before_s),
            padding_after_s = COALESCE(?, padding_after_s),
            priority = COALESCE(?, priority),
            keep_last = CASE WHEN ? THEN ? ELSE keep_last END,
            enabled = COALESCE(?, enabled),
            updated_at = ?
         WHERE id = ?",
    )
    .bind(patch.channel_id.is_some())
    .bind(patch.channel_id.flatten())
    .bind(patch.new_only)
    .bind(patch.window.is_some())
    .bind(patch.window.flatten().map(|w| w.0))
    .bind(patch.window.is_some())
    .bind(patch.window.flatten().map(|w| w.1))
    .bind(patch.utc_offset_min)
    .bind(patch.padding_before_s)
    .bind(patch.padding_after_s)
    .bind(patch.priority)
    .bind(patch.keep_last.is_some())
    .bind(patch.keep_last.flatten())
    .bind(patch.enabled)
    .bind(now_ms())
    .bind(id)
    .execute(db.writer())
    .await?;
    if done.rows_affected() > 0 {
        // Scheduled recordings take the rule's paddings and priority.
        sqlx::query(
            "UPDATE recordings SET
                 padding_before_s = (SELECT padding_before_s FROM recording_rules WHERE id = ?),
                 padding_after_s = (SELECT padding_after_s FROM recording_rules WHERE id = ?),
                 priority = (SELECT priority FROM recording_rules WHERE id = ?),
                 updated_at = ?
             WHERE rule_id = ? AND status = 'scheduled'",
        )
        .bind(id)
        .bind(id)
        .bind(id)
        .bind(now_ms())
        .bind(id)
        .execute(db.writer())
        .await?;
    }
    Ok(done.rows_affected() > 0)
}

/// Deletes a rule with its scheduled (and skipped) recordings; recordings that ran
/// stay (without rule).
pub async fn delete_rule(db: &Db, id: i64) -> Result<bool> {
    // One transaction: a planner run in between must not schedule for the rule again.
    let mut tx = db.writer().begin().await?;
    sqlx::query(
        "DELETE FROM recordings WHERE rule_id = ? AND status IN ('scheduled', 'cancelled')",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    let done = sqlx::query("DELETE FROM recording_rules WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(done.rows_affected() > 0)
}

// ---- Recordings --------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Recording {
    pub id: i64,
    pub rule_id: Option<i64>,
    /// Kind of the rule (`program` | `series`), if it still exists.
    pub rule_kind: Option<String>,
    pub channel_id: Option<i64>,
    pub channel_name: String,
    /// `scheduled` | `recording` | `completed` | `failed` | `cancelled`
    pub status: String,
    pub conflict: bool,
    pub title: String,
    pub subtitle: Option<String>,
    pub description: Option<String>,
    /// JSON array.
    pub categories: String,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub year: Option<i64>,
    pub rating: Option<String>,
    pub series_id: Option<String>,
    pub is_new: bool,
    pub icon_url: Option<String>,
    pub start_ms: i64,
    pub end_ms: i64,
    pub padding_before_s: i64,
    pub padding_after_s: i64,
    pub priority: i64,
    pub utc_offset_min: i64,
    pub path: Option<String>,
    pub parts: i64,
    pub bytes: i64,
    pub error: Option<String>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    /// The library item the recording became (once scanned).
    pub item_id: Option<i64>,
}

impl Recording {
    pub fn category_list(&self) -> Vec<String> {
        serde_json::from_str(&self.categories).unwrap_or_default()
    }

    /// When the recording starts and stops, paddings included.
    pub fn record_span(&self) -> (i64, i64) {
        (
            self.start_ms - self.padding_before_s * 1000,
            self.end_ms + self.padding_after_s * 1000,
        )
    }
}

macro_rules! recording_select {
    () => {
        "SELECT r.id, r.rule_id, rr.kind AS rule_kind, r.channel_id, r.channel_name, r.status,
             r.conflict, r.title, r.subtitle, r.description, r.categories, r.season, r.episode,
             r.year, r.rating, r.series_id, r.is_new, r.icon_url, r.start_ms, r.end_ms,
             r.padding_before_s, r.padding_after_s, r.priority, r.utc_offset_min, r.path,
             r.parts, r.bytes, r.error, r.started_at, r.finished_at, r.created_at, r.updated_at,
             (SELECT m.item_id FROM media_files m WHERE m.path = r.path) AS item_id
         FROM recordings r LEFT JOIN recording_rules rr ON rr.id = r.rule_id "
    };
}

/// A recording the planner (or a user) schedules.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewRecording {
    pub rule_id: i64,
    pub channel_id: i64,
    pub channel_name: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub description: Option<String>,
    pub categories: Vec<String>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub year: Option<i64>,
    pub rating: Option<String>,
    pub series_id: Option<String>,
    pub is_new: bool,
    pub icon_url: Option<String>,
    pub start_ms: i64,
    pub end_ms: i64,
    pub padding_before_s: i64,
    pub padding_after_s: i64,
    pub priority: i64,
    pub utc_offset_min: i64,
}

/// Schedules recordings: new slots are inserted, slots already scheduled by the same
/// rule get the guide's latest details. Slots another rule has, or that ran or were
/// cancelled, stay as they are. Returns the rows inserted or updated.
pub async fn upsert_scheduled(db: &Db, recordings: &[NewRecording]) -> Result<u64> {
    let now = now_ms();
    let mut changed = 0;
    for batch in recordings.chunks(BATCH) {
        let mut tx = db.writer().begin().await?;
        for r in batch {
            changed += sqlx::query(
                "INSERT INTO recordings (rule_id, channel_id, channel_name, status, title,
                     subtitle, description, categories, season, episode, year, rating, series_id,
                     is_new, icon_url, start_ms, end_ms, padding_before_s, padding_after_s,
                     priority, utc_offset_min, created_at, updated_at)
                 VALUES (?, ?, ?, 'scheduled', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (channel_id, start_ms) DO UPDATE SET
                     channel_name = excluded.channel_name,
                     title = excluded.title,
                     subtitle = excluded.subtitle,
                     description = excluded.description,
                     categories = excluded.categories,
                     season = excluded.season,
                     episode = excluded.episode,
                     year = excluded.year,
                     rating = excluded.rating,
                     series_id = excluded.series_id,
                     is_new = excluded.is_new,
                     icon_url = excluded.icon_url,
                     end_ms = excluded.end_ms,
                     padding_before_s = excluded.padding_before_s,
                     padding_after_s = excluded.padding_after_s,
                     priority = excluded.priority,
                     updated_at = excluded.updated_at
                 WHERE recordings.status = 'scheduled' AND recordings.rule_id = excluded.rule_id
                   AND (recordings.title IS NOT excluded.title
                        OR recordings.subtitle IS NOT excluded.subtitle
                        OR recordings.description IS NOT excluded.description
                        OR recordings.end_ms IS NOT excluded.end_ms
                        OR recordings.rating IS NOT excluded.rating
                        OR recordings.priority IS NOT excluded.priority
                        OR recordings.padding_before_s IS NOT excluded.padding_before_s
                        OR recordings.padding_after_s IS NOT excluded.padding_after_s)",
            )
            .bind(r.rule_id)
            .bind(r.channel_id)
            .bind(&r.channel_name)
            .bind(&r.title)
            .bind(&r.subtitle)
            .bind(&r.description)
            .bind(serde_json::to_string(&r.categories)?)
            .bind(r.season)
            .bind(r.episode)
            .bind(r.year)
            .bind(&r.rating)
            .bind(&r.series_id)
            .bind(r.is_new)
            .bind(&r.icon_url)
            .bind(r.start_ms)
            .bind(r.end_ms)
            .bind(r.padding_before_s)
            .bind(r.padding_after_s)
            .bind(r.priority)
            .bind(r.utc_offset_min)
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        }
        tx.commit().await?;
    }
    Ok(changed)
}

/// Removes scheduled recordings of a series rule that the guide no longer has
/// (`keep` = the slots it still wants, as `(channel_id, start_ms)`), from `after` on.
pub async fn delete_unwanted(
    db: &Db,
    rule_id: i64,
    keep: &[(i64, i64)],
    after: i64,
) -> Result<u64> {
    let keep: Vec<[i64; 2]> = keep.iter().map(|(c, s)| [*c, *s]).collect();
    Ok(sqlx::query(
        "DELETE FROM recordings
         WHERE rule_id = ? AND status = 'scheduled' AND start_ms > ?
           AND NOT EXISTS (SELECT 1 FROM json_each(?) k
                           WHERE json_extract(k.value, '$[0]') = recordings.channel_id
                             AND json_extract(k.value, '$[1]') = recordings.start_ms)",
    )
    .bind(rule_id)
    .bind(after)
    .bind(serde_json::to_string(&keep)?)
    .execute(db.writer())
    .await?
    .rows_affected())
}

/// Recordings by status (all when `statuses` is empty), newest broadcast last for
/// scheduled ones.
pub async fn list(db: &Db, statuses: &[&str]) -> Result<Vec<Recording>> {
    Ok(sqlx::query_as(concat!(
        recording_select!(),
        "WHERE (? = '[]' OR r.status IN (SELECT value FROM json_each(?)))
         ORDER BY r.start_ms, r.id"
    ))
    .bind(serde_json::to_string(statuses)?)
    .bind(serde_json::to_string(statuses)?)
    .fetch_all(db.reader())
    .await?)
}

pub async fn get(db: &Db, id: i64) -> Result<Option<Recording>> {
    Ok(
        sqlx::query_as(concat!(recording_select!(), "WHERE r.id = ?"))
            .bind(id)
            .fetch_optional(db.reader())
            .await?,
    )
}

/// The recording of a broadcast slot, if any.
pub async fn by_slot(db: &Db, channel_id: i64, start_ms: i64) -> Result<Option<Recording>> {
    Ok(sqlx::query_as(concat!(
        recording_select!(),
        "WHERE r.channel_id = ? AND r.start_ms = ?"
    ))
    .bind(channel_id)
    .bind(start_ms)
    .fetch_optional(db.reader())
    .await?)
}

/// Recordings of channels overlapping `[from, to)` (guide markers).
pub async fn between(db: &Db, from_ms: i64, to_ms: i64) -> Result<Vec<Recording>> {
    Ok(sqlx::query_as(concat!(
        recording_select!(),
        "WHERE r.start_ms < ? AND r.end_ms > ? AND r.status <> 'cancelled'"
    ))
    .bind(to_ms)
    .bind(from_ms)
    .fetch_all(db.reader())
    .await?)
}

/// Scheduled and running recordings that end after `now` (for conflicts).
pub async fn upcoming(db: &Db, now: i64) -> Result<Vec<Recording>> {
    Ok(sqlx::query_as(concat!(
        recording_select!(),
        "WHERE r.status IN ('scheduled', 'recording')
           AND r.end_ms + r.padding_after_s * 1000 > ?
         ORDER BY r.start_ms"
    ))
    .bind(now)
    .fetch_all(db.reader())
    .await?)
}

/// Marks exactly `conflicted` among the scheduled recordings as conflicts.
pub async fn set_conflicts(db: &Db, conflicted: &[i64]) -> Result<()> {
    sqlx::query(
        "UPDATE recordings SET conflict = (id IN (SELECT value FROM json_each(?)))
         WHERE status = 'scheduled'
           AND conflict <> (id IN (SELECT value FROM json_each(?)))",
    )
    .bind(serde_json::to_string(conflicted)?)
    .bind(serde_json::to_string(conflicted)?)
    .execute(db.writer())
    .await?;
    sqlx::query("UPDATE recordings SET conflict = 0 WHERE status <> 'scheduled' AND conflict = 1")
        .execute(db.writer())
        .await?;
    Ok(())
}

/// Recordings that should be running at `now`: scheduled ones (without conflict)
/// whose padded span has begun, and ones a server restart interrupted.
pub async fn due(db: &Db, now: i64) -> Result<Vec<Recording>> {
    Ok(sqlx::query_as(concat!(
        recording_select!(),
        "WHERE ((r.status = 'scheduled' AND r.conflict = 0) OR r.status = 'recording')
           AND r.start_ms - r.padding_before_s * 1000 <= ?
           AND r.end_ms + r.padding_after_s * 1000 > ?
         ORDER BY r.priority DESC, r.start_ms"
    ))
    .bind(now)
    .bind(now)
    .fetch_all(db.reader())
    .await?)
}

/// Recordings whose time passed without them running to the end: scheduled ones
/// never started (conflict, server down) and running ones the server lost.
pub async fn overdue(db: &Db, now: i64) -> Result<Vec<Recording>> {
    Ok(sqlx::query_as(concat!(
        recording_select!(),
        "WHERE r.status IN ('scheduled', 'recording')
           AND r.end_ms + r.padding_after_s * 1000 <= ?"
    ))
    .bind(now)
    .fetch_all(db.reader())
    .await?)
}

/// A recording (re)starts with part `parts` written to `path` (the first part).
pub async fn start_part(db: &Db, id: i64, path: &str, parts: i64) -> Result<()> {
    let now = now_ms();
    sqlx::query(
        "UPDATE recordings SET status = 'recording', conflict = 0, path = ?, parts = ?,
             error = NULL, started_at = COALESCE(started_at, ?), updated_at = ?
         WHERE id = ?",
    )
    .bind(path)
    .bind(parts)
    .bind(now)
    .bind(now)
    .bind(id)
    .execute(db.writer())
    .await?;
    Ok(())
}

pub async fn set_progress(db: &Db, id: i64, bytes: i64, error: Option<&str>) -> Result<()> {
    sqlx::query("UPDATE recordings SET bytes = ?, error = ?, updated_at = ? WHERE id = ?")
        .bind(bytes)
        .bind(error)
        .bind(now_ms())
        .bind(id)
        .execute(db.writer())
        .await?;
    Ok(())
}

/// Ends a recording (`completed` | `failed` | `cancelled`).
pub async fn finish(db: &Db, id: i64, status: &str, error: Option<&str>) -> Result<()> {
    let now = now_ms();
    sqlx::query(
        "UPDATE recordings SET status = ?, error = COALESCE(?, error), conflict = 0,
             finished_at = ?, updated_at = ?
         WHERE id = ?",
    )
    .bind(status)
    .bind(error)
    .bind(now)
    .bind(now)
    .bind(id)
    .execute(db.writer())
    .await?;
    Ok(())
}

pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    let done = sqlx::query("DELETE FROM recordings WHERE id = ?")
        .bind(id)
        .execute(db.writer())
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Episodes a rule has recorded (or is recording): `(season, episode, subtitle)`.
pub async fn rule_episodes(
    db: &Db,
    rule_id: i64,
) -> Result<Vec<(Option<i64>, Option<i64>, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT season, episode, subtitle FROM recordings
         WHERE rule_id = ? AND status IN ('recording', 'completed')",
    )
    .bind(rule_id)
    .fetch_all(db.reader())
    .await?)
}

/// Completed recordings of a rule beyond the newest `keep` (retention).
pub async fn beyond_keep(db: &Db, rule_id: i64, keep: i64) -> Result<Vec<Recording>> {
    Ok(sqlx::query_as(concat!(
        recording_select!(),
        "WHERE r.rule_id = ? AND r.status = 'completed'
         ORDER BY r.start_ms DESC, r.id DESC LIMIT -1 OFFSET ?"
    ))
    .bind(rule_id)
    .bind(keep.max(0))
    .fetch_all(db.reader())
    .await?)
}

// ---- Live TV access per profile ----------------------------------------------------

/// Whether a profile may use Live TV (unknown users: no).
pub async fn live_tv_allowed(db: &Db, user_id: i64) -> Result<bool> {
    Ok(
        sqlx::query_scalar("SELECT live_tv_allowed FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(db.reader())
            .await?
            .unwrap_or(false),
    )
}

pub async fn set_live_tv_allowed(db: &Db, user_id: i64, allowed: bool) -> Result<bool> {
    let done = sqlx::query("UPDATE users SET live_tv_allowed = ? WHERE id = ?")
        .bind(allowed)
        .bind(user_id)
        .execute(db.writer())
        .await?;
    Ok(done.rows_affected() > 0)
}
