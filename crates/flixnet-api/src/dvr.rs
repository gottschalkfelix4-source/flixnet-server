//! `/api/v1/livetv/recordings`, `/livetv/recording-rules`, `/livetv/profiles`: the
//! DVR (record a program, record a series, stop, cancel, delete), and Live TV access
//! per profile.
//!
//! Parental controls for Live TV live here too: a profile may be barred from Live TV
//! altogether, and programs (and recordings) are checked against the profile's
//! restriction by their guide age rating (`FSK 12`, `TV-14`, …) and categories.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use flixnet_core::jobs::priority;
use flixnet_db::browse::Restriction;
use flixnet_db::dvr::{self, NewRule, Recording, RecordingRule, RulePatch};
use flixnet_db::livetv;
use flixnet_livetv::config::{self, MAX_PADDING_MINUTES};
use flixnet_livetv::dvr::{FinishedHook, naming, plan};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::{AdminUser, AuthUser};
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::validate;

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_recordings, create_recording))
        .routes(routes!(get_recording, delete_recording))
        .routes(routes!(stop_recording))
        .routes(routes!(list_rules, create_rule))
        .routes(routes!(update_rule, delete_rule))
        .routes(routes!(list_profiles))
        .routes(routes!(set_profile_access))
}

// ---- Parental controls -------------------------------------------------------------

/// 403 unless the profile may use Live TV.
pub(crate) async fn live_viewer(state: &AppState, user: &AuthUser) -> ApiResult<()> {
    if dvr::live_tv_allowed(&state.db, user.user_id).await? {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "live_tv_forbidden",
            "Live TV ist für dieses Profil gesperrt.",
        ))
    }
}

/// Whether a program (or recording) with this guide rating and categories may be
/// shown to a profile with `restriction`.
pub(crate) fn program_allowed(
    restriction: &Restriction,
    rating: Option<&str>,
    categories: &[String],
) -> bool {
    if !restriction.is_restricted() {
        return true;
    }
    let blocked = categories.iter().any(|c| {
        restriction
            .blocked_genres
            .iter()
            .any(|g| g.eq_ignore_ascii_case(c.trim()))
    });
    if blocked {
        return false;
    }
    match (
        restriction.max_age,
        rating.and_then(flixnet_library::nfo::age_from_certification),
    ) {
        (None, _) => true,
        (Some(max), Some(age)) => age <= max,
        (Some(_), None) => restriction.allow_unrated,
    }
}

// ---- DTOs --------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecordingStatus {
    Scheduled,
    Recording,
    Completed,
    Failed,
    Cancelled,
}

