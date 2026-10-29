//! Playback: which sources an item has and how to stream them.
//!
//! `playback-info` runs the decision engine against the client's device profile.
//! `/stream/{file_id}` serves the original file with HTTP range support (Direct Play).
//! `<video>` elements cannot send an `Authorization` header, so playback info hands
//! out signed URLs bound to the file *and* the user; every request re-checks that the
//! user still exists and may see the library, so revoking access takes effect at once.

use std::path::{Path as FsPath, PathBuf};

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use flixnet_db::{items, libraries, media, users};
use flixnet_media::decision::{
    DeviceProfile, PlayMethod, PlaybackChoice, PlaybackPlan, SourceAudio, SourceMedia,
    SourceSubtitle, SourceVideo, StreamAction, decide,
};
use flixnet_media::hls::Segments;
use flixnet_media::transcode::SessionSpec;
use serde::{Deserialize, Serialize};
use tower_http::services::ServeFile;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AuthUser;
use crate::browse::ensure_visible;
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::tracks::{self, AudioTrackDto, SubtitleTrackDto};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_playback_info))
        .routes(routes!(stream_file))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct PlaybackInfoRequest {
    /// What the client can play directly; an empty profile means "transcode everything".
    #[serde(default)]
    pub device_profile: DeviceProfile,
    /// Chosen audio stream (file stream index); default track when omitted.
    pub audio_stream_index: Option<i64>,
    /// Chosen subtitle stream (file stream index); none when omitted.
    pub subtitle_stream_index: Option<i64>,
    /// Quality limit chosen by the user (bits/s).
    pub max_bitrate: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PlaybackInfoDto {
    pub item_id: i64,
    /// Playable versions/parts of the item, main versions first.
    pub sources: Vec<MediaSourceDto>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MediaSourceDto {
    pub file_id: i64,
    pub file_name: String,
    /// Container as probed (e.g. `matroska,webm`, `mov,mp4,m4a,3gp,3g2,mj2`).
    pub container: Option<String>,
    pub size: i64,
    pub duration_ms: Option<i64>,
    pub bitrate: Option<i64>,
    /// Part number for multi-part movies (`cd1`, `part2`, …).
    pub part: Option<i64>,
    /// Signed URL of the original file (supports HTTP range requests).
    pub direct_url: String,
    /// HLS playlist of a transcode session, when the plan is not Direct Play.
    pub hls_url: Option<String>,
    /// How this source would be played on the requesting device, and why.
    pub plan: PlaybackPlan,
    /// Audio tracks to choose from (`audio_stream_index`).
    pub audio_tracks: Vec<AudioTrackDto>,
    /// Subtitle tracks to choose from: text ones with a WebVTT URL, picture ones
    /// burned in when chosen (`subtitle_stream_index`).
    pub subtitle_tracks: Vec<SubtitleTrackDto>,
}

/// Sources of a movie or episode with a playback decision for the client's device.
#[utoipa::path(
    post,
    path = "/items/{id}/playback-info",
    operation_id = "getPlaybackInfo",
    tag = "playback",
    params(("id" = i64, Path, description = "Item id (movie or episode)")),
    request_body = PlaybackInfoRequest,
    responses(
        (status = OK, description = "Playable sources with their playback plans", body = PlaybackInfoDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such item, or nothing playable", body = ErrorBody),
    )
)]
async fn get_playback_info(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(request): Json<PlaybackInfoRequest>,
) -> ApiResult<Json<PlaybackInfoDto>> {
    ensure_visible(&state, &user, id).await?;
    let choice = PlaybackChoice {
        audio_index: request.audio_stream_index,
        subtitle_index: request.subtitle_stream_index,
        max_bitrate: request.max_bitrate,
    };
    let mut sources = Vec::new();
    for file in media::files_for_item(&state.db, id).await? {
        if file.role != "main" {
            continue;
        }
        let streams = media::streams(&state.db, file.id).await?;
        let plan = decide(
            &source_media(&file, &streams),
            &request.device_profile,
            choice,
        );
        let hls_url = if plan.method == PlayMethod::DirectPlay {
            None
        } else {
            let (session_plan, segments) = session_layout(&state, &file, &plan).await?;
            let session = state.transcoder.create(SessionSpec {
                user_id: user.user_id,
                item_id: id,
                file_id: file.id,
                input: PathBuf::from(&file.path),
                segments,
                plan: session_plan,
            });
            Some(format!("/api/v1/transcode/{session}/main.m3u8"))
        };
        sources.push(MediaSourceDto {
            direct_url: state.signer.stream_url(file.id, user.user_id),
            hls_url,
            file_name: file_name(&file.path),
            file_id: file.id,
            container: file.container,
            size: file.size,
            duration_ms: file.duration_ms,
            bitrate: file.bitrate,
            part: file.part,
            plan,
            audio_tracks: tracks::audio_tracks(&streams),
            subtitle_tracks: tracks::subtitle_tracks(
                &state.signer,
                file.id,
                user.user_id,
                &streams,
            ),
        });
    }
    if sources.is_empty() {
        return Err(ApiError::not_found("Keine abspielbare Datei vorhanden."));
    }
    Ok(Json(PlaybackInfoDto {
        item_id: id,
        sources,
    }))
}

/// What a session runs and how it is cut. Copied video needs the file's keyframe
/// index (cached in the database, computed on first use); without one the video is
/// re-encoded instead.
async fn session_layout(
    state: &AppState,
    file: &media::MediaFile,
    plan: &PlaybackPlan,
) -> ApiResult<(PlaybackPlan, Segments)> {
    let duration_ms = file.duration_ms.unwrap_or(0);
    let mut plan = plan.clone();
    let Some(video) = plan
        .video
        .as_mut()
        .filter(|v| v.action == StreamAction::Copy)
    else {
        return Ok((plan, Segments::fixed(duration_ms)));
    };
    let keyframes = match media::keyframes(&state.db, file.id, video.stream_index).await? {
        Some(cached) => Some(cached),
        None => match state
            .ffprobe
            .keyframes(FsPath::new(&file.path), video.stream_index)
            .await
        {
            Ok(times) => {
                media::store_keyframes(&state.db, file.id, video.stream_index, &times).await?;
                Some(times)
            }
            Err(err) => {
                tracing::warn!(file = file.id, %err, "no keyframe index; transcoding the video");
                None
            }
        },
    };
    if let Some(segments) = keyframes.and_then(|k| Segments::keyframes(&k, duration_ms)) {
        return Ok((plan, segments));
    }
    video.action = StreamAction::Transcode;
    video.codec = "h264".to_owned();
    Ok((plan, Segments::fixed(duration_ms)))
}

/// Embedded cover art shows up as a video stream; it is not the movie.
const COVER_CODECS: &[&str] = &["mjpeg", "png", "bmp", "gif", "webp"];

/// The decision engine's view of a probed file.
fn source_media(file: &media::MediaFile, streams: &[media::Stream]) -> SourceMedia {
    let codec = |s: &media::Stream| s.codec.clone().unwrap_or_default();
    SourceMedia {
        container: FsPath::new(&file.path)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default(),
        bitrate: file.bitrate,
        video: streams
            .iter()
            .find(|s| s.kind == "video" && !COVER_CODECS.contains(&codec(s).as_str()))
            .map(|s| SourceVideo {
                index: s.idx,
                codec: codec(s),
                width: s.width.unwrap_or(0),
                height: s.height.unwrap_or(0),
                bit_depth: s.bit_depth.unwrap_or(8),
                hdr: s.hdr.clone().unwrap_or_else(|| "sdr".to_owned()),
            }),
        audio: streams
            .iter()
            .filter(|s| s.kind == "audio")
            .map(|s| SourceAudio {
                index: s.idx,
                codec: codec(s),
                channels: s.channels.unwrap_or(2),
                is_default: s.is_default,
            })
            .collect(),
        subtitles: streams
            .iter()
            .filter(|s| s.kind == "subtitle")
            .map(|s| SourceSubtitle {
                index: s.idx,
                codec: codec(s),
                is_external: s.is_external,
            })
            .collect(),
    }
}

fn file_name(path: &str) -> String {
    FsPath::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The file a media request may read. `signed_user` is the user a verified URL
/// signature was issued to: the signature proves who it was, but that user's access
/// is checked again on every request. Without one, a signed-in user must see the item.
pub(crate) async fn accessible_file(
    state: &AppState,
    user: Option<&AuthUser>,
    signed_user: Option<i64>,
    file_id: i64,
) -> ApiResult<media::MediaFile> {
    let not_found = || ApiError::not_found("Datei nicht gefunden.");
    if signed_user.is_none() && user.is_none() {
        return Err(ApiError::unauthorized());
    }
    let file = media::get_file(&state.db, file_id)
        .await?
        .filter(|f| f.missing_since.is_none())
        .ok_or_else(not_found)?;
    let item_id = file.item_id.ok_or_else(not_found)?;
    if let Some(user_id) = signed_user {
        let user = users::get(&state.db, user_id)
            .await?
            .ok_or_else(ApiError::unauthorized)?;
        if !user.is_admin
            && !libraries::user_can_access(&state.db, user.id, file.library_id).await?
        {
            return Err(not_found());
        }
    } else if let Some(user) = user {
        ensure_visible(state, user, item_id).await?;
    }
    if items::get(&state.db, item_id).await?.is_none() {
        return Err(not_found());
    }
    Ok(file)
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct StreamQuery {
    /// User the URL was issued to, from URLs returned by the API.
    pub u: Option<i64>,
    /// Expiry of the signature (unix seconds).
    pub exp: Option<i64>,
    /// URL signature.
    pub sig: Option<String>,
}

/// The original media file (Direct Play), with HTTP range support.
///
/// Use the signed `direct_url` from playback info; alternatively authenticate normally.
#[utoipa::path(
    get,
    path = "/stream/{file_id}",
    operation_id = "streamFile",
    tag = "playback",
    security(()),
    params(("file_id" = i64, Path, description = "Media file id"), StreamQuery),
    responses(
        (status = OK, description = "The whole file", content_type = "application/octet-stream"),
        (status = PARTIAL_CONTENT, description = "The requested byte range", content_type = "application/octet-stream"),
        (status = RANGE_NOT_SATISFIABLE, description = "Range outside the file"),
        (status = UNAUTHORIZED, description = "Neither a valid signature nor a valid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such file, or no access", body = ErrorBody),
    )
)]
async fn stream_file(
    user: Option<AuthUser>,
    State(state): State<AppState>,
    Path(file_id): Path<i64>,
    Query(query): Query<StreamQuery>,
    request: Request,
) -> ApiResult<Response> {
    let not_found = || ApiError::not_found("Datei nicht gefunden.");
    let signed_user = match (query.u, query.exp, query.sig.as_deref()) {
        (Some(u), Some(exp), Some(sig)) if state.signer.verify_stream(file_id, u, exp, sig) => {
            Some(u)
        }
        _ => None,
    };
    let file = accessible_file(&state, user.as_ref(), signed_user, file_id).await?;

    // The path comes from the database, never from the request.
    let response = ServeFile::new(&file.path)
        .try_call(request)
        .await
        .map_err(|err| {
            tracing::warn!(file = file_id, %err, "stream unavailable");
            not_found()
        })?;
    if response.status() == StatusCode::NOT_FOUND {
        return Err(not_found());
    }
    let mut response = response.map(Body::new);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-transform"),
    );
    Ok(response)
}
