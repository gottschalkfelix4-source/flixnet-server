//! `/api/v1/settings/*`: server-wide settings (administrators).

use axum::Json;
use axum::extract::State;
use flixnet_db::settings;
use flixnet_library::providers::{self, MetadataSettings, Tmdb, TmdbConfig, TmdbError};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AdminUser;
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::validate;

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_metadata_settings, update_metadata_settings))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MetadataSettingsDto {
    /// Whether a TMDB API key is stored (the key itself is never returned).
    pub has_tmdb_api_key: bool,
    /// Whether a fanart.tv API key is stored.
    pub has_fanart_api_key: bool,
    /// Metadata language, e.g. `de-DE`.
    pub language: String,
    /// Country for age ratings, e.g. `DE`.
    pub country: String,
    /// Music metadata and covers from MusicBrainz and the Cover Art Archive.
    pub musicbrainz_enabled: bool,
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct UpdateMetadataSettings {
    /// TMDB v3 API key or v4 read access token; `null` removes it.
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub tmdb_api_key: Option<Option<String>>,
    /// fanart.tv personal API key; `null` removes it.
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub fanart_api_key: Option<Option<String>>,
    pub language: Option<String>,
    pub country: Option<String>,
    /// Switch MusicBrainz / Cover Art Archive lookups for music on or off.
    pub musicbrainz_enabled: Option<bool>,
}

fn to_dto(s: &MetadataSettings) -> MetadataSettingsDto {
    MetadataSettingsDto {
        has_fanart_api_key: s.fanart_api_key.as_deref().is_some_and(|k| !k.is_empty()),
        has_tmdb_api_key: s.tmdb_api_key.as_deref().is_some_and(|k| !k.is_empty()),
        language: s.language.clone().unwrap_or_else(|| "de-DE".to_owned()),
        country: s.country.clone().unwrap_or_else(|| "DE".to_owned()),
        musicbrainz_enabled: s.musicbrainz_enabled != Some(false),
    }
}

/// Metadata provider settings.
#[utoipa::path(
    get,
    path = "/settings/metadata",
    operation_id = "getMetadataSettings",
    tag = "settings",
    responses(
        (status = OK, description = "Current settings", body = MetadataSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn get_metadata_settings(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<MetadataSettingsDto>> {
    let current = providers::load_settings(&state.db)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(to_dto(&current)))
}

/// Change metadata provider settings. A new TMDB key is checked against TMDB.
#[utoipa::path(
    put,
    path = "/settings/metadata",
    operation_id = "updateMetadataSettings",
    tag = "settings",
    request_body = UpdateMetadataSettings,
    responses(
        (status = OK, description = "Updated settings", body = MetadataSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid key, language or country", body = ErrorBody),
    )
)]
async fn update_metadata_settings(
    _: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<UpdateMetadataSettings>,
) -> ApiResult<Json<MetadataSettingsDto>> {
    let mut current = providers::load_settings(&state.db)
        .await
        .map_err(ApiError::internal)?;

    if let Some(language) = body.language {
        let valid = language.len() == 5
            && language.as_bytes()[2] == b'-'
            && language[..2].chars().all(|c| c.is_ascii_lowercase())
            && language[3..].chars().all(|c| c.is_ascii_uppercase());
        if !valid {
            return Err(ApiError::validation("Sprache im Format „de-DE“ angeben."));
        }
        current.language = Some(language);
    }
    if let Some(country) = body.country {
        if country.len() != 2 || !country.chars().all(|c| c.is_ascii_uppercase()) {
            return Err(ApiError::validation(
                "Land als zweistelligen Code angeben, z. B. „DE“.",
            ));
        }
        current.country = Some(country);
    }
    match body.tmdb_api_key {
        Some(Some(key)) => {
            let key = key.trim().to_owned();
            if key.is_empty() {
                return Err(ApiError::validation("Der TMDB-Schlüssel ist leer."));
            }
            // Reject keys TMDB refuses; network trouble does not block saving.
            let probe = Tmdb::new(TmdbConfig::new(key.clone()), state.db.clone());
            if let Err(TmdbError::InvalidKey) = probe.search_movie("Flixnet", None).await {
                return Err(ApiError::validation("Der TMDB-Schlüssel ist ungültig."));
            }
            current.tmdb_api_key = Some(key);
        }
        Some(None) => current.tmdb_api_key = None,
        None => {}
    }
    match body.fanart_api_key {
        Some(Some(key)) if !key.trim().is_empty() => {
            current.fanart_api_key = Some(key.trim().to_owned())
        }
        Some(_) => current.fanart_api_key = None,
        None => {}
    }
    if let Some(enabled) = body.musicbrainz_enabled {
        current.musicbrainz_enabled = Some(enabled);
    }
    settings::set(&state.db, providers::SETTINGS_KEY, &current).await?;
    Ok(Json(to_dto(&current)))
}
