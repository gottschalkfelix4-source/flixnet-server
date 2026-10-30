//! `/api/v1/jobs` and `/api/v1/tasks`: background work, for administrators.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use flixnet_db::jobs;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AdminUser;
use crate::error::{ApiError, ApiResult, ErrorBody};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_jobs))
        .routes(routes!(job_summary))
        .routes(routes!(get_activity))
        .routes(routes!(cancel_job))
        .routes(routes!(retry_job))
        .routes(routes!(list_tasks))
        .routes(routes!(update_task))
        .routes(routes!(run_task))
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl JobState {
    fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Done => "done",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct JobsQuery {
    /// Only jobs in this state.
    #[param(inline)]
    pub state: Option<JobState>,
    /// Maximum number of jobs (1–500, default 100).
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct JobDto {
    pub id: i64,
    pub kind: String,
    pub pool: String,
    pub state: String,
    pub priority: i64,
    pub attempts: i64,
    pub max_attempts: i64,
    pub progress: Option<f64>,
    pub error: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

#[derive(Debug, Default, Serialize, ToSchema)]
pub struct PoolSummary {
    pub pool: String,
    pub queued: i64,
    pub running: i64,
    pub failed: i64,
    pub done: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TaskDto {
    pub name: String,
    pub description: String,
    pub interval_ms: i64,
    pub enabled: bool,
    pub last_run: Option<i64>,
    pub next_run: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateTaskRequest {
    /// New interval (1 minute to 30 days).
    pub interval_ms: Option<i64>,
    pub enabled: Option<bool>,
}

/// Recent and pending background jobs.
#[utoipa::path(
    get,
    path = "/jobs",
    operation_id = "listJobs",
    tag = "jobs",
    params(JobsQuery),
    responses(
        (status = OK, description = "Running first, then queued, then finished (newest first)", body = Vec<JobDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn list_jobs(
    _: AdminUser,
    State(state): State<AppState>,
    Query(query): Query<JobsQuery>,
) -> ApiResult<Json<Vec<JobDto>>> {
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let list = jobs::list(&state.db, query.state.map(JobState::as_str), limit).await?;
    Ok(Json(list.into_iter().map(JobDto::from).collect()))
}

/// What the server is busy with, for the activity indicator in the top bar.
#[derive(Debug, Serialize, ToSchema)]
pub struct ActivityDto {
    /// Job kind, e.g. `library.scan`, `media.probe` or `metadata.refresh`.
    pub kind: String,
    /// Scans: the library being scanned.
    pub library_id: Option<i64>,
    pub library_name: Option<String>,
    pub running: i64,
    pub queued: i64,
    /// Finished since this batch began.
    pub done: i64,
    /// 0–1 when known (share of the batch, or what a single job reported).
    pub progress: Option<f64>,
}

/// Kinds that are bookkeeping rather than server work worth showing.
const QUIET_KINDS: &[&str] = &["notify.deliver", "maintenance.cleanup_jobs"];

/// Scans per library, then every other kind of work as one batch with its progress.
#[utoipa::path(
    get,
    path = "/jobs/activity",
    operation_id = "getServerActivity",
    tag = "jobs",
    responses(
        (status = OK, description = "Current server activity, oldest first; empty when idle", body = Vec<ActivityDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn get_activity(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<ActivityDto>>> {
    let now = flixnet_db::now_ms();
    let kinds = jobs::activity(&state.db, now).await?;
    let mut out = Vec::new();
    for kind in kinds {
        if QUIET_KINDS.contains(&kind.kind.as_str()) {
            continue;
        }
        if kind.kind == "library.scan" {
            let libraries = flixnet_db::libraries::list(&state.db).await?;
            for job in jobs::pending_of_kind(&state.db, &kind.kind, now).await? {
                let library_id = serde_json::from_str::<serde_json::Value>(&job.payload)
                    .ok()
                    .and_then(|p| p.get("library_id").and_then(serde_json::Value::as_i64));
                let running = job.state == "running";
                out.push(ActivityDto {
                    kind: job.kind,
                    library_name: library_id
                        .and_then(|id| libraries.iter().find(|l| l.id == id))
                        .map(|l| l.name.clone()),
                    library_id,
                    running: i64::from(running),
                    queued: i64::from(!running),
                    done: 0,
                    progress: job.progress,
                });
            }
            continue;
        }
        let total = kind.running + kind.queued + kind.done;
        let progress = if total > 1 {
            Some(kind.done as f64 / total as f64)
        } else {
            kind.progress
        };
        out.push(ActivityDto {
            kind: kind.kind,
            library_id: None,
            library_name: None,
            running: kind.running,
            queued: kind.queued,
            done: kind.done,
            progress,
        });
    }
    Ok(Json(out))
}

/// Job counts per pool and state.
#[utoipa::path(
    get,
    path = "/jobs/summary",
    operation_id = "getJobSummary",
    tag = "jobs",
    responses(
        (status = OK, description = "One entry per worker pool", body = Vec<PoolSummary>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn job_summary(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<PoolSummary>>> {
    let counts = jobs::counts(&state.db).await?;
    let summary = flixnet_core::jobs::Pool::ALL
        .iter()
        .map(|pool| {
            let mut entry = PoolSummary {
                pool: pool.as_str().to_owned(),
                ..Default::default()
            };
            for c in counts.iter().filter(|c| c.pool == pool.as_str()) {
                match c.state.as_str() {
                    "queued" => entry.queued = c.count,
                    "running" => entry.running = c.count,
                    "failed" => entry.failed = c.count,
                    "done" => entry.done = c.count,
                    _ => {}
                }
            }
            entry
        })
        .collect();
    Ok(Json(summary))
}

/// Cancel a queued or running job.
#[utoipa::path(
    post,
    path = "/jobs/{id}/cancel",
    operation_id = "cancelJob",
    tag = "jobs",
    params(("id" = i64, Path, description = "Job id")),
    responses(
        (status = NO_CONTENT, description = "Cancelled"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such pending or running job", body = ErrorBody),
    )
)]
async fn cancel_job(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    if state.jobs.cancel(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "Keine laufende oder wartende Aufgabe mit dieser ID.",
        ))
    }
}

/// Run a failed or cancelled job again.
#[utoipa::path(
    post,
    path = "/jobs/{id}/retry",
    operation_id = "retryJob",
    tag = "jobs",
    params(("id" = i64, Path, description = "Job id")),
    responses(
        (status = NO_CONTENT, description = "Queued again"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such failed or cancelled job", body = ErrorBody),
        (status = CONFLICT, description = "An equal job is already pending", body = ErrorBody),
    )
)]
async fn retry_job(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    match state.jobs.retry(id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "Keine fehlgeschlagene oder abgebrochene Aufgabe mit dieser ID.",
        )),
        Err(flixnet_db::DbError::Conflict) => Err(ApiError::conflict(
            "already_queued",
            "Eine gleiche Aufgabe wartet bereits.",
        )),
        Err(err) => Err(err.into()),
    }
}

/// Recurring maintenance tasks.
#[utoipa::path(
    get,
    path = "/tasks",
    operation_id = "listTasks",
    tag = "jobs",
    responses(
        (status = OK, description = "All scheduled tasks", body = Vec<TaskDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn list_tasks(_: AdminUser, State(state): State<AppState>) -> ApiResult<Json<Vec<TaskDto>>> {
    Ok(Json(task_dtos(&state).await?))
}

/// Change a task's interval or switch it on/off.
#[utoipa::path(
    patch,
    path = "/tasks/{name}",
    operation_id = "updateTask",
    tag = "jobs",
    params(("name" = String, Path, description = "Task name")),
    request_body = UpdateTaskRequest,
    responses(
        (status = OK, description = "Updated task", body = TaskDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such task", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Interval out of range", body = ErrorBody),
    )
)]
async fn update_task(
    _: AdminUser,
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<UpdateTaskRequest>,
) -> ApiResult<Json<TaskDto>> {
    const MINUTE: i64 = 60_000;
    if body
        .interval_ms
        .is_some_and(|ms| !(MINUTE..=30 * 24 * 60 * MINUTE).contains(&ms))
    {
        return Err(ApiError::validation(
            "Das Intervall muss zwischen 1 Minute und 30 Tagen liegen.",
        ));
    }
    let def = state
        .tasks
        .get(&name)
        .ok_or_else(|| ApiError::not_found("Aufgabe nicht gefunden."))?;
    let interval = def.default_interval.as_millis() as i64;
    jobs::ensure_task(
        &state.db,
        def.name,
        interval,
        flixnet_db::now_ms() + interval,
    )
    .await?;
    jobs::update_task(&state.db, &name, body.interval_ms, body.enabled).await?;
    let task = task_dtos(&state)
        .await?
        .into_iter()
        .find(|t| t.name == name)
        .ok_or_else(|| ApiError::not_found("Aufgabe nicht gefunden."))?;
    Ok(Json(task))
}

/// Run a task now.
#[utoipa::path(
    post,
    path = "/tasks/{name}/run",
    operation_id = "runTask",
    tag = "jobs",
    params(("name" = String, Path, description = "Task name")),
    responses(
        (status = ACCEPTED, description = "Queued"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such task", body = ErrorBody),
    )
)]
async fn run_task(
    _: AdminUser,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    if let Some(def) = state.tasks.get(&name) {
        let interval = def.default_interval.as_millis() as i64;
        jobs::ensure_task(
            &state.db,
            def.name,
            interval,
            flixnet_db::now_ms() + interval,
        )
        .await?;
    }
    if state
        .tasks
        .run_now(&state.db, state.jobs.queue(), &name)
        .await?
    {
        Ok(StatusCode::ACCEPTED)
    } else {
        Err(ApiError::not_found("Aufgabe nicht gefunden."))
    }
}

/// Registered task definitions merged with their stored schedule.
async fn task_dtos(state: &AppState) -> ApiResult<Vec<TaskDto>> {
    let rows = jobs::list_tasks(&state.db).await?;
    Ok(state
        .tasks
        .all()
        .iter()
        .map(|def| {
            let row = rows.iter().find(|r| r.name == def.name);
            TaskDto {
                name: def.name.to_owned(),
                description: def.description.to_owned(),
                interval_ms: row.map_or(def.default_interval.as_millis() as i64, |r| r.interval_ms),
                enabled: row.is_none_or(|r| r.enabled),
                last_run: row.and_then(|r| r.last_run),
                next_run: row.map(|r| r.next_run),
            }
        })
        .collect())
}

impl From<jobs::Job> for JobDto {
    fn from(j: jobs::Job) -> Self {
        Self {
            id: j.id,
            kind: j.kind,
            pool: j.pool,
            state: j.state,
            priority: j.priority,
            attempts: j.attempts,
            max_attempts: j.max_attempts,
            progress: j.progress,
            error: j.error,
            created_at: j.created_at,
            started_at: j.started_at,
            finished_at: j.finished_at,
        }
    }
}
