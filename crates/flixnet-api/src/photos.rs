//! `/api/v1/photos*`: the timeline, albums and single pictures of photo libraries.
//!
//! Like every listing, limited to the libraries the user may see and to their
//! parental controls. Pictures load through signed image URLs (thumbnails and the
//! large view from the image cache), clips through signed stream URLs.

use axum::Json;
use axum::extract::{Path, Query, State};
use flixnet_db::photos::{self, AlbumRow, PhotoQuery, PhotoRow};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AuthUser;
use crate::browse::visible_libraries;
use crate::error::{ApiError, ApiResult, ErrorBody};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_photos))
        .routes(routes!(list_photo_months))
        .routes(routes!(list_photo_albums))
        .routes(routes!(get_photo))
}

/// Thumbnail width on the timeline.
const THUMB_WIDTH: u32 = 500;
/// Width of the large view.
const LARGE_WIDTH: u32 = 1920;

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PhotosQuery {
    pub library_id: Option<i64>,
    /// Only the pictures of this album.
    pub album_id: Option<i64>,
    /// `asc`: oldest first (default: newest first).
    pub order: Option<String>,
    /// 1–500, default 200.
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CameraDto {
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens: Option<String>,
    pub f_number: Option<f64>,
    /// e.g. `1/250`.
    pub exposure: Option<String>,
    pub iso: Option<i64>,
    /// Millimetres.
    pub focal_length: Option<f64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LocationDto {
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PhotoDto {
    pub id: i64,
    pub library_id: i64,
    /// `photo` or `video` (a short clip).
    pub kind: String,
    /// The album (folder) it is in.
    pub album_id: Option<i64>,
    /// File name without extension.
    pub title: String,
    /// When it was taken (unix ms; the file time without EXIF).
    pub taken_at: i64,
    pub width: Option<i64>,
    pub height: Option<i64>,
    /// Camera and exposure from EXIF (`null` without any).
    pub camera: Option<CameraDto>,
    /// GPS position from EXIF.
    pub location: Option<LocationDto>,
    /// Signed thumbnail URL (500 px wide).
    pub thumb_url: Option<String>,
    /// Signed URL of the large view (1920 px wide).
    pub large_url: Option<String>,
    pub blurhash: Option<String>,
    /// Signed URL of the original file (clips play from it).
    pub original_url: Option<String>,
    /// Clips: duration in milliseconds.
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PhotoPage {
    pub items: Vec<PhotoDto>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PhotoMonthDto {
    /// `YYYY-MM`.
    pub month: String,
    pub count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PhotoAlbumDto {
    pub id: i64,
    pub library_id: i64,
    pub title: String,
    pub photo_count: i64,
    /// Signed URL of the newest picture.
    pub cover_url: Option<String>,
    pub cover_blurhash: Option<String>,
    /// Oldest and newest picture (unix ms).
    pub first_taken_at: Option<i64>,
    pub last_taken_at: Option<i64>,
}

fn to_dto(state: &AppState, user: &AuthUser, row: PhotoRow) -> PhotoDto {
    let camera = (row.camera_make.is_some()
        || row.camera_model.is_some()
        || row.lens.is_some()
        || row.f_number.is_some()
        || row.exposure.is_some()
        || row.iso.is_some())
    .then_some(CameraDto {
        make: row.camera_make,
        model: row.camera_model,
        lens: row.lens,
        f_number: row.f_number,
        exposure: row.exposure,
        iso: row.iso,
        focal_length: row.focal_length,
    });
    PhotoDto {
        thumb_url: row
            .image_id
            .map(|id| state.signer.image_url(id, Some(THUMB_WIDTH))),
        large_url: row
            .image_id
            .map(|id| state.signer.image_url(id, Some(LARGE_WIDTH))),
        blurhash: row.blurhash,
        original_url: row
            .file_id
            .map(|id| state.signer.stream_url(id, user.user_id)),
        location: row
            .latitude
            .zip(row.longitude)
            .map(|(latitude, longitude)| LocationDto {
                latitude,
                longitude,
            }),
        camera,
        id: row.id,
        library_id: row.library_id,
        duration_ms: (row.kind == "video").then_some(row.duration_ms).flatten(),
        kind: row.kind,
        album_id: row.parent_id,
        title: row.title,
        taken_at: row.taken_at,
        width: row.width,
        height: row.height,
    }
}

async fn photo_query(
    state: &AppState,
    user: &AuthUser,
    library_id: Option<i64>,
) -> ApiResult<PhotoQuery> {
    Ok(PhotoQuery {
        library_ids: visible_libraries(state, user, library_id).await?,
        restriction: user.restriction.clone(),
        ..Default::default()
    })
}

/// The photo timeline: pictures and clips, newest first.
#[utoipa::path(
    get,
    path = "/photos",
    operation_id = "listPhotos",
    tag = "photos",
    params(PhotosQuery),
    responses(
        (status = OK, description = "One page of the timeline", body = PhotoPage),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn list_photos(
    user: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<PhotosQuery>,
) -> ApiResult<Json<PhotoPage>> {
    let mut q = photo_query(&state, &user, query.library_id).await?;
    q.album_id = query.album_id;
    q.ascending = query.order.as_deref() == Some("asc");
    q.limit = query.limit.unwrap_or(200).clamp(1, 500);
    q.offset = query.offset.unwrap_or(0).max(0);
    let (rows, total) = photos::list(&state.db, &q).await?;
    Ok(Json(PhotoPage {
        items: rows.into_iter().map(|r| to_dto(&state, &user, r)).collect(),
        total,
        offset: q.offset,
        limit: q.limit,
    }))
}

/// Months of the timeline with their number of pictures (for the jump bar).
#[utoipa::path(
    get,
    path = "/photos/months",
    operation_id = "listPhotoMonths",
    tag = "photos",
    params(PhotosQuery),
    responses(
        (status = OK, description = "Months, newest first", body = Vec<PhotoMonthDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn list_photo_months(
    user: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<PhotosQuery>,
) -> ApiResult<Json<Vec<PhotoMonthDto>>> {
    let mut q = photo_query(&state, &user, query.library_id).await?;
    q.album_id = query.album_id;
    let months = photos::months(&state.db, &q).await?;
    Ok(Json(
        months
            .into_iter()
            .map(|(month, count)| PhotoMonthDto { month, count })
            .collect(),
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AlbumsQuery {
    pub library_id: Option<i64>,
}

/// Albums (folders) with visible pictures, most recent first.
#[utoipa::path(
    get,
    path = "/photos/albums",
    operation_id = "listPhotoAlbums",
    tag = "photos",
    params(AlbumsQuery),
    responses(
        (status = OK, description = "Albums", body = Vec<PhotoAlbumDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn list_photo_albums(
    user: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<AlbumsQuery>,
) -> ApiResult<Json<Vec<PhotoAlbumDto>>> {
    let library_ids = visible_libraries(&state, &user, query.library_id).await?;
    let albums = photos::albums(&state.db, &library_ids, &user.restriction).await?;
    Ok(Json(
        albums
            .into_iter()
            .map(|a: AlbumRow| PhotoAlbumDto {
                cover_url: a
                    .cover_image_id
                    .map(|id| state.signer.image_url(id, Some(THUMB_WIDTH))),
                cover_blurhash: a.cover_blurhash,
                id: a.id,
                library_id: a.library_id,
                title: a.title,
                photo_count: a.photo_count,
                first_taken_at: a.first_taken_at,
                last_taken_at: a.last_taken_at,
            })
            .collect(),
    ))
}

/// One picture or clip with its EXIF details.
#[utoipa::path(
    get,
    path = "/photos/{id}",
    operation_id = "getPhoto",
    tag = "photos",
    params(("id" = i64, Path, description = "Item id of the picture or clip")),
    responses(
        (status = OK, description = "The picture", body = PhotoDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) picture", body = ErrorBody),
    )
)]
async fn get_photo(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<PhotoDto>> {
    let q = photo_query(&state, &user, None).await?;
    let row = photos::get(&state.db, id, &q)
        .await?
        .ok_or_else(|| ApiError::not_found("Foto nicht gefunden."))?;
    Ok(Json(to_dto(&state, &user, row)))
}