impl RecordingStatus {
    fn parse(status: &str) -> Self {
        match status {
            "recording" => RecordingStatus::Recording,
            "completed" => RecordingStatus::Completed,
            "failed" => RecordingStatus::Failed,
            "cancelled" => RecordingStatus::Cancelled,
            _ => RecordingStatus::Scheduled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecordingRuleKind {
    /// One broadcast.
    Program,
    /// Every broadcast of a series.
    Series,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RecordingDto {
    pub id: i64,
    pub rule_id: Option<i64>,
    pub rule_kind: Option<RecordingRuleKind>,
    pub channel_id: Option<i64>,
    pub channel_name: String,
    pub status: RecordingStatus,
    /// Scheduled, but no tuner is left for it (raise the rule's priority).
    pub conflict: bool,
    pub title: String,
    pub subtitle: Option<String>,
    pub description: Option<String>,
    pub categories: Vec<String>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub year: Option<i64>,
    pub rating: Option<String>,
    pub is_new: bool,
    /// The broadcast (unix ms).
    pub start_ms: i64,
    pub end_ms: i64,
    pub padding_before_s: i64,
    pub padding_after_s: i64,
    pub priority: i64,
    /// Bytes recorded so far.
    pub bytes: i64,
    /// Files: more than one when a server restart interrupted the recording.
    pub parts: i64,
    pub error: Option<String>,
    /// The library item the recording became (once scanned).
    pub item_id: Option<i64>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

fn recording_dto(r: Recording) -> RecordingDto {
    RecordingDto {
        categories: r.category_list(),
        rule_kind: match r.rule_kind.as_deref() {
            Some("program") => Some(RecordingRuleKind::Program),
            Some("series") => Some(RecordingRuleKind::Series),
            _ => None,
        },
        status: RecordingStatus::parse(&r.status),
        id: r.id,
        rule_id: r.rule_id,
        channel_id: r.channel_id,
        channel_name: r.channel_name,
        conflict: r.conflict,
        title: r.title,
        subtitle: r.subtitle,
        description: r.description,
        season: r.season,
        episode: r.episode,
        year: r.year,
        rating: r.rating,
        is_new: r.is_new,
        start_ms: r.start_ms,
        end_ms: r.end_ms,
        padding_before_s: r.padding_before_s,
        padding_after_s: r.padding_after_s,
        priority: r.priority,
        bytes: r.bytes,
        parts: r.parts,
        error: r.error,
        item_id: r.item_id,
        started_at: r.started_at,
        finished_at: r.finished_at,
    }
}

fn visible(user: &AuthUser, r: &Recording) -> bool {
    program_allowed(&user.restriction, r.rating.as_deref(), &r.category_list())
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RecordingRuleDto {
    pub id: i64,
    pub kind: RecordingRuleKind,
    pub title: String,
    /// Series rules match this guide series id instead of the title.
    pub series_id: Option<String>,
    /// Program rules: the channel; series rules: only this channel (`null` = all).
    pub channel_id: Option<i64>,
    pub channel_name: Option<String>,
    pub new_only: bool,
    /// Minutes after local midnight; both set = only broadcasts starting within.
    pub window_start_min: Option<i64>,
    pub window_end_min: Option<i64>,
    /// Local time = UTC + this many minutes (time window, air dates).
    pub utc_offset_min: i64,
    pub padding_before_s: i64,
    pub padding_after_s: i64,
    /// Higher wins when tuners run short.
    pub priority: i64,
    /// Keep only the newest N recordings (`null` = all).
    pub keep_last: Option<i64>,
    pub enabled: bool,
    /// Recordings scheduled by the rule.
    pub scheduled: i64,
    /// Scheduled recordings without a tuner.
    pub conflicts: i64,
    /// Next scheduled broadcast (unix ms).
    pub next_start_ms: Option<i64>,
    /// Created by the signed-in user (or the caller is an administrator).
    pub can_edit: bool,
}

async fn rule_dto(
    state: &AppState,
    user: &AuthUser,
    rule: RecordingRule,
    scheduled: &[Recording],
) -> ApiResult<RecordingRuleDto> {
    let mine: Vec<&Recording> = scheduled
        .iter()
        .filter(|r| r.rule_id == Some(rule.id))
        .collect();
    let channel_name = match rule.channel_id {
        Some(id) => livetv::get_channel(&state.db, id).await?.map(|c| c.name),
        None => None,
    };
    Ok(RecordingRuleDto {
        kind: if rule.kind == "program" {
            RecordingRuleKind::Program
        } else {
            RecordingRuleKind::Series
        },
        scheduled: mine.len() as i64,
        conflicts: mine.iter().filter(|r| r.conflict).count() as i64,
        next_start_ms: mine.iter().map(|r| r.start_ms).min(),
        can_edit: user.is_admin || rule.user_id == Some(user.user_id),
        channel_name,
        id: rule.id,
        title: rule.title,
        series_id: rule.series_id,
        channel_id: rule.channel_id,
        new_only: rule.new_only,
        window_start_min: rule.window_start_min,
        window_end_min: rule.window_end_min,
        utc_offset_min: rule.utc_offset_min,
        padding_before_s: rule.padding_before_s,
        padding_after_s: rule.padding_after_s,
        priority: rule.priority,
        keep_last: rule.keep_last,
        enabled: rule.enabled,
    })
}

fn not_found() -> ApiError {
    ApiError::not_found("Aufnahme nicht gefunden.")
}

fn rule_not_found() -> ApiError {
    ApiError::not_found("Aufnahmeregel nicht gefunden.")
}

fn validate_padding(secs: Option<i64>) -> ApiResult<()> {
    match secs {
        Some(s) if !(0..=MAX_PADDING_MINUTES * 60).contains(&s) => Err(ApiError::validation(
            "Vor- und Nachlauf dürfen höchstens 120 Minuten lang sein.",
        )),
        _ => Ok(()),
    }
}

fn validate_offset(offset: Option<i64>) -> ApiResult<i64> {
    match offset {
        Some(o) if !(-15 * 60..=15 * 60).contains(&o) => {
            Err(ApiError::validation("Ungültige Zeitzone."))
        }
        Some(o) => Ok(o),
        None => Ok(60),
    }
}

fn validate_window(start: Option<i64>, end: Option<i64>) -> ApiResult<Option<(i64, i64)>> {
    match (start, end) {
        (None, None) => Ok(None),
        (Some(s), Some(e)) if (0..1440).contains(&s) && (0..=1440).contains(&e) && s != e => {
            Ok(Some((s, e)))
        }
        _ => Err(ApiError::validation(
            "Das Zeitfenster braucht einen Beginn und ein Ende (00:00–24:00).",
        )),
    }
}

fn validate_priority(priority: Option<i64>) -> ApiResult<()> {
    match priority {
        Some(p) if !(-100..=100).contains(&p) => Err(ApiError::validation(
            "Die Priorität muss zwischen -100 und 100 liegen.",
        )),
        _ => Ok(()),
    }
}

fn validate_keep(keep: Option<i64>) -> ApiResult<()> {
    match keep {
        Some(k) if !(1..=1000).contains(&k) => Err(ApiError::validation(
            "Behalten: zwischen 1 und 1000 Aufnahmen.",
        )),
        _ => Ok(()),
    }
}

/// May change this rule (or its recordings): administrators and its creator.
fn may_edit(user: &AuthUser, rule_user: Option<i64>) -> ApiResult<()> {
    if user.is_admin || rule_user == Some(user.user_id) {
        Ok(())
    } else {
        Err(ApiError::forbidden())
    }
}

// ---- Recordings --------------------------------------------------------------------

/// All recordings the profile may see: scheduled, running, done, failed (with
/// conflicts flagged).
#[utoipa::path(
    get,
    path = "/livetv/recordings",
    operation_id = "listRecordings",
    tag = "livetv",
    responses(
        (status = OK, description = "Recordings in broadcast order", body = [RecordingDto]),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Live TV is barred for this profile", body = ErrorBody),
    )
)]
async fn list_recordings(
    user: AuthUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<RecordingDto>>> {
    live_viewer(&state, &user).await?;
    let all = dvr::list(&state.db, &[]).await?;
    Ok(Json(
        all.into_iter()
            .filter(|r| visible(&user, r))
            .map(recording_dto)
            .collect(),
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateRecordingRequest {
    /// The program to record (from the guide).
    pub program_id: i64,
    /// Default: from the Live TV settings.
    pub padding_before_s: Option<i64>,
    pub padding_after_s: Option<i64>,
    /// The client's offset from UTC in minutes (air dates in file names).
    pub utc_offset_min: Option<i64>,
}

/// Records one program ("Aufnehmen"): creates a program rule and schedules it — or
/// starts at once when the program is running.
#[utoipa::path(
    post,
    path = "/livetv/recordings",
    operation_id = "createRecording",
    tag = "livetv",
    request_body = CreateRecordingRequest,
    responses(
        (status = CREATED, description = "Scheduled (see `conflict`)", body = RecordingDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Live TV is barred for this profile", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown program", body = ErrorBody),
        (status = CONFLICT, description = "The program is recorded already", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Over, or invalid padding", body = ErrorBody),
    )
)]
async fn create_recording(
    user: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<CreateRecordingRequest>,
) -> ApiResult<(StatusCode, Json<RecordingDto>)> {
    live_viewer(&state, &user).await?;
    validate_padding(body.padding_before_s)?;
    validate_padding(body.padding_after_s)?;
    let offset = validate_offset(body.utc_offset_min)?;
    let program = livetv::get_program(&state.db, body.program_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Sendung nicht gefunden."))?;
    if !program_allowed(
        &user.restriction,
        program.rating.as_deref(),
        &program.category_list(),
    ) {
        return Err(ApiError::not_found("Sendung nicht gefunden."));
    }
    let channel = livetv::get_channel(&state.db, program.channel)
        .await?
        .ok_or_else(|| ApiError::not_found("Sender nicht gefunden."))?;
    if program.end_ms <= flixnet_db::now_ms() {
        return Err(ApiError::validation("Die Sendung ist schon vorbei."));
    }
    if let Some(existing) = dvr::by_slot(&state.db, channel.id, program.start_ms).await? {
        if existing.status == "cancelled" {
            dvr::delete(&state.db, existing.id).await?;
        } else {
            return Err(ApiError::conflict(
                "already_recorded",
                "Diese Sendung wird schon aufgenommen.",
            ));
        }
    }
    let settings = config::load(&state.db).await?;
    let rule = NewRule {
        kind: "program",
        user_id: Some(user.user_id),
        title: &program.title,
        series_id: program.series_id.as_deref(),
        channel_id: Some(channel.id),
        program_start_ms: Some(program.start_ms),
        utc_offset_min: offset,
        padding_before_s: body
            .padding_before_s
            .unwrap_or(settings.padding_before_minutes * 60),
        padding_after_s: body
            .padding_after_s
            .unwrap_or(settings.padding_after_minutes * 60),
        ..NewRule::default()
    };
    let rule_id = dvr::create_rule(&state.db, &rule).await?;
    let rule = dvr::get_rule(&state.db, rule_id)
        .await?
        .ok_or_else(rule_not_found)?;
    dvr::upsert_scheduled(&state.db, &[plan::new_recording(&rule, &program, &channel)]).await?;
    plan::update_conflicts(&state.db).await?;
    state.dvr.replan();
    let recording = dvr::by_slot(&state.db, channel.id, program.start_ms)
        .await?
        .ok_or_else(not_found)?;
    Ok((StatusCode::CREATED, Json(recording_dto(recording))))
}

/// One recording.
#[utoipa::path(
    get,
    path = "/livetv/recordings/{id}",
    operation_id = "getRecording",
    tag = "livetv",
    params(("id" = i64, Path, description = "Recording id")),
    responses(
        (status = OK, description = "The recording", body = RecordingDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Live TV is barred for this profile", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown recording", body = ErrorBody),
    )
)]
async fn get_recording(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<RecordingDto>> {
    live_viewer(&state, &user).await?;
    let r = dvr::get(&state.db, id)
        .await?
        .filter(|r| visible(&user, r))
        .ok_or_else(not_found)?;
    Ok(Json(recording_dto(r)))
}

async fn rule_owner(state: &AppState, r: &Recording) -> ApiResult<Option<i64>> {
    Ok(match r.rule_id {
        Some(rule) => dvr::get_rule(&state.db, rule)
            .await?
            .and_then(|r| r.user_id),
        None => None,
    })
}

/// Cancels a scheduled recording (one of a series is skipped) or deletes a finished
/// one with its files. Running recordings are stopped first (`stopRecording`).
#[utoipa::path(
    delete,
    path = "/livetv/recordings/{id}",
    operation_id = "deleteRecording",
    tag = "livetv",
    params(("id" = i64, Path, description = "Recording id")),
    responses(
        (status = NO_CONTENT, description = "Cancelled or deleted"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not yours, or Live TV is barred", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown recording", body = ErrorBody),
        (status = CONFLICT, description = "The recording is running", body = ErrorBody),
    )
)]
async fn delete_recording(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    live_viewer(&state, &user).await?;
    let r = dvr::get(&state.db, id)
        .await?
        .filter(|r| visible(&user, r))
        .ok_or_else(not_found)?;
    may_edit(&user, rule_owner(&state, &r).await?)?;
    match r.status.as_str() {
        "scheduled" => {
            if r.rule_kind.as_deref() == Some("program")
                && let Some(rule) = r.rule_id
            {
                dvr::delete_rule(&state.db, rule).await?;
            } else {
                dvr::finish(&state.db, id, "cancelled", None).await?;
            }
            plan::update_conflicts(&state.db).await?;
        }
        "recording" => {
            return Err(ApiError::conflict(
                "recording_running",
                "Die Aufnahme läuft noch; erst stoppen.",
            ));
        }
        _ => {
            flixnet_livetv::dvr::delete_files(&r).await;
            dvr::delete(&state.db, id).await?;
            if let Some(rule) = r.rule_id
                && r.rule_kind.as_deref() == Some("program")
            {
                dvr::delete_rule(&state.db, rule).await?;
            }
            state.dvr.notify_changed().await;
        }
    }
    state.dvr.replan();
    Ok(StatusCode::NO_CONTENT)
}

/// Stops a running recording; what was recorded is kept and added to the library.
#[utoipa::path(
    post,
    path = "/livetv/recordings/{id}/stop",
    operation_id = "stopRecording",
    tag = "livetv",
    params(("id" = i64, Path, description = "Recording id")),
    responses(
        (status = OK, description = "The stopped recording", body = RecordingDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not yours, or Live TV is barred", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown recording", body = ErrorBody),
        (status = CONFLICT, description = "The recording is not running", body = ErrorBody),
    )
)]
async fn stop_recording(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<RecordingDto>> {
    live_viewer(&state, &user).await?;
    let r = dvr::get(&state.db, id)
        .await?
        .filter(|r| visible(&user, r))
        .ok_or_else(not_found)?;
    may_edit(&user, rule_owner(&state, &r).await?)?;
    if r.status != "recording" {
        return Err(ApiError::conflict(
            "recording_not_running",
            "Die Aufnahme läuft nicht.",
        ));
    }
    if !state.dvr.stop_recording(id).await {
        // Not running in this server (e.g. interrupted): close it as it is.
        let status = if r.bytes > 0 { "completed" } else { "failed" };
        dvr::finish(&state.db, id, status, None).await?;
    }
    let r = dvr::get(&state.db, id).await?.ok_or_else(not_found)?;
    Ok(Json(recording_dto(r)))
}

// ---- Rules -------------------------------------------------------------------------

/// Recording rules (one-off programs and series).
#[utoipa::path(
    get,
    path = "/livetv/recording-rules",
    operation_id = "listRecordingRules",
    tag = "livetv",
    responses(
        (status = OK, description = "Rules", body = [RecordingRuleDto]),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Live TV is barred for this profile", body = ErrorBody),
    )
)]
async fn list_rules(
    user: AuthUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<RecordingRuleDto>>> {
    live_viewer(&state, &user).await?;
    let scheduled = dvr::list(&state.db, &["scheduled"]).await?;
    let mut out = Vec::new();
    for rule in dvr::list_rules(&state.db).await? {
        out.push(rule_dto(&state, &user, rule, &scheduled).await?);
    }
    Ok(Json(out))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateRuleRequest {
    /// Record the series of this program (title and guide series id come from it).
    pub program_id: Option<i64>,
    /// Or: record everything with this title.
    pub title: Option<String>,
    /// Only on this channel (`null` = any channel).
    pub channel_id: Option<i64>,
    /// "Nur neue Folgen": skip episodes recorded before and repeats.
    #[serde(default)]
    pub new_only: bool,
    /// Only broadcasts starting within this window (minutes after local midnight).
    pub window_start_min: Option<i64>,
    pub window_end_min: Option<i64>,
    /// The client's offset from UTC in minutes (the window's local time).
    pub utc_offset_min: Option<i64>,
    pub padding_before_s: Option<i64>,
    pub padding_after_s: Option<i64>,
    pub priority: Option<i64>,
    /// Keep only the newest N recordings.
    pub keep_last: Option<i64>,
}

/// Records a series ("Serie aufnehmen") and schedules its upcoming broadcasts.
#[utoipa::path(
    post,
    path = "/livetv/recording-rules",
    operation_id = "createRecordingRule",
    tag = "livetv",
    request_body = CreateRuleRequest,
    responses(
        (status = CREATED, description = "The rule, with what it scheduled", body = RecordingRuleDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Live TV is barred for this profile", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown program or channel", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid input", body = ErrorBody),
    )
)]
async fn create_rule(
    user: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<CreateRuleRequest>,
) -> ApiResult<(StatusCode, Json<RecordingRuleDto>)> {
    live_viewer(&state, &user).await?;
    validate_padding(body.padding_before_s)?;
    validate_padding(body.padding_after_s)?;
    validate_priority(body.priority)?;
    validate_keep(body.keep_last)?;
    let window = validate_window(body.window_start_min, body.window_end_min)?;
    let offset = validate_offset(body.utc_offset_min)?;
    let (title, series_id) = match (body.program_id, body.title.as_deref()) {
        (Some(id), _) => {
            let program = livetv::get_program(&state.db, id)
                .await?
                .filter(|p| {
                    program_allowed(&user.restriction, p.rating.as_deref(), &p.category_list())
                })
                .ok_or_else(|| ApiError::not_found("Sendung nicht gefunden."))?;
            (program.title, program.series_id)
        }
        (None, Some(title)) => (validate::name(title)?, None),
        (None, None) => {
            return Err(ApiError::validation(
                "Bitte eine Sendung oder einen Titel angeben.",
            ));
        }
    };
    if let Some(channel) = body.channel_id {
        livetv::get_channel(&state.db, channel)
            .await?
            .ok_or_else(|| ApiError::not_found("Sender nicht gefunden."))?;
    }
    let settings = config::load(&state.db).await?;
    let id = dvr::create_rule(
        &state.db,
        &NewRule {
            kind: "series",
            user_id: Some(user.user_id),
            title: &title,
            series_id: series_id.as_deref(),
            channel_id: body.channel_id,
            program_start_ms: None,
            new_only: body.new_only,
            window,
            utc_offset_min: offset,
            padding_before_s: body
                .padding_before_s
                .unwrap_or(settings.padding_before_minutes * 60),
            padding_after_s: body
                .padding_after_s
                .unwrap_or(settings.padding_after_minutes * 60),
            priority: body.priority.unwrap_or(0),
            keep_last: body.keep_last,
        },
    )
    .await?;
    plan::plan(&state.db).await?;
    state.dvr.replan();
    let rule = dvr::get_rule(&state.db, id)
        .await?
        .ok_or_else(rule_not_found)?;
    let scheduled = dvr::list(&state.db, &["scheduled"]).await?;
    Ok((
        StatusCode::CREATED,
        Json(rule_dto(&state, &user, rule, &scheduled).await?),
    ))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct UpdateRuleRequest {
    /// `null` = any channel.
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<i64>, nullable)]
    pub channel_id: Option<Option<i64>>,
    pub new_only: Option<bool>,
    /// Set both, or `null` both to remove the window.
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<i64>, nullable)]
    pub window_start_min: Option<Option<i64>>,
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<i64>, nullable)]
    pub window_end_min: Option<Option<i64>>,
    pub utc_offset_min: Option<i64>,
    pub padding_before_s: Option<i64>,
    pub padding_after_s: Option<i64>,
    pub priority: Option<i64>,
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<i64>, nullable)]
    pub keep_last: Option<Option<i64>>,
    pub enabled: Option<bool>,
}

