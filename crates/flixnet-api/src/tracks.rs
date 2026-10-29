//! Audio and subtitle tracks of a media file, and text subtitles as WebVTT.
//!
//! Playback info lists the tracks of every source; text subtitles come with a signed
//! WebVTT URL bound to user and file (`<track>` elements cannot send headers), picture
//! subtitles are burned into the video by the transcode when chosen.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use flixnet_db::media;
use flixnet_media::subtitles::{SubtitleError, SubtitleInput, is_text_subtitle};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::playback::{StreamQuery, accessible_file};
use crate::signing::UrlSigner;

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_subtitle_vtt))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AudioTrackDto {
    /// Stream index within the file (`audio_stream_index` in playback info).
    pub index: i64,
    pub codec: Option<String>,
    /// ISO 639-2 language code (`deu`, `eng`, …).
    pub language: Option<String>,
    pub title: Option<String>,
    pub channels: Option<i64>,
    pub channel_layout: Option<String>,
    pub is_default: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SubtitleTrackDto {
    /// Stream index within the file; sidecar files are numbered from 1000
    /// (`subtitle_stream_index` in playback info).
    pub index: i64,
    /// ffprobe codec name, or the file extension of a sidecar file.
    pub codec: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_default: bool,
    /// Only translates foreign-language parts.
    pub is_forced: bool,
    /// For the deaf and hard of hearing.
    pub is_sdh: bool,
    /// A sidecar file next to the media file.
    pub is_external: bool,
    /// Signed WebVTT URL for text subtitles, rendered by the player.
    pub vtt_url: Option<String>,
    /// Picture subtitles: choosing them burns them into the video (a transcode).
    pub burn_in: bool,
}

pub(crate) fn audio_tracks(streams: &[media::Stream]) -> Vec<AudioTrackDto> {
    streams
        .iter()
        .filter(|s| s.kind == "audio")
        .map(|s| AudioTrackDto {
            index: s.idx,
            codec: s.codec.clone(),
            language: s.language.clone(),
            title: s.title.clone(),
            channels: s.channels,
            channel_layout: s.channel_layout.clone(),
            is_default: s.is_default,
        })
        .collect()
}

pub(crate) fn subtitle_tracks(
    signer: &UrlSigner,
    file_id: i64,
    user_id: i64,
    streams: &[media::Stream],
) -> Vec<SubtitleTrackDto> {
    streams
        .iter()
        .filter(|s| s.kind == "subtitle")
        .map(|s| {
            let codec = s.codec.as_deref().unwrap_or_default();
            let text = is_text_subtitle(codec, s.is_external);
            SubtitleTrackDto {
                index: s.idx,
                codec: s.codec.clone(),
                language: s.language.clone(),
                title: s.title.clone(),
                is_default: s.is_default,
                is_forced: s.is_forced,
                is_sdh: s.is_sdh,
                is_external: s.is_external,
                vtt_url: text.then(|| signer.subtitle_url(file_id, s.idx, user_id)),
                // External picture subtitles (.sup, .idx/.sub) are not supported yet.
                burn_in: !text && !s.is_external,
            }
        })
        .collect()
}

/// A text subtitle track as WebVTT (`{stream_index}.vtt`), converted on first use.
///
/// Use the signed `vtt_url` from playback info; alternatively authenticate normally.
#[utoipa::path(
    get,
    path = "/subtitles/{file_id}/{track}",
    operation_id = "getSubtitleVtt",
    tag = "playback",
    security(()),
    params(
        ("file_id" = i64, Path, description = "Media file id"),
        ("track" = String, Path, description = "Subtitle stream index plus `.vtt`, e.g. `1000.vtt`"),
        StreamQuery,
    ),
    responses(
        (status = OK, description = "The subtitles", content_type = "text/vtt", body = String),
        (status = UNAUTHORIZED, description = "Neither a valid signature nor a valid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such file or text subtitle track, or no access", body = ErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "The conversion took too long", body = ErrorBody),
    )
)]
async fn get_subtitle_vtt(
    user: Option<AuthUser>,
    State(state): State<AppState>,
    Path((file_id, track)): Path<(i64, String)>,
    Query(query): Query<StreamQuery>,
) -> ApiResult<Response> {
    let no_track = || ApiError::not_found("Untertitelspur nicht gefunden.");
    let signed_user = match (query.u, query.exp, query.sig.as_deref()) {
        (Some(u), Some(exp), Some(sig)) if state.signer.verify_subtitle(file_id, u, exp, sig) => {
            Some(u)
        }
        _ => None,
    };
    let file = accessible_file(&state, user.as_ref(), signed_user, file_id).await?;
    let index: i64 = track
        .strip_suffix(".vtt")
        .and_then(|n| n.parse().ok())
        .ok_or_else(no_track)?;
    let streams = media::streams(&state.db, file_id).await?;
    let stream = streams
        .into_iter()
        .find(|s| s.kind == "subtitle" && s.idx == index)
        .filter(|s| is_text_subtitle(s.codec.as_deref().unwrap_or_default(), s.is_external))
        .ok_or_else(no_track)?;
    // Paths come from the database, never from the request.
    let input = match (stream.is_external, stream.external_path) {
        (true, Some(path)) => SubtitleInput::External {
            path: path.into(),
            track: index,
        },
        (true, None) => return Err(no_track()),
        (false, _) => SubtitleInput::Embedded {
            file: file.path.into(),
            stream_index: index,
        },
    };
    let path = state
        .subtitles
        .webvtt(file_id, input)
        .await
        .map_err(|err| match err {
            SubtitleError::Source(_) => no_track(),
            SubtitleError::Timeout(_) => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "subtitle_timeout",
                "Die Untertitel konnten nicht rechtzeitig umgewandelt werden.",
            ),
            other => ApiError::internal(other),
        })?;
    let bytes = tokio::fs::read(&path).await.map_err(ApiError::internal)?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/vtt; charset=utf-8"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=86400"),
            ),
        ],
        bytes,
    )
        .into_response())
}
