//! HLS for transcode sessions created by playback info.
//!
//! The 128-bit session id in the URL is the capability (players such as Safari's
//! native HLS cannot add headers); sessions belong to one user, end when the player
//! closes them and are reaped after a minute without requests.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use flixnet_media::transcode::{TranscodeError, is_session_id};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::{AdminUser, AuthUser};
use crate::error::{ApiError, ApiResult, ErrorBody};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_sessions))
        .routes(routes!(stop_session))
        .routes(routes!(get_playlist))
        .routes(routes!(get_init))
        .routes(routes!(get_segment))
}

fn session_id(id: &str) -> ApiResult<&str> {
    if is_session_id(id) {
        Ok(id)
    } else {
        Err(ApiError::not_found("Wiedergabe-Sitzung nicht gefunden."))
    }
}

fn map_error(err: TranscodeError) -> ApiError {
    match err {
        TranscodeError::UnknownSession => ApiError::not_found("Wiedergabe-Sitzung nicht gefunden."),
        TranscodeError::OutOfRange(_) => ApiError::not_found("Segment nicht vorhanden."),
        TranscodeError::Timeout(_) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "transcode_timeout",
            "Die Umwandlung ist zu langsam.",
        ),
        other => ApiError::internal(other),
    }
}

async fn file_response(
    path: std::path::PathBuf,
    content_type: &'static str,
) -> ApiResult<Response> {
    let bytes = tokio::fs::read(&path).await.map_err(ApiError::internal)?;
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=3600"),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// The session's VOD playlist (all segments, so players can seek anywhere).
#[utoipa::path(
    get,
    path = "/transcode/{session}/main.m3u8",
    operation_id = "getTranscodePlaylist",
    tag = "playback",
    security(()),
    params(("session" = String, Path, description = "Session id from playback info")),
    responses(
        (status = OK, description = "HLS playlist", content_type = "application/vnd.apple.mpegurl", body = String),
        (status = NOT_FOUND, description = "Unknown or expired session", body = ErrorBody),
    )
)]
async fn get_playlist(
    State(state): State<AppState>,
    Path(session): Path<String>,
) -> ApiResult<Response> {
    let playlist = state
        .transcoder
        .playlist(session_id(&session)?)
        .map_err(map_error)?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/vnd.apple.mpegurl"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        playlist,
    )
        .into_response())
}

/// The fMP4 init segment.
#[utoipa::path(
    get,
    path = "/transcode/{session}/init.mp4",
    operation_id = "getTranscodeInit",
    tag = "playback",
    security(()),
    params(("session" = String, Path, description = "Session id from playback info")),
    responses(
        (status = OK, description = "Init segment", content_type = "video/mp4"),
        (status = NOT_FOUND, description = "Unknown or expired session", body = ErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "Transcoding too slow", body = ErrorBody),
    )
)]
async fn get_init(
    State(state): State<AppState>,
    Path(session): Path<String>,
) -> ApiResult<Response> {
    let path = state
        .transcoder
        .init(session_id(&session)?)
        .await
        .map_err(map_error)?;
    file_response(path, "video/mp4").await
}

/// One media segment (`{n}.m4s`); seeking far ahead restarts the transcode there.
#[utoipa::path(
    get,
    path = "/transcode/{session}/{segment}",
    operation_id = "getTranscodeSegment",
    tag = "playback",
    security(()),
    params(
        ("session" = String, Path, description = "Session id from playback info"),
        ("segment" = String, Path, description = "Segment file name, e.g. `12.m4s`"),
    ),
    responses(
        (status = OK, description = "Media segment", content_type = "video/iso.segment"),
        (status = NOT_FOUND, description = "Unknown session or segment", body = ErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "Transcoding too slow", body = ErrorBody),
    )
)]
async fn get_segment(
    State(state): State<AppState>,
    Path((session, segment)): Path<(String, String)>,
) -> ApiResult<Response> {
    let index: u32 = segment
        .strip_suffix(".m4s")
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| ApiError::not_found("Segment nicht vorhanden."))?;
    let path = state
        .transcoder
        .segment(session_id(&session)?, index)
        .await
        .map_err(map_error)?;
    file_response(path, "video/iso.segment").await
}

/// Ends a session (the player closed); stops its ffmpeg and removes its files.
#[utoipa::path(
    delete,
    path = "/transcode/{session}",
    operation_id = "stopTranscodeSession",
    tag = "playback",
    params(("session" = String, Path, description = "Session id from playback info")),
    responses(
        (status = NO_CONTENT, description = "Stopped (or already gone)"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "Someone else's session", body = ErrorBody),
    )
)]
async fn stop_session(
    user: AuthUser,
    State(state): State<AppState>,
    Path(session): Path<String>,
) -> ApiResult<StatusCode> {
    let id = session_id(&session)?;
    match state.transcoder.owner(id) {
        None => {}
        Some(owner) if owner == user.user_id || user.is_admin => state.transcoder.stop(id).await,
        Some(_) => return Err(ApiError::not_found("Wiedergabe-Sitzung nicht gefunden.")),
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TranscodeSessionDto {
    pub id: String,
    pub user_id: i64,
    pub item_id: i64,
    pub file_id: i64,
    /// An ffmpeg process is currently running for this session.
    pub transcoding: bool,
    pub last_segment: Option<u32>,
}

/// Active transcode sessions (administrators).
#[utoipa::path(
    get,
    path = "/transcode/sessions",
    operation_id = "listTranscodeSessions",
    tag = "playback",
    responses(
        (status = OK, description = "Active sessions", body = [TranscodeSessionDto]),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn list_sessions(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Json<Vec<TranscodeSessionDto>> {
    Json(
        state
            .transcoder
            .sessions()
            .await
            .into_iter()
            .map(|s| TranscodeSessionDto {
                id: s.id,
                user_id: s.user_id,
                item_id: s.item_id,
                file_id: s.file_id,
                transcoding: s.transcoding,
                last_segment: s.last_requested,
            })
            .collect(),
    )
}