/// Changes a rule (priority, filters, padding, retention); its schedule follows.
#[utoipa::path(
    patch,
    path = "/livetv/recording-rules/{id}",
    operation_id = "updateRecordingRule",
    tag = "livetv",
    params(("id" = i64, Path, description = "Rule id")),
    request_body = UpdateRuleRequest,
    responses(
        (status = OK, description = "The updated rule", body = RecordingRuleDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not yours, or Live TV is barred", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown rule", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid input", body = ErrorBody),
    )
)]
async fn update_rule(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<UpdateRuleRequest>,
) -> ApiResult<Json<RecordingRuleDto>> {
    live_viewer(&state, &user).await?;
    let rule = dvr::get_rule(&state.db, id)
        .await?
        .ok_or_else(rule_not_found)?;
    may_edit(&user, rule.user_id)?;
    validate_padding(body.padding_before_s)?;
    validate_padding(body.padding_after_s)?;
    validate_priority(body.priority)?;
    validate_keep(body.keep_last.flatten())?;
    if body.utc_offset_min.is_some() {
        validate_offset(body.utc_offset_min)?;
    }
    let window = match (body.window_start_min, body.window_end_min) {
        (None, None) => None,
        (start, end) => Some(validate_window(start.flatten(), end.flatten())?),
    };
    if let Some(Some(channel)) = body.channel_id {
        livetv::get_channel(&state.db, channel)
            .await?
            .ok_or_else(|| ApiError::not_found("Sender nicht gefunden."))?;
    }
    let program_rule = rule.kind == "program";
    dvr::update_rule(
        &state.db,
        id,
        &RulePatch {
            // A program rule keeps its channel and filters.
            channel_id: if program_rule { None } else { body.channel_id },
            new_only: if program_rule { None } else { body.new_only },
            window: if program_rule { None } else { window },
            utc_offset_min: body.utc_offset_min,
            padding_before_s: body.padding_before_s,
            padding_after_s: body.padding_after_s,
            priority: body.priority,
            keep_last: body.keep_last,
            enabled: body.enabled,
        },
    )
    .await?;
    plan::plan(&state.db).await?;
    state.dvr.replan();
    let rule = dvr::get_rule(&state.db, id)
        .await?
        .ok_or_else(rule_not_found)?;
    let scheduled = dvr::list(&state.db, &["scheduled"]).await?;
    Ok(Json(rule_dto(&state, &user, rule, &scheduled).await?))
}

