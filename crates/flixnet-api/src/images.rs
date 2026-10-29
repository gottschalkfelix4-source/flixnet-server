//! `/api/v1/images/{id}`: cached, resized artwork.

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use flixnet_db::{items, libraries, metadata};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult, ErrorBody};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_image))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ImageQuery {
    /// Desired width in pixels (snapped up to 92, 185, 342, 500, 780, 1280 or 1920).
    pub w: Option<u32>,
    /// Expiry of the signature (unix seconds), from URLs returned by the API.
    pub exp: Option<i64>,
    /// URL signature, from URLs returned by the API.
    pub sig: Option<String>,
}

/// An artwork image, resized and cached.
///
/// Use the signed URLs returned by item endpoints; alternatively authenticate normally.
#[utoipa::path(
    get,
    path = "/images/{id}",
    operation_id = "getImage",
    tag = "libraries",
    security(()),
    params(("id" = i64, Path, description = "Image id"), ImageQuery),
    responses(
        (status = OK, description = "The image (JPEG, PNG for logos)", content_type = "image/jpeg"),
        (status = UNAUTHORIZED, description = "Neither a valid signature nor a valid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such image", body = ErrorBody),
    )
)]
async fn get_image(
    user: Option<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(query): Query<ImageQuery>,
) -> ApiResult<Response> {
    let signed = match (query.exp, query.sig.as_deref()) {
        (Some(exp), Some(sig)) => state.signer.verify("img", id, exp, sig),
        _ => false,
    };
    let image = metadata::image(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Bild nicht gefunden."))?;
    if !signed {
        let user = user.ok_or_else(ApiError::unauthorized)?;
        // Without a signature, the caller must be allowed to see the library.
        let item = items::get(&state.db, image.item_id)
            .await?
            .ok_or_else(|| ApiError::not_found("Bild nicht gefunden."))?;
        if !user.is_admin
            && !libraries::user_can_access(&state.db, user.user_id, item.library_id).await?
        {
            return Err(ApiError::not_found("Bild nicht gefunden."));
        }
    }

    serve_image(&state, &image, query.w).await
}

/// The image resized to (about) `width`, from the cache (access already checked).
pub async fn serve_image(
    state: &AppState,
    image: &metadata::Image,
    width: Option<u32>,
) -> ApiResult<Response> {
    let id = image.id;
    let variant = state.images.variant(image, width).await.map_err(|err| {
        tracing::warn!(image = id, %err, "image unavailable");
        ApiError::not_found("Bild nicht verfügbar.")
    })?;
    if let Some((width, height, hash)) = &variant.blurhash {
        metadata::set_image_details(&state.db, id, i64::from(*width), i64::from(*height), hash)
            .await?;
    }
    let bytes = tokio::fs::read(&variant.path)
        .await
        .map_err(ApiError::internal)?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, variant.content_type)
        .header(header::CACHE_CONTROL, "private, max-age=604800")
        .body(Body::from(bytes))
        .map_err(ApiError::internal)
}
