//! Persistence for the background job queue and scheduled tasks.

use crate::{Db, Result, now_ms};

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    pub pool: String,
    pub payload: String,
    pub priority: i64,
    pub state: String,
    pub attempts: i64,
    pub max_attempts: i64,
    pub run_after: i64,
    pub dedupe_key: Option<String>,
    pub progress: Option<f64>,
    pub error: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

macro_rules! job_columns {
    () => {
        "id, kind, pool, payload, priority, state, attempts, max_attempts, run_after,
         dedupe_key, progress, error, created_at, started_at, finished_at"
    };
}

pub struct NewJob<'a> {
    pub kind: &'a str,
    pub pool: &'a str,
    pub payload: &'a str,
    pub priority: i64,
    pub max_attempts: i64,
    pub dedupe_key: Option<&'a str>,
    pub run_after: Option<i64>,
}

/// Inserts a job. Returns `None` when an equal `dedupe_key` is already pending/running.
pub async fn enqueue(db: &Db, job: &NewJob<'_>) -> Result<Option<i64>> {
    let now = now_ms();
    let id: Option<(i64,)> = sqlx::query_as(
        "INSERT INTO jobs (kind, pool, payload, priority, max_attempts, dedupe_key, run_after, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT DO NOTHING
         RETURNING id",
    )
    .bind(job.kind)
    .bind(job.pool)
    .bind(job.payload)
    .bind(job.priority)
    .bind(job.max_attempts)
    .bind(job.dedupe_key)
    .bind(job.run_after.unwrap_or(now))
    .bind(now)
    .fetch_optional(db.writer())
    .await?;
    Ok(id.map(|(id,)| id))
}

/// Atomically takes the most important due job of a pool and marks it running.
pub async fn claim(db: &Db, pool: &str) -> Result<Option<Job>> {
    let now = now_ms();
    let job = sqlx::query_as(concat!(
        "UPDATE jobs SET state = 'running', attempts = attempts + 1, started_at = ?,
                         heartbeat_at = ?, error = NULL, progress = NULL
         WHERE id = (SELECT id FROM jobs
                     WHERE pool = ? AND state = 'queued' AND run_after <= ?
                     ORDER BY priority DESC, id LIMIT 1)
         RETURNING ",
        job_columns!()
    ))
    .bind(now)
    .bind(now)
    .bind(pool)
    .bind(now)
    .fetch_optional(db.writer())
    .await?;
    Ok(job)
}

/// Earliest `run_after` among queued jobs of a pool that are not due yet.
pub async fn next_due(db: &Db, pool: &str) -> Result<Option<i64>> {
    let (next,): (Option<i64>,) =
        sqlx::query_as("SELECT MIN(run_after) FROM jobs WHERE pool = ? AND state = 'queued'")
            .bind(pool)
            .fetch_one(db.reader())
            .await?;
    Ok(next)
}

pub async fn heartbeat(db: &Db, id: i64, progress: Option<f64>) -> Result<()> {
    sqlx::query(
        "UPDATE jobs SET heartbeat_at = ?, progress = COALESCE(?, progress)
         WHERE id = ? AND state = 'running'",
    )
    .bind(now_ms())
    .bind(progress)
    .bind(id)
    .execute(db.writer())
    .await?;
    Ok(())
}

pub async fn complete(db: &Db, id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE jobs SET state = 'done', progress = 1.0, finished_at = ?
         WHERE id = ? AND state = 'running'",
    )
    .bind(now_ms())
    .bind(id)
    .execute(db.writer())
    .await?;
    Ok(())
}

/// Records a failure: requeues with `retry_at` while attempts remain, else marks failed.
/// Returns the new state.
pub async fn fail(db: &Db, id: i64, error: &str, retry_at: i64) -> Result<String> {
    let (state,): (String,) = sqlx::query_as(
        "UPDATE jobs SET
            state = CASE WHEN attempts < max_attempts THEN 'queued' ELSE 'failed' END,
            run_after = CASE WHEN attempts < max_attempts THEN ? ELSE run_after END,
            finished_at = CASE WHEN attempts < max_attempts THEN NULL ELSE ? END,
            error = ?
         WHERE id = ?
         RETURNING state",
    )
    .bind(retry_at)
    .bind(now_ms())
    .bind(error)
    .bind(id)
    .fetch_one(db.writer())
    .await?;
    Ok(state)
}

/// Marks a job cancelled if it has not finished yet. Returns whether it changed.
pub async fn cancel(db: &Db, id: i64) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE jobs SET state = 'cancelled', finished_at = ?
         WHERE id = ? AND state IN ('queued', 'running')",
    )
    .bind(now_ms())
    .bind(id)
    .execute(db.writer())
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Puts a failed or cancelled job back into the queue with fresh attempts.
pub async fn retry(db: &Db, id: i64) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE jobs SET state = 'queued', attempts = 0, run_after = ?, error = NULL,
                         finished_at = NULL, progress = NULL
         WHERE id = ? AND state IN ('failed', 'cancelled')",
    )
    .bind(now_ms())
    .bind(id)
    .execute(db.writer())
    .await
    .map_err(crate::DbError::from_unique)?;
    Ok(result.rows_affected() > 0)
}

