//! Playback reporting and what is built from it.
//!
//! * Players report start, progress (every ~10 s and on pause/seek) and stop. The
//!   server keeps the resume point, marks items watched near the end and appends to
//!   the watch history.
//! * "Weiterschauen" (`/items/resume`), "Nächste Folge" (`/items/next-up`), the
//!   episode to start a series with and the episode after an episode (autoplay).
//! * Administrators see the active sessions (`/sessions/active`).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use flixnet_db::playstate::{self, NewWatchEvent, Progress, Visibility};
use flixnet_db::{browse, items, sessions, userdata};
use flixnet_media::decision::PlayMethod;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::active_sessions::ActiveSession;
use crate::auth::{AdminUser, AuthUser};
use crate::browse::{ItemCardDto, card, ensure_visible, visible_libraries};
use crate::error::{ApiError, ApiResult, ErrorBody};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(start_playback))
        .routes(routes!(report_playback_progress))
        .routes(routes!(stop_playback))
        .routes(routes!(list_active_sessions))
        .routes(routes!(list_resume_items))
        .routes(routes!(list_next_up))
        .routes(routes!(get_series_next_up))
        .routes(routes!(get_next_episode))
}

/// What a player reports (the same shape for start, progress and stop).
#[derive(Debug, Deserialize, ToSchema)]
pub struct PlaybackReport {
    /// The movie or episode being played.
    pub item_id: i64,
    /// Current position in milliseconds (multi-part movies: across all parts).
    #[serde(default)]
    pub position_ms: i64,
    /// Total duration as the player sees it; the file's duration when omitted.
    pub duration_ms: Option<i64>,
    pub play_method: PlayMethod,
    /// Transcode session from playback info, for Remux/Transcode.
    pub transcode_session_id: Option<String>,
    #[serde(default)]
    pub is_paused: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PlaybackSessionDto {
    /// Id for the following progress and stop reports.
    pub id: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PlaybackStopDto {
    /// The item now counts as watched.
    pub played: bool,
    /// Where the item resumes from now (0 = from the start).
    pub resume_position_ms: i64,
}

fn validate(report: &PlaybackReport) -> ApiResult<()> {
    if report.position_ms < 0 || report.duration_ms.is_some_and(|d| d < 0) {
        return Err(ApiError::validation(
            "Position und Dauer dürfen nicht negativ sein.",
        ));
    }
    if report
        .transcode_session_id
        .as_deref()
        .is_some_and(|id| !flixnet_media::transcode::is_session_id(id))
    {
        return Err(ApiError::validation("Ungültige Transcode-Sitzung."));
    }
    Ok(())
}

fn is_playback_session_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Only movies and episodes are played (and visible to this user).
async fn ensure_playable(state: &AppState, user: &AuthUser, item_id: i64) -> ApiResult<()> {
    ensure_visible(state, user, item_id).await?;
    let item = items::get(&state.db, item_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
    if item.kind != "movie" && item.kind != "episode" {
        return Err(ApiError::validation(
            "Nur Filme und Folgen lassen sich abspielen.",
        ));
    }
    Ok(())
}

fn method_name(method: PlayMethod) -> &'static str {
    match method {
        PlayMethod::DirectPlay => "direct_play",
        PlayMethod::Remux => "remux",
        PlayMethod::Transcode => "transcode",
    }
}

async fn duration(state: &AppState, report: &PlaybackReport) -> ApiResult<Option<i64>> {
    match report.duration_ms.filter(|d| *d > 0) {
        Some(d) => Ok(Some(d)),
        None => Ok(playstate::item_duration(&state.db, report.item_id).await?),
    }
}

async fn event(state: &AppState, session: &ActiveSession, kind: &str, completed: bool) {
    let result = playstate::record_event(
        &state.db,
        &NewWatchEvent {
            user_id: session.user_id,
            item_id: session.item_id,
            event: kind,
            position_ms: session.position_ms,
            duration_ms: session.duration_ms,
            play_method: Some(method_name(session.play_method)),
            completed,
            session_id: Some(&session.id),
            device_id: Some(session.device_id),
            client: session.client.as_deref(),
            device_name: session.device_name.as_deref(),
        },
    )
    .await;
    if let Err(err) = result {
        tracing::warn!(session = %session.id, %err, "recording a watch event failed");
    }
}

/// Registers a session for `report` (new, or re-created after a restart or expiry).
async fn open_session(
    state: &AppState,
    user: &AuthUser,
    id: String,
    report: &PlaybackReport,
) -> ApiResult<ActiveSession> {
    let device = sessions::list_devices(&state.db, user.user_id)
        .await?
        .into_iter()
        .find(|d| d.id == user.device_id);
    let duration_ms = duration(state, report).await?;
    let now = flixnet_db::now_ms();
    let session = ActiveSession {
        id,
        user_id: user.user_id,
        user_name: user.name.clone(),
        device_id: user.device_id,
        device_name: device.as_ref().map(|d| d.name.clone()),
        client: device.map(|d| d.client),
        item_id: report.item_id,
        position_ms: report.position_ms,
        duration_ms,
        play_method: report.play_method,
        transcode_session_id: report.transcode_session_id.clone(),
        is_paused: report.is_paused,
        started_at: now,
        last_report_at: now,
        milestone: playstate::quartile(report.position_ms, duration_ms),
    };
    state.playback.insert(session.clone());
    event(state, &session, "start", false).await;
    Ok(session)
}

/// Ends a session: resume point or watched, plus the history entry.
pub(crate) async fn finish(
    db: &flixnet_db::Db,
    session: &ActiveSession,
) -> flixnet_db::Result<Progress> {
    let progress = playstate::save_stop(
        db,
        session.user_id,
        session.item_id,
        session.position_ms,
        session.duration_ms,
    )
    .await?;
    playstate::record_event(
        db,
        &NewWatchEvent {
            user_id: session.user_id,
            item_id: session.item_id,
            event: "stop",
            position_ms: session.position_ms,
            duration_ms: session.duration_ms,
            play_method: Some(method_name(session.play_method)),
            completed: progress == Progress::Played,
            session_id: Some(&session.id),
            device_id: Some(session.device_id),
            client: session.client.as_deref(),
            device_name: session.device_name.as_deref(),
        },
    )
    .await?;
    Ok(progress)
}

/// Playback started.
#[utoipa::path(
    post,
    path = "/playback/sessions",
    operation_id = "startPlayback",
    tag = "playback",
    request_body = PlaybackReport,
    responses(
        (status = OK, description = "Session registered", body = PlaybackSessionDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) item", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid report", body = ErrorBody),
    )
)]
async fn start_playback(
    user: AuthUser,
    State(state): State<AppState>,
    Json(report): Json<PlaybackReport>,
) -> ApiResult<Json<PlaybackSessionDto>> {
    let session = start_session(&state, &user, None, &report).await?;
    Ok(Json(PlaybackSessionDto { id: session.id }))
}

