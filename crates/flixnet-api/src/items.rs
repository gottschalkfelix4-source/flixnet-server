//! `/api/v1/items/*`: metadata management of single items (browsing follows in 2.6).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use flixnet_core::jobs::priority;
use flixnet_db::items;
use flixnet_library::jobs::queue_refresh;
use flixnet_library::providers;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AdminUser;
use crate::error::{ApiError, ApiResult, ErrorBody};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(refresh_item))
        .routes(routes!(identify_candidates, identify_item))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct IdentifyQuery {
    /// Search text (defaults to the item's title).
    pub query: Option<String>,
    pub year: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CandidateDto {
    pub provider: String,
    pub id: i64,
    pub title: String,
    pub original_title: String,
    pub year: Option<i64>,
    pub overview: Option<String>,
    pub poster_url: Option<String>,
    /// 0–1: how well the candidate fits the item's title and year.
    pub score: f64,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct IdentifyRequest {
    pub tmdb_id: i64,
}

/// Re-read NFO/artwork and provider data for an item and its children.
#[utoipa::path(
    post,
    path = "/items/{id}/refresh",
    operation_id = "refreshItem",
    tag = "items",
    params(("id" = i64, Path, description = "Item id")),
    responses(
        (status = ACCEPTED, description = "Refresh queued"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such item", body = ErrorBody),
    )
)]
async fn refresh_item(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    items::get(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
    queue_refresh(state.jobs.queue(), id, priority::INTERACTIVE).await?;
    for child in items::descendant_ids(&state.db, id).await? {
        queue_refresh(state.jobs.queue(), child, priority::INTERACTIVE).await?;
    }
    Ok(StatusCode::ACCEPTED)
}

async fn tmdb(state: &AppState) -> ApiResult<providers::Tmdb> {
    providers::tmdb_from_settings(&state.db)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::conflict(
                "tmdb_not_configured",
                "Für die Online-Suche wird ein TMDB-Schlüssel benötigt (Einstellungen → Metadaten).",
            )
        })
}

/// Search TMDB for candidates matching a movie or series.
#[utoipa::path(
    get,
    path = "/items/{id}/identify",
    operation_id = "identifyCandidates",
    tag = "items",
    params(("id" = i64, Path, description = "Item id"), IdentifyQuery),
    responses(
        (status = OK, description = "Candidates, best first", body = Vec<CandidateDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such movie or series", body = ErrorBody),
        (status = CONFLICT, description = "No TMDB key configured", body = ErrorBody),
    )
)]
async fn identify_candidates(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(query): Query<IdentifyQuery>,
) -> ApiResult<Json<Vec<CandidateDto>>> {
    let item = identifiable(&state, id).await?;
    let tmdb = tmdb(&state).await?;
    let title = query
        .query
        .filter(|q| !q.trim().is_empty())
        .unwrap_or(item.title);
    let year = query.year.or(item.year);
    let found = if item.kind == "movie" {
        providers::movie_candidates(&tmdb, &title, year).await
    } else {
        providers::series_candidates(&tmdb, &title, year).await
    }
    .map_err(|err| ApiError::new(StatusCode::BAD_GATEWAY, "provider_error", err.to_string()))?;
    Ok(Json(
        found
            .into_iter()
            .take(20)
            .map(|c| CandidateDto {
                provider: c.provider.to_owned(),
                id: c.id,
                title: c.title,
                original_title: c.original_title,
                year: c.year,
                overview: c.overview,
                poster_url: c.poster_url,
                score: c.score.clamp(0.0, 1.0),
            })
            .collect(),
    ))
}

/// Pin a movie or series to a TMDB entry and load its metadata.
#[utoipa::path(
    post,
    path = "/items/{id}/identify",
    operation_id = "identifyItem",
    tag = "items",
    params(("id" = i64, Path, description = "Item id")),
    request_body = IdentifyRequest,
    responses(
        (status = NO_CONTENT, description = "Identified and updated"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such movie or series", body = ErrorBody),
        (status = CONFLICT, description = "No TMDB key configured", body = ErrorBody),
    )
)]
async fn identify_item(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<IdentifyRequest>,
) -> ApiResult<StatusCode> {
    identifiable(&state, id).await?;
    let tmdb = tmdb(&state).await?;
    providers::identify(&state.db, &tmdb, id, body.tmdb_id)
        .await
        .map_err(|err| ApiError::new(StatusCode::BAD_GATEWAY, "provider_error", err.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn identifiable(state: &AppState, id: i64) -> ApiResult<items::Item> {
    items::get(&state.db, id)
        .await?
        .filter(|i| i.kind == "movie" || i.kind == "series")
        .ok_or_else(|| ApiError::not_found("Film oder Serie nicht gefunden."))
}