/// After a crash or restart: jobs left `running` by a dead process go back to the queue.
/// `stale_before` = heartbeat cut-off (use `i64::MAX` on startup to requeue all).
pub async fn requeue_stale(db: &Db, stale_before: i64) -> Result<u64> {
    let result = sqlx::query(
        "UPDATE jobs SET state = 'queued', run_after = ?
         WHERE state = 'running' AND COALESCE(heartbeat_at, 0) < ?",
    )
    .bind(now_ms())
    .bind(stale_before)
    .execute(db.writer())
    .await?;
    Ok(result.rows_affected())
}

pub async fn get(db: &Db, id: i64) -> Result<Option<Job>> {
    let job = sqlx::query_as(concat!(
        "SELECT ",
        job_columns!(),
        " FROM jobs WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db.reader())
    .await?;
    Ok(job)
}

/// Newest jobs first, optionally filtered by state.
pub async fn list(db: &Db, state: Option<&str>, limit: i64) -> Result<Vec<Job>> {
    let jobs = sqlx::query_as(concat!(
        "SELECT ",
        job_columns!(),
        " FROM jobs WHERE (? IS NULL OR state = ?)
          ORDER BY CASE state WHEN 'running' THEN 0 WHEN 'queued' THEN 1 ELSE 2 END,
                   COALESCE(finished_at, created_at) DESC, id DESC
          LIMIT ?"
    ))
    .bind(state)
    .bind(state)
    .bind(limit)
    .fetch_all(db.reader())
    .await?;
    Ok(jobs)
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Count {
    pub pool: String,
    pub state: String,
    pub count: i64,
}

pub async fn counts(db: &Db) -> Result<Vec<Count>> {
    let counts = sqlx::query_as(
        "SELECT pool, state, COUNT(*) AS count FROM jobs GROUP BY pool, state ORDER BY pool, state",
    )
    .fetch_all(db.reader())
    .await?;
    Ok(counts)
}

/// Deletes finished jobs older than the cut-off. Returns how many were removed.
pub async fn delete_finished_before(db: &Db, cutoff: i64) -> Result<u64> {
    let result = sqlx::query(
        "DELETE FROM jobs WHERE state IN ('done', 'cancelled', 'failed') AND finished_at < ?",
    )
    .bind(cutoff)
    .execute(db.writer())
    .await?;
    Ok(result.rows_affected())
}

// ---- Scheduled tasks --------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ScheduledTask {
    pub name: String,
    pub interval_ms: i64,
    pub enabled: bool,
    pub last_run: Option<i64>,
    pub next_run: i64,
}

/// Registers a task with its default interval; existing rows (user settings) are kept.
pub async fn ensure_task(db: &Db, name: &str, interval_ms: i64, first_run: i64) -> Result<()> {
    sqlx::query(
        "INSERT INTO scheduled_tasks (name, interval_ms, next_run) VALUES (?, ?, ?)
         ON CONFLICT (name) DO NOTHING",
    )
    .bind(name)
    .bind(interval_ms)
    .bind(first_run)
    .execute(db.writer())
    .await?;
    Ok(())
}

pub async fn list_tasks(db: &Db) -> Result<Vec<ScheduledTask>> {
    let tasks = sqlx::query_as(
        "SELECT name, interval_ms, enabled, last_run, next_run FROM scheduled_tasks ORDER BY name",
    )
    .fetch_all(db.reader())
    .await?;
    Ok(tasks)
}

/// Due, enabled tasks.
pub async fn due_tasks(db: &Db, now: i64) -> Result<Vec<ScheduledTask>> {
    let tasks = sqlx::query_as(
        "SELECT name, interval_ms, enabled, last_run, next_run FROM scheduled_tasks
         WHERE enabled = 1 AND next_run <= ?",
    )
    .bind(now)
    .fetch_all(db.reader())
    .await?;
    Ok(tasks)
}

/// Records a run and schedules the next one.
pub async fn mark_task_run(db: &Db, name: &str, now: i64) -> Result<()> {
    sqlx::query(
        "UPDATE scheduled_tasks SET last_run = ?, next_run = ? + interval_ms WHERE name = ?",
    )
    .bind(now)
    .bind(now)
    .bind(name)
    .execute(db.writer())
    .await?;
    Ok(())
}

pub async fn update_task(
    db: &Db,
    name: &str,
    interval_ms: Option<i64>,
    enabled: Option<bool>,
) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE scheduled_tasks SET
            interval_ms = COALESCE(?, interval_ms),
            enabled = COALESCE(?, enabled),
            next_run = CASE WHEN ? IS NOT NULL THEN COALESCE(last_run, ?) + ? ELSE next_run END
         WHERE name = ?",
    )
    .bind(interval_ms)
    .bind(enabled)
    .bind(interval_ms)
    .bind(now_ms())
    .bind(interval_ms)
    .bind(name)
    .execute(db.writer())
    .await?;
    Ok(result.rows_affected() > 0)
}
