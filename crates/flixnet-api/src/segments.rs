//! Intro, credits and recap segments: reading them for the player, triggering the
//! analysis and correcting it by hand (administrators). Also the personal playback
//! preference that goes with them ("skip intros automatically").

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use flixnet_core::jobs::priority;
use flixnet_db::{items, libraries, media, segments};
use flixnet_library::segments::queue_season;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::{AdminUser, AuthUser};
use crate::browse::ensure_visible;
use crate::error::{ApiError, ApiResult, ErrorBody};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_media_segments))
        .routes(routes!(set_media_segment, delete_media_segment))
        .routes(routes!(analyze_item_segments))
        .routes(routes!(analyze_library_segments))
        .routes(routes!(get_my_preferences, update_my_preferences))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SegmentKind {
    Intro,
    Credits,
    Recap,
}

impl SegmentKind {
    fn as_str(self) -> &'static str {
        match self {
            SegmentKind::Intro => "intro",
            SegmentKind::Credits => "credits",
            SegmentKind::Recap => "recap",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "intro" => Some(SegmentKind::Intro),
            "credits" => Some(SegmentKind::Credits),
            "recap" => Some(SegmentKind::Recap),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SegmentSource {
    /// Audio shared with other episodes of the season.
    Fingerprint,
    /// Black frames near the end of the file.
    Blackdetect,
    /// Set by an administrator.
    Manual,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MediaSegmentDto {
    pub file_id: i64,
    pub kind: SegmentKind,
    /// From the start of the file.
    pub start_ms: i64,
    pub end_ms: i64,
    pub source: SegmentSource,
    /// 0–1: share of compared episodes that agree (1 for manual segments).
    pub confidence: f64,
}

/// Segments as the API shows them (unknown kinds are skipped).
pub(crate) fn to_dtos(rows: Vec<segments::Segment>) -> Vec<MediaSegmentDto> {
    rows.into_iter()
        .filter_map(|s| {
            Some(MediaSegmentDto {
                file_id: s.file_id,
                kind: SegmentKind::parse(&s.kind)?,
                start_ms: s.start_ms,
                end_ms: s.end_ms,
                source: match s.source.as_str() {
                    "manual" => SegmentSource::Manual,
                    "blackdetect" => SegmentSource::Blackdetect,
                    _ => SegmentSource::Fingerprint,
                },
                confidence: s.confidence,
            })
        })
        .collect()
}

/// Segments of one file, for playback info.
pub(crate) async fn for_file(state: &AppState, file_id: i64) -> ApiResult<Vec<MediaSegmentDto>> {
    Ok(to_dtos(segments::for_file(&state.db, file_id).await?))
}

/// Intro, credits and recap of an item's files.
#[utoipa::path(
    get,
    path = "/items/{id}/segments",
    operation_id = "getMediaSegments",
    tag = "playback",
    params(("id" = i64, Path, description = "Item id (episode or movie)")),
    responses(
        (status = OK, description = "Segments of the item's main files, in playback order", body = Vec<MediaSegmentDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) item", body = ErrorBody),
    )
)]
async fn get_media_segments(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<MediaSegmentDto>>> {
    ensure_visible(&state, &user, id).await?;
    Ok(Json(to_dtos(segments::for_item(&state.db, id).await?)))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetSegmentRequest {
    pub start_ms: i64,
    pub end_ms: i64,
    /// One file of the item; all its main files when omitted.
    pub file_id: Option<i64>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SegmentFileQuery {
    /// One file of the item; all its main files when omitted.
    pub file_id: Option<i64>,
}

/// The item's main files, or the one named (which must belong to it).
async fn target_files(
    state: &AppState,
    item_id: i64,
    file_id: Option<i64>,
) -> ApiResult<Vec<media::MediaFile>> {
    if items::get(&state.db, item_id).await?.is_none() {
        return Err(ApiError::not_found("Eintrag nicht gefunden."));
    }
    let files: Vec<media::MediaFile> = media::files_for_item(&state.db, item_id)
        .await?
        .into_iter()
        .filter(|f| f.role == "main" && file_id.is_none_or(|id| f.id == id))
        .collect();
    if files.is_empty() {
        return Err(ApiError::not_found("Keine passende Datei vorhanden."));
    }
    Ok(files)
}

/// Set a segment by hand (administrators). It replaces the detected one and is kept
/// when the season is analyzed again.
#[utoipa::path(
    put,
    path = "/items/{id}/segments/{kind}",
    operation_id = "setMediaSegment",
    tag = "playback",
    params(
        ("id" = i64, Path, description = "Item id (episode or movie)"),
        ("kind" = SegmentKind, Path, description = "Segment kind"),
    ),
    request_body = SetSegmentRequest,
    responses(
        (status = OK, description = "Segments of the item after the change", body = Vec<MediaSegmentDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such item or file", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Times outside the file", body = ErrorBody),
    )
)]
async fn set_media_segment(
    _: AdminUser,
    State(state): State<AppState>,
    Path((id, kind)): Path<(i64, SegmentKind)>,
    Json(body): Json<SetSegmentRequest>,
) -> ApiResult<Json<Vec<MediaSegmentDto>>> {
    let files = target_files(&state, id, body.file_id).await?;
    if body.start_ms < 0 || body.end_ms <= body.start_ms {
        return Err(ApiError::validation(
            "Das Ende muss nach dem Anfang liegen.",
        ));
    }
    for file in &files {
        if file.duration_ms.is_some_and(|d| body.end_ms > d) {
            return Err(ApiError::validation(
                "Der Abschnitt reicht über das Ende der Datei hinaus.",
            ));
        }
    }
    for file in &files {
        segments::set_manual(
            &state.db,
            file.id,
            kind.as_str(),
            body.start_ms,
            body.end_ms,
        )
        .await?;
    }
    Ok(Json(to_dtos(segments::for_item(&state.db, id).await?)))
}

/// Remove a segment (administrators). A detected one may come back with the next
/// analysis of the season.
#[utoipa::path(
    delete,
    path = "/items/{id}/segments/{kind}",
    operation_id = "deleteMediaSegment",
    tag = "playback",
    params(
        ("id" = i64, Path, description = "Item id (episode or movie)"),
        ("kind" = SegmentKind, Path, description = "Segment kind"),
        SegmentFileQuery,
    ),
    responses(
        (status = NO_CONTENT, description = "Segment removed"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such item, file or segment", body = ErrorBody),
    )
)]
async fn delete_media_segment(
    _: AdminUser,
    State(state): State<AppState>,
    Path((id, kind)): Path<(i64, SegmentKind)>,
    Query(query): Query<SegmentFileQuery>,
) -> ApiResult<StatusCode> {
    let mut removed = false;
    for file in target_files(&state, id, query.file_id).await? {
        removed |= segments::delete(&state.db, file.id, kind.as_str()).await?;
    }
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("Kein solcher Abschnitt vorhanden."))
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalysisQueued {
    /// Seasons whose analysis was queued (already pending ones are not counted).
    pub seasons: usize,
}

async fn queue_seasons(state: &AppState, seasons: Vec<i64>) -> ApiResult<AnalysisQueued> {
    let mut queued = 0;
    for season_id in seasons {
        if queue_season(state.jobs.queue(), season_id, priority::INTERACTIVE, None)
            .await?
            .is_some()
        {
            queued += 1;
        }
    }
    Ok(AnalysisQueued { seasons: queued })
}

/// Detect intros and credits of a series, season or episode's season now (administrators).
#[utoipa::path(
    post,
    path = "/items/{id}/segments/analyze",
    operation_id = "analyzeItemSegments",
    tag = "playback",
    params(("id" = i64, Path, description = "Series, season or episode id")),
    responses(
        (status = ACCEPTED, description = "Analysis queued", body = AnalysisQueued),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such item", body = ErrorBody),
    )
)]
async fn analyze_item_segments(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<(StatusCode, Json<AnalysisQueued>)> {
    if items::get(&state.db, id).await?.is_none() {
        return Err(ApiError::not_found("Eintrag nicht gefunden."));
    }
    let seasons = segments::seasons_below(&state.db, id).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(queue_seasons(&state, seasons).await?),
    ))
}