/// Deletes a rule with its scheduled recordings (finished ones stay).
#[utoipa::path(
    delete,
    path = "/livetv/recording-rules/{id}",
    operation_id = "deleteRecordingRule",
    tag = "livetv",
    params(("id" = i64, Path, description = "Rule id")),
    responses(
        (status = NO_CONTENT, description = "Deleted"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not yours, or Live TV is barred", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown rule", body = ErrorBody),
    )
)]
async fn delete_rule(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    live_viewer(&state, &user).await?;
    let rule = dvr::get_rule(&state.db, id)
        .await?
        .ok_or_else(rule_not_found)?;
    may_edit(&user, rule.user_id)?;
    dvr::delete_rule(&state.db, id).await?;
    plan::update_conflicts(&state.db).await?;
    state.dvr.replan();
    Ok(StatusCode::NO_CONTENT)
}

// ---- Live TV access per profile ----------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct LiveTvProfileDto {
    pub user_id: i64,
    pub name: String,
    pub is_admin: bool,
    /// The profile may watch Live TV and use the DVR.
    pub live_tv_allowed: bool,
}

/// Profiles and whether they may use Live TV (administrators).
#[utoipa::path(
    get,
    path = "/livetv/profiles",
    operation_id = "listLiveTvProfiles",
    tag = "livetv",
    responses(
        (status = OK, description = "Profiles", body = [LiveTvProfileDto]),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn list_profiles(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<LiveTvProfileDto>>> {
    let mut out = Vec::new();
    for user in flixnet_db::users::list(&state.db).await? {
        out.push(LiveTvProfileDto {
            live_tv_allowed: dvr::live_tv_allowed(&state.db, user.id).await?,
            user_id: user.id,
            name: user.name,
            is_admin: user.is_admin,
        });
    }
    Ok(Json(out))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetLiveTvAccessRequest {
    pub live_tv_allowed: bool,
}

/// Allows or bars Live TV for a profile ("Live TV erlaubt"; administrators).
#[utoipa::path(
    put,
    path = "/livetv/profiles/{id}",
    operation_id = "setLiveTvAccess",
    tag = "livetv",
    params(("id" = i64, Path, description = "User id")),
    request_body = SetLiveTvAccessRequest,
    responses(
        (status = NO_CONTENT, description = "Changed"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown user", body = ErrorBody),
    )
)]
async fn set_profile_access(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<SetLiveTvAccessRequest>,
) -> ApiResult<StatusCode> {
    if dvr::set_live_tv_allowed(&state.db, id, body.live_tv_allowed).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("Profil nicht gefunden."))
    }
}