/// Registers a playback start. `id` is a caller-chosen session id (32 lowercase hex
/// characters, e.g. derived from a Jellyfin play session); a random one otherwise.
pub async fn start_session(
    state: &AppState,
    user: &AuthUser,
    id: Option<String>,
    report: &PlaybackReport,
) -> ApiResult<ActiveSession> {
    validate(report)?;
    if id.as_deref().is_some_and(|id| !is_playback_session_id(id)) {
        return Err(ApiError::validation("Ungültige Wiedergabe-Sitzung."));
    }
    ensure_playable(state, user, report.item_id).await?;
    let id = id.unwrap_or_else(|| flixnet_core::auth::new_token().token[..32].to_owned());
    open_session(state, user, id, report).await
}

/// Looks a session up for its owner; an unknown (expired, or lost in a restart) id is
/// opened again from the report.
async fn owned_session(
    state: &AppState,
    user: &AuthUser,
    id: &str,
    report: &PlaybackReport,
) -> ApiResult<Option<ActiveSession>> {
    if !is_playback_session_id(id) {
        return Err(ApiError::not_found("Wiedergabe nicht gefunden."));
    }
    match state.playback.get(id) {
        Some(s) if s.user_id != user.user_id => {
            Err(ApiError::not_found("Wiedergabe nicht gefunden."))
        }
        Some(s) if s.item_id != report.item_id => Err(ApiError::validation(
            "Die Wiedergabe gehört zu einem anderen Eintrag.",
        )),
        Some(s) => Ok(Some(s)),
        None => {
            ensure_playable(state, user, report.item_id).await?;
            Ok(None)
        }
    }
}

