//! OpenSubtitles (administrators): settings, searching subtitles for a movie or
//! episode and downloading one, which then shows up as a subtitle track at once.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use flixnet_db::settings;
use flixnet_library::subtitles::{
    self, DownloadError, OpenSubtitlesError, SubtitleSettings, Wanted,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AdminUser;
use crate::browse::ensure_visible;
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::validate;

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_subtitle_settings, update_subtitle_settings))
        .routes(routes!(search_subtitles))
        .routes(routes!(download_subtitle))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SubtitleSettingsDto {
    /// Whether an OpenSubtitles API key is stored (the key itself is never returned).
    pub has_api_key: bool,
    /// OpenSubtitles account (more downloads per day).
    pub username: Option<String>,
    /// Whether a password for the account is stored.
    pub has_password: bool,
    /// Download subtitles for new movies and episodes automatically.
    pub auto_download: bool,
    /// Languages for automatic downloads (ISO 639-1, e.g. `de`).
    pub languages: Vec<String>,
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct UpdateSubtitleSettings {
    /// OpenSubtitles.com API key (consumer key); `null` removes it.
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub api_key: Option<Option<String>>,
    /// `null` removes the account.
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub username: Option<Option<String>>,
    /// `null` removes it.
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub password: Option<Option<String>>,
    pub auto_download: Option<bool>,
    /// ISO 639-1 codes (`de`, `en`, `pt-br`), at most 10.
    pub languages: Option<Vec<String>>,
}

fn to_dto(s: &SubtitleSettings) -> SubtitleSettingsDto {
    SubtitleSettingsDto {
        has_api_key: s.api_key.as_deref().is_some_and(|k| !k.is_empty()),
        username: s.username.clone(),
        has_password: s.password.as_deref().is_some_and(|p| !p.is_empty()),
        auto_download: s.auto_download,
        languages: s.languages.clone(),
    }
}

fn provider_error(err: OpenSubtitlesError) -> ApiError {
    match err {
        OpenSubtitlesError::InvalidKey => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "opensubtitles_rejected",
            "OpenSubtitles hat den API-Schlüssel abgelehnt.",
        ),
        OpenSubtitlesError::InvalidLogin => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "opensubtitles_rejected",
            "OpenSubtitles hat Benutzername oder Passwort abgelehnt.",
        ),
        OpenSubtitlesError::QuotaExhausted(_) => ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "quota_exhausted",
            "Das tägliche Download-Kontingent bei OpenSubtitles ist aufgebraucht.",
        ),
        OpenSubtitlesError::RateLimited => ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "OpenSubtitles bittet um eine kurze Pause. Bitte gleich noch einmal versuchen.",
        ),
        other => {
            tracing::warn!(err = %other, "OpenSubtitles request failed");
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "opensubtitles_unavailable",
                "OpenSubtitles ist gerade nicht erreichbar.",
            )
        }
    }
}

async fn load(state: &AppState) -> ApiResult<SubtitleSettings> {
    subtitles::load_settings(&state.db)
        .await
        .map_err(ApiError::internal)
}