// ---- Library ----------------------------------------------------------------------

/// A library of `kind` covering `folder`, created ("Aufnahmen – …") if none does.
async fn ensure_library(
    state: &AppState,
    folder: &std::path::Path,
    kind: &str,
    name: &str,
) -> Result<i64, flixnet_db::DbError> {
    let path = folder.to_string_lossy().into_owned();
    for library in flixnet_db::libraries::list(&state.db).await? {
        if library.kind == kind
            && flixnet_db::libraries::paths(&state.db, library.id)
                .await?
                .iter()
                .any(|p| std::path::Path::new(p) == folder)
        {
            return Ok(library.id);
        }
    }
    let library = flixnet_db::libraries::create(&state.db, name, kind, &[path]).await?;
    tracing::info!(library = library.id, folder = %folder.display(), "recordings library created");
    if let Some(watcher) = &state.watcher
        && let Err(err) = watcher.sync(&state.db).await
    {
        tracing::warn!(%err, "file watcher not updated");
    }
    Ok(library.id)
}

/// Finished recordings become library items: the recordings folders get libraries
/// (created on first use) and are scanned right away.
pub(crate) fn library_hook(state: AppState) -> FinishedHook {
    std::sync::Arc::new(move |_recording| {
        let state = state.clone();
        Box::pin(async move {
            let root = state.dvr.recordings_root().await;
            for (dir, kind, name) in [
                (naming::SERIES_DIR, "shows", "Aufnahmen – Serien"),
                (naming::MOVIES_DIR, "movies", "Aufnahmen – Filme"),
            ] {
                let folder = root.join(dir);
                if let Err(err) = tokio::fs::create_dir_all(&folder).await {
                    tracing::warn!(folder = %folder.display(), %err, "recordings folder unavailable");
                    continue;
                }
                match ensure_library(&state, &folder, kind, name).await {
                    Ok(library) => {
                        if let Err(err) = flixnet_library::jobs::queue_scan(
                            state.jobs.queue(),
                            library,
                            priority::INTERACTIVE,
                        )
                        .await
                        {
                            tracing::warn!(%err, "could not queue the recordings scan");
                        }
                    }
                    Err(err) => tracing::warn!(%err, "recordings library unavailable"),
                }
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_follow_the_restriction() {
        let open = Restriction::default();
        assert!(program_allowed(&open, Some("FSK 18"), &[]));
        let kid = Restriction {
            max_age: Some(12),
            allow_unrated: false,
            blocked_genres: vec!["Horror".into()],
        };
        assert!(program_allowed(&kid, Some("FSK 12"), &[]));
        assert!(!program_allowed(&kid, Some("FSK 16"), &[]));
        assert!(
            !program_allowed(&kid, None, &[]),
            "unrated hidden by default"
        );
        assert!(!program_allowed(&kid, Some("FSK 6"), &["horror".into()]));
        let lenient = Restriction {
            allow_unrated: true,
            ..kid
        };
        assert!(program_allowed(&lenient, None, &["Krimi".into()]));
    }
}