/// Progress while playing: every ~10 s and on pause, resume and seek.
#[utoipa::path(
    post,
    path = "/playback/sessions/{id}/progress",
    operation_id = "reportPlaybackProgress",
    tag = "playback",
    params(("id" = String, Path, description = "Session id from startPlayback")),
    request_body = PlaybackReport,
    responses(
        (status = NO_CONTENT, description = "Recorded"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "Someone else's session, or no such item", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid report", body = ErrorBody),
    )
)]
async fn report_playback_progress(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(report): Json<PlaybackReport>,
) -> ApiResult<StatusCode> {
    progress_session(&state, &user, &id, &report).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Records progress of session `id` (re-opened from the report if it is unknown).
pub async fn progress_session(
    state: &AppState,
    user: &AuthUser,
    id: &str,
    report: &PlaybackReport,
) -> ApiResult<()> {
    validate(report)?;
    let duration_ms = duration(state, report).await?;
    let session = match owned_session(state, user, id, report).await? {
        Some(_) => state.playback.update(id, |s| {
            s.position_ms = report.position_ms;
            s.duration_ms = duration_ms;
            s.play_method = report.play_method;
            s.transcode_session_id = report.transcode_session_id.clone();
            s.is_paused = report.is_paused;
            s.last_report_at = flixnet_db::now_ms();
        }),
        None => Some(open_session(state, user, id.to_owned(), report).await?),
    };
    let Some(session) = session else {
        // Stopped concurrently; nothing left to update.
        return Ok(());
    };
    playstate::save_progress(
        &state.db,
        user.user_id,
        report.item_id,
        report.position_ms,
        duration_ms,
    )
    .await?;
    let quartile = playstate::quartile(report.position_ms, duration_ms);
    if quartile > session.milestone {
        state.playback.update(id, |s| s.milestone = quartile);
        event(state, &session, "progress", false).await;
    }
    Ok(())
}

/// Playback ended (player closed, item finished or switched).
#[utoipa::path(
    post,
    path = "/playback/sessions/{id}/stop",
    operation_id = "stopPlayback",
    tag = "playback",
    params(("id" = String, Path, description = "Session id from startPlayback")),
    request_body = PlaybackReport,
    responses(
        (status = OK, description = "The item's state afterwards", body = PlaybackStopDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "Someone else's session, or no such item", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid report", body = ErrorBody),
    )
)]
async fn stop_playback(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(report): Json<PlaybackReport>,
) -> ApiResult<Json<PlaybackStopDto>> {
    stop_session(&state, &user, &id, &report).await.map(Json)
}