/// OpenSubtitles settings.
#[utoipa::path(
    get,
    path = "/settings/subtitles",
    operation_id = "getSubtitleSettings",
    tag = "settings",
    responses(
        (status = OK, description = "Current settings", body = SubtitleSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn get_subtitle_settings(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<SubtitleSettingsDto>> {
    Ok(Json(to_dto(&load(&state).await?)))
}

/// Change OpenSubtitles settings. A new key or login is checked against OpenSubtitles.
#[utoipa::path(
    put,
    path = "/settings/subtitles",
    operation_id = "updateSubtitleSettings",
    tag = "settings",
    request_body = UpdateSubtitleSettings,
    responses(
        (status = OK, description = "Updated settings", body = SubtitleSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid key, login or languages", body = ErrorBody),
    )
)]
async fn update_subtitle_settings(
    _: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<UpdateSubtitleSettings>,
) -> ApiResult<Json<SubtitleSettingsDto>> {
    let mut current = load(&state).await?;
    let trimmed = |v: Option<String>| v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    let credentials_changed =
        body.api_key.is_some() || body.username.is_some() || body.password.is_some();
    if let Some(key) = body.api_key {
        current.api_key = trimmed(key);
    }
    if let Some(username) = body.username {
        current.username = trimmed(username);
    }
    if let Some(password) = body.password {
        current.password = password.filter(|p| !p.is_empty());
    }
    if let Some(languages) = body.languages {
        let mut list: Vec<String> = Vec::new();
        for language in languages {
            let language = language.trim().to_lowercase();
            if !subtitles::is_language(&language) {
                return Err(ApiError::validation(format!(
                    "Unbekannter Sprachcode „{language}“ (z. B. „de“ oder „pt-br“)."
                )));
            }
            if !list.contains(&language) {
                list.push(language);
            }
        }
        if list.len() > 10 {
            return Err(ApiError::validation("Höchstens 10 Sprachen."));
        }
        current.languages = list;
    }
    if let Some(auto) = body.auto_download {
        if auto && !current.auto_download {
            // Only items added from now on count as new.
            current.auto_since_ms = Some(flixnet_db::now_ms());
        }
        current.auto_download = auto;
    }
    if current.auto_download && current.languages.is_empty() {
        return Err(ApiError::validation(
            "Für automatische Downloads mindestens eine Sprache wählen.",
        ));
    }
    if credentials_changed && let Some(client) = current.client() {
        // Reject what OpenSubtitles refuses; network trouble does not block saving.
        match client.verify().await {
            Err(OpenSubtitlesError::InvalidKey) => {
                return Err(ApiError::validation(
                    "Der OpenSubtitles-Schlüssel ist ungültig.",
                ));
            }
            Err(OpenSubtitlesError::InvalidLogin) => {
                return Err(ApiError::validation(
                    "Benutzername oder Passwort für OpenSubtitles ist falsch.",
                ));
            }
            Err(err) => tracing::warn!(%err, "OpenSubtitles key not verified"),
            Ok(()) => {}
        }
    }
    settings::set(&state.db, subtitles::SETTINGS_KEY, &current).await?;
    Ok(Json(to_dto(&current)))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SubtitleSearchQuery {
    /// ISO 639-1 language (`de`, `en`, `pt-br`).
    pub language: String,
    /// Version of the item (media file id); default: the first main file.
    pub file_id: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SubtitleCandidateDto {
    /// OpenSubtitles file id, for `downloadSubtitle`.
    pub subtitle_file_id: i64,
    pub file_name: Option<String>,
    /// Release the subtitle was made for.
    pub release: Option<String>,
    pub language: Option<String>,
    pub download_count: i64,
    /// Rating 0–10 (0 = none yet).
    pub rating: f64,
    pub votes: i64,
    pub hearing_impaired: bool,
    /// Translates only foreign-language parts.
    pub foreign_parts_only: bool,
    pub fps: Option<f64>,
    pub hd: bool,
    pub from_trusted: bool,
    /// Machine or AI translated.
    pub machine_translated: bool,
    /// Made for exactly this file (same OpenSubtitles hash).
    pub moviehash_match: bool,
    /// How well the release fits the file, 0–100 (100 = hash match).
    pub match_score: i64,
    pub uploader: Option<String>,
    pub upload_date: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SubtitleSearchDto {
    /// The media file searched for.
    pub file_id: i64,
    pub file_name: String,
    /// OpenSubtitles hash of the file, if it could be read.
    pub moviehash: Option<String>,
    /// Best matches first.
    pub results: Vec<SubtitleCandidateDto>,
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

async fn configured(state: &AppState) -> ApiResult<flixnet_library::subtitles::OpenSubtitles> {
    load(state).await?.client().ok_or_else(|| {
        ApiError::conflict(
            "opensubtitles_not_configured",
            "Unter Einstellungen → Untertitel ist noch kein OpenSubtitles-Schlüssel eingetragen.",
        )
    })
}

fn check_language(language: &str) -> ApiResult<String> {
    let language = language.trim().to_lowercase();
    if subtitles::is_language(&language) {
        Ok(language)
    } else {
        Err(ApiError::validation(
            "Sprache als Code angeben, z. B. „de“.",
        ))
    }
}

/// Search OpenSubtitles for subtitles of a movie or episode (administrators).
#[utoipa::path(
    get,
    path = "/items/{id}/subtitles/search",
    operation_id = "searchSubtitles",
    tag = "playback",
    params(("id" = i64, Path, description = "Movie or episode id"), SubtitleSearchQuery),
    responses(
        (status = OK, description = "Matching subtitles, best first", body = SubtitleSearchDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such movie/episode with a file", body = ErrorBody),
        (status = CONFLICT, description = "No OpenSubtitles key configured", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid language", body = ErrorBody),
        (status = TOO_MANY_REQUESTS, description = "OpenSubtitles rate limit", body = ErrorBody),
        (status = BAD_GATEWAY, description = "OpenSubtitles failed or rejected the key", body = ErrorBody),
    )
)]
async fn search_subtitles(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(query): Query<SubtitleSearchQuery>,
) -> ApiResult<Json<SubtitleSearchDto>> {
    let language = check_language(&query.language)?;
    ensure_visible(&state, &user, id).await?;
    let client = configured(&state).await?;
    let target = subtitles::target(
        &state.db,
        id,
        query.file_id,
        std::slice::from_ref(&language),
    )
    .await
    .map_err(ApiError::internal)?
    .ok_or_else(|| ApiError::not_found("Kein Film und keine Folge mit Datei."))?;
    let results = client.search(&target.query).await.map_err(provider_error)?;
    let results = subtitles::rank(results, &target.file.path)
        .into_iter()
        .map(|r| SubtitleCandidateDto {
            match_score: r.score,
            subtitle_file_id: r.result.file_id,
            file_name: r.result.file_name,
            release: r.result.release,
            language: r.result.language,
            download_count: r.result.download_count,
            rating: r.result.ratings,
            votes: r.result.votes,
            hearing_impaired: r.result.hearing_impaired,
            foreign_parts_only: r.result.foreign_parts_only,
            fps: r.result.fps,
            hd: r.result.hd,
            from_trusted: r.result.from_trusted,
            machine_translated: r.result.machine_translated,
            moviehash_match: r.result.moviehash_match,
            uploader: r.result.uploader,
            upload_date: r.result.upload_date,
        })
        .collect();
    Ok(Json(SubtitleSearchDto {
        file_id: target.file.id,
        file_name: file_name(&target.file.path),
        moviehash: target.query.moviehash,
        results,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct DownloadSubtitleRequest {
    /// `subtitle_file_id` from the search.
    pub subtitle_file_id: i64,
    /// ISO 639-1 language of the subtitle (`de`).
    pub language: String,
    /// Version of the item (media file id); default: the first main file.
    pub file_id: Option<i64>,
    #[serde(default)]
    pub hearing_impaired: bool,
    #[serde(default)]
    pub forced: bool,
    /// Release name from the search, kept for reference.
    pub release: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DownloadedSubtitleDto {
    /// The new subtitle track (`subtitle_stream_index` in playback info).
    pub stream_index: i64,
    pub file_id: i64,
    pub language: String,
    pub file_name: String,
    /// `true`: saved next to the media file; `false`: in Flixnet's subtitle store
    /// (the library folder is not writable).
    pub saved_in_library: bool,
    /// Downloads left today, if OpenSubtitles said.
    pub remaining_downloads: Option<i64>,
}

/// Download a subtitle from OpenSubtitles for a movie or episode (administrators).
///
/// Saved next to the media file when its folder is writable, otherwise in the config
/// directory; either way it is a subtitle track of the file right away.
#[utoipa::path(
    post,
    path = "/items/{id}/subtitles/download",
    operation_id = "downloadSubtitle",
    tag = "playback",
    params(("id" = i64, Path, description = "Movie or episode id")),
    request_body = DownloadSubtitleRequest,
    responses(
        (status = CREATED, description = "Downloaded and added as a track", body = DownloadedSubtitleDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such movie/episode with a file", body = ErrorBody),
        (status = CONFLICT, description = "No OpenSubtitles key configured", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid language", body = ErrorBody),
        (status = TOO_MANY_REQUESTS, description = "Rate limit or daily download quota reached", body = ErrorBody),
        (status = BAD_GATEWAY, description = "OpenSubtitles failed or rejected the key", body = ErrorBody),
    )
)]
async fn download_subtitle(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<DownloadSubtitleRequest>,
) -> ApiResult<(StatusCode, Json<DownloadedSubtitleDto>)> {
    let language = check_language(&body.language)?;
    let release =
        body.release.as_deref().map(str::trim).filter(|r| {
            !r.is_empty() && r.chars().count() <= 300 && !r.chars().any(char::is_control)
        });
    ensure_visible(&state, &user, id).await?;
    let client = configured(&state).await?;
    let target = subtitles::target(&state.db, id, body.file_id, &[])
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("Kein Film und keine Folge mit Datei."))?;
    let store_dir = state.config.paths.config_dir.join("subtitles");
    let wanted = Wanted {
        subtitle_file_id: body.subtitle_file_id,
        language: &language,
        hearing_impaired: body.hearing_impaired,
        forced: body.forced,
        release,
    };
    let saved = subtitles::download(&state.db, &client, &store_dir, &target.file, &wanted)
        .await
        .map_err(|err| match err {
            DownloadError::Provider(err) => provider_error(err),
            DownloadError::Store(err) => ApiError::internal(err),
            DownloadError::Db(err) => err.into(),
        })?;
    Ok((
        StatusCode::CREATED,
        Json(DownloadedSubtitleDto {
            stream_index: saved.stream_index,
            file_id: target.file.id,
            language: subtitles::stream_language(&language),
            file_name: file_name(&saved.path.to_string_lossy()),
            saved_in_library: !saved.in_store,
            remaining_downloads: saved.remaining,
        }),
    ))
}