/// Detect intros and credits in every season of a library now (administrators).
#[utoipa::path(
    post,
    path = "/libraries/{id}/segments/analyze",
    operation_id = "analyzeLibrarySegments",
    tag = "libraries",
    params(("id" = i64, Path, description = "Library id")),
    responses(
        (status = ACCEPTED, description = "Analysis queued", body = AnalysisQueued),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such library", body = ErrorBody),
    )
)]
async fn analyze_library_segments(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<(StatusCode, Json<AnalysisQueued>)> {
    if libraries::get(&state.db, id).await?.is_none() {
        return Err(ApiError::not_found("Bibliothek nicht gefunden."));
    }
    let seasons = segments::library_seasons(&state.db, id).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(queue_seasons(&state, seasons).await?),
    ))
}

// ---- Personal preferences -----------------------------------------------------------

const SKIP_INTRO_KEY: &str = "skip_intro_automatically";

#[derive(Debug, Serialize, ToSchema)]
pub struct PreferencesDto {
    /// Jump over intros (and recaps) without a click.
    pub skip_intro_automatically: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdatePreferencesRequest {
    pub skip_intro_automatically: Option<bool>,
}

async fn preferences(state: &AppState, user_id: i64) -> ApiResult<PreferencesDto> {
    Ok(PreferencesDto {
        skip_intro_automatically: segments::preference(&state.db, user_id, SKIP_INTRO_KEY)
            .await?
            .unwrap_or(false),
    })
}

/// Playback preferences of the signed-in profile.
#[utoipa::path(
    get,
    path = "/users/me/preferences",
    operation_id = "getMyPreferences",
    tag = "users",
    responses(
        (status = OK, description = "The preferences", body = PreferencesDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn get_my_preferences(
    user: AuthUser,
    State(state): State<AppState>,
) -> ApiResult<Json<PreferencesDto>> {
    Ok(Json(preferences(&state, user.user_id).await?))
}

/// Change playback preferences of the signed-in profile (omitted fields stay).
#[utoipa::path(
    patch,
    path = "/users/me/preferences",
    operation_id = "updateMyPreferences",
    tag = "users",
    request_body = UpdatePreferencesRequest,
    responses(
        (status = OK, description = "The updated preferences", body = PreferencesDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn update_my_preferences(
    user: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<UpdatePreferencesRequest>,
) -> ApiResult<Json<PreferencesDto>> {
    if let Some(skip) = body.skip_intro_automatically {
        segments::set_preference(&state.db, user.user_id, SKIP_INTRO_KEY, &skip).await?;
    }
    Ok(Json(preferences(&state, user.user_id).await?))
}