/// Ends session `id`: resume point or watched, plus the history entry. An unknown
/// session is finished from the report alone.
pub async fn stop_session(
    state: &AppState,
    user: &AuthUser,
    id: &str,
    report: &PlaybackReport,
) -> ApiResult<PlaybackStopDto> {
    validate(report)?;
    let duration_ms = duration(state, report).await?;
    let known = owned_session(state, user, id, report).await?;
    let now = flixnet_db::now_ms();
    let mut session = match known.and_then(|_| state.playback.remove(id)) {
        Some(session) => session,
        // Unknown (or just expired): finish it from the report alone.
        None => ActiveSession {
            id: id.to_owned(),
            user_id: user.user_id,
            user_name: user.name.clone(),
            device_id: user.device_id,
            device_name: None,
            client: None,
            item_id: report.item_id,
            position_ms: 0,
            duration_ms: None,
            play_method: report.play_method,
            transcode_session_id: None,
            is_paused: false,
            started_at: now,
            last_report_at: now,
            milestone: 0,
        },
    };
    session.position_ms = report.position_ms;
    session.duration_ms = duration_ms;
    session.play_method = report.play_method;
    let progress = finish(&state.db, &session).await?;
    let resume_position_ms = userdata::get(&state.db, user.user_id, report.item_id)
        .await?
        .position_ms;
    Ok(PlaybackStopDto {
        played: progress == Progress::Played,
        resume_position_ms,
    })
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ActiveSessionDto {
    pub id: String,
    pub user_id: i64,
    /// Profile name.
    pub user_name: String,
    pub device_name: Option<String>,
    pub client: Option<String>,
    /// What is playing (`None` if it vanished from the library meanwhile).
    pub item: Option<ItemCardDto>,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub play_method: PlayMethod,
    /// The video is re-encoded (`play_method` = transcode).
    pub transcoding: bool,
    /// An ffmpeg process currently works for this session.
    pub ffmpeg_running: bool,
    pub transcode_session_id: Option<String>,
    pub is_paused: bool,
    pub started_at: i64,
    pub last_report_at: i64,
}

/// Playback sessions that reported recently (administrators).
#[utoipa::path(
    get,
    path = "/sessions/active",
    operation_id = "listActiveSessions",
    tag = "playback",
    responses(
        (status = OK, description = "Active sessions, oldest first", body = [ActiveSessionDto]),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn list_active_sessions(
    AdminUser(admin): AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<ActiveSessionDto>>> {
    let active = state.playback.list(flixnet_db::now_ms());
    let transcodes = state.transcoder.sessions().await;
    let ids: Vec<i64> = active.iter().map(|s| s.item_id).collect();
    let mut cards: std::collections::HashMap<i64, ItemCardDto> =
        browse::summaries(&state.db, &ids, Some(admin.user_id))
            .await?
            .into_iter()
            .map(|s| (s.id, card(&state, s)))
            .collect();
    Ok(Json(
        active
            .into_iter()
            .map(|s| ActiveSessionDto {
                item: cards.remove(&s.item_id),
                ffmpeg_running: s.transcode_session_id.as_deref().is_some_and(|t| {
                    transcodes
                        .iter()
                        .any(|info| info.id == t && info.transcoding)
                }),
                transcoding: s.play_method == PlayMethod::Transcode,
                id: s.id,
                user_id: s.user_id,
                user_name: s.user_name,
                device_name: s.device_name,
                client: s.client,
                position_ms: s.position_ms,
                duration_ms: s.duration_ms,
                play_method: s.play_method,
                transcode_session_id: s.transcode_session_id,
                is_paused: s.is_paused,
                started_at: s.started_at,
                last_report_at: s.last_report_at,
            })
            .collect(),
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HomeRowQuery {
    /// 1–100, default 20.
    pub limit: Option<i64>,
}

async fn visibility(state: &AppState, user: &AuthUser) -> ApiResult<Visibility> {
    Ok(Visibility {
        library_ids: visible_libraries(state, user, None).await?,
        restriction: user.restriction.clone(),
    })
}

/// Cards for ids, in the given order.
async fn cards(state: &AppState, user: &AuthUser, ids: &[i64]) -> ApiResult<Vec<ItemCardDto>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut rows = browse::summaries(&state.db, ids, Some(user.user_id)).await?;
    rows.sort_by_key(|r| ids.iter().position(|id| *id == r.id));
    Ok(rows.into_iter().map(|r| card(state, r)).collect())
}

/// "Weiterschauen": movies and episodes with a resume point, most recent first.
#[utoipa::path(
    get,
    path = "/items/resume",
    operation_id = "listResumeItems",
    tag = "items",
    params(HomeRowQuery),
    responses(
        (status = OK, description = "Items to continue", body = [ItemCardDto]),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn list_resume_items(
    user: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<HomeRowQuery>,
) -> ApiResult<Json<Vec<ItemCardDto>>> {
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let ids = playstate::resume_items(
        &state.db,
        user.user_id,
        &visibility(&state, &user).await?,
        limit,
    )
    .await?;
    Ok(Json(cards(&state, &user, &ids).await?))
}

/// "Nächste Folge": the next unwatched episode of series watched recently.
#[utoipa::path(
    get,
    path = "/items/next-up",
    operation_id = "listNextUp",
    tag = "items",
    params(HomeRowQuery),
    responses(
        (status = OK, description = "Next episodes, most recently watched series first", body = [ItemCardDto]),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn list_next_up(
    user: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<HomeRowQuery>,
) -> ApiResult<Json<Vec<ItemCardDto>>> {
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let since = flixnet_db::now_ms() - playstate::NEXT_UP_MAX_AGE_MS;
    let ids = playstate::next_up(
        &state.db,
        user.user_id,
        &visibility(&state, &user).await?,
        since,
        limit,
    )
    .await?;
    Ok(Json(cards(&state, &user, &ids).await?))
}

async fn single_card(state: &AppState, user: &AuthUser, id: Option<i64>) -> ApiResult<Response> {
    let Some(id) = id else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let library_ids = visible_libraries(state, user, None).await?;
    if !browse::is_visible(&state.db, id, &library_ids, &user.restriction).await? {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    Ok(match cards(state, user, &[id]).await?.pop() {
        Some(card) => Json(card).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    })
}

/// The episode a series' play button starts: the next one after the furthest watched
/// episode (with its resume point), else the first; when everything is watched, the
/// first episode again.
#[utoipa::path(
    get,
    path = "/items/{id}/next-up",
    operation_id = "getSeriesNextUp",
    tag = "items",
    params(("id" = i64, Path, description = "Series id")),
    responses(
        (status = OK, description = "The episode to play", body = ItemCardDto),
        (status = NO_CONTENT, description = "The series has no episodes"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) series", body = ErrorBody),
    )
)]
async fn get_series_next_up(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Response> {
    ensure_visible(&state, &user, id).await?;
    let series = items::get(&state.db, id)
        .await?
        .filter(|i| i.kind == "series")
        .ok_or_else(|| ApiError::not_found("Serie nicht gefunden."))?;
    let next = match playstate::series_next(&state.db, user.user_id, series.id).await? {
        Some(next) => Some(next.episode_id),
        None => playstate::first_episode(&state.db, series.id).await?,
    };
    single_card(&state, &user, next).await
}

/// The episode after an episode in broadcast order, watched or not (autoplay).
#[utoipa::path(
    get,
    path = "/items/{id}/next-episode",
    operation_id = "getNextEpisode",
    tag = "items",
    params(("id" = i64, Path, description = "Episode id")),
    responses(
        (status = OK, description = "The following episode", body = ItemCardDto),
        (status = NO_CONTENT, description = "This is the last episode"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) item", body = ErrorBody),
    )
)]
async fn get_next_episode(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Response> {
    ensure_visible(&state, &user, id).await?;
    let next = playstate::next_episode(&state.db, id).await?;
    single_card(&state, &user, next).await
}
