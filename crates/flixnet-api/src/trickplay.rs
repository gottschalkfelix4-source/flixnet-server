//! Trickplay (seek-bar previews) and chapters of a media file.
//!
//! The manifest and the chapter list need a token; the images they point to are
//! loaded by `<img>`/CSS and come with signed URLs bound to user and file, like
//! subtitles. Images are made by background jobs (`flixnet_library::trickplay`).

use std::path::PathBuf;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use flixnet_db::{media, trickplay};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::playback::accessible_file;

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_trickplay))
        .routes(routes!(get_trickplay_sheet))
        .routes(routes!(list_chapters))
        .routes(routes!(get_chapter_image))
}

/// How the sheets are laid out: thumbnail `n = floor(position_ms / interval_ms)` is on
/// sheet `n / (columns × rows)`, row-major within it.
#[derive(Debug, Serialize, ToSchema)]
pub struct TrickplayDto {
    pub interval_ms: i64,
    /// Size of one thumbnail in pixels.
    pub tile_width: i64,
    pub tile_height: i64,
    /// Thumbnails per sheet row.
    pub columns: i64,
    /// Rows per sheet.
    pub rows: i64,
    /// Thumbnails in total (the last sheet may be partly filled).
    pub thumbnails: i64,
    /// Signed URLs of the sheets (JPEG), in order.
    pub sheets: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ChapterDto {
    /// Chapter number within the file, from 0.
    pub index: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub title: Option<String>,
    /// Signed URL of the chapter's image (JPEG), once made.
    pub image_url: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ImageQuery {
    /// User the URL was issued to, from URLs returned by the API.
    pub u: Option<i64>,
    /// Expiry of the signature (unix seconds).
    pub exp: Option<i64>,
    /// URL signature.
    pub sig: Option<String>,
    /// Version of the image (ignored; new images get new URLs).
    #[allow(dead_code)] // documents the cache-busting parameter
    pub v: Option<i64>,
}

fn cache_dir(state: &AppState) -> PathBuf {
    state.config.paths.config_dir.join("cache")
}

fn signed_url(
    state: &AppState,
    scope: &str,
    file_id: i64,
    user_id: i64,
    path: &str,
    version: i64,
) -> String {
    let (expires, sig) = state.signer.sign(&format!("{scope}:{user_id}"), file_id);
    format!("/api/v1/files/{file_id}/{path}?u={user_id}&exp={expires}&sig={sig}&v={version}")
}

/// The user a valid signature was issued to.
fn signed_user(state: &AppState, scope: &str, file_id: i64, query: &ImageQuery) -> Option<i64> {
    match (query.u, query.exp, query.sig.as_deref()) {
        (Some(u), Some(exp), Some(sig))
            if state
                .signer
                .verify(&format!("{scope}:{u}"), file_id, exp, sig) =>
        {
            Some(u)
        }
        _ => None,
    }
}

/// `3.jpg` → 3.
fn jpeg_index(name: &str) -> Option<i64> {
    name.strip_suffix(".jpg")?.parse().ok().filter(|n| *n >= 0)
}

async fn jpeg(path: PathBuf, missing: &'static str) -> ApiResult<Response> {
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| ApiError::not_found(missing))?;
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg")),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=604800"),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// Trickplay manifest of a media file: tile layout and signed sheet URLs.
#[utoipa::path(
    get,
    path = "/files/{file_id}/trickplay",
    operation_id = "getTrickplay",
    tag = "playback",
    params(("file_id" = i64, Path, description = "Media file id")),
    responses(
        (status = OK, description = "Layout and sheets", body = TrickplayDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such file, no access, or no thumbnails yet", body = ErrorBody),
    )
)]
async fn get_trickplay(
    user: AuthUser,
    State(state): State<AppState>,
    Path(file_id): Path<i64>,
) -> ApiResult<Json<TrickplayDto>> {
    accessible_file(&state, Some(&user), None, file_id).await?;
    let sheets = trickplay::get(&state.db, file_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Für diese Datei gibt es noch keine Vorschaubilder."))?;
    Ok(Json(TrickplayDto {
        interval_ms: sheets.interval_ms,
        tile_width: sheets.tile_width,
        tile_height: sheets.tile_height,
        columns: sheets.tile_columns,
        rows: sheets.tile_rows,
        thumbnails: sheets.thumbnails,
        sheets: (0..sheets.sheets)
            .map(|n| {
                signed_url(
                    &state,
                    "trickplay",
                    file_id,
                    user.user_id,
                    &format!("trickplay/{n}.jpg"),
                    sheets.created_at,
                )
            })
            .collect(),
    }))
}

/// One trickplay sheet (`{n}.jpg`).
///
/// Use the signed URLs from the trickplay manifest; alternatively authenticate normally.
#[utoipa::path(
    get,
    path = "/files/{file_id}/trickplay/{sheet}",
    operation_id = "getTrickplaySheet",
    tag = "playback",
    security(()),
    params(
        ("file_id" = i64, Path, description = "Media file id"),
        ("sheet" = String, Path, description = "Sheet number plus `.jpg`, e.g. `0.jpg`"),
        ImageQuery,
    ),
    responses(
        (status = OK, description = "The sheet", content_type = "image/jpeg"),
        (status = UNAUTHORIZED, description = "Neither a valid signature nor a valid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such file or sheet, or no access", body = ErrorBody),
    )
)]
async fn get_trickplay_sheet(
    user: Option<AuthUser>,
    State(state): State<AppState>,
    Path((file_id, sheet)): Path<(i64, String)>,
    Query(query): Query<ImageQuery>,
) -> ApiResult<Response> {
    const MISSING: &str = "Vorschaubild nicht gefunden.";
    let signed = signed_user(&state, "trickplay", file_id, &query);
    accessible_file(&state, user.as_ref(), signed, file_id).await?;
    let sheet = jpeg_index(&sheet).ok_or_else(|| ApiError::not_found(MISSING))?;
    let known = trickplay::get(&state.db, file_id)
        .await?
        .is_some_and(|t| sheet < t.sheets);
    if !known {
        return Err(ApiError::not_found(MISSING));
    }
    // The path is built from numbers, never from the request string.
    jpeg(
        flixnet_media::trickplay::sheet_path(&cache_dir(&state), file_id, sheet),
        MISSING,
    )
    .await
}

/// Chapters of a media file, with signed URLs of their images.
#[utoipa::path(
    get,
    path = "/files/{file_id}/chapters",
    operation_id = "listChapters",
    tag = "playback",
    params(("file_id" = i64, Path, description = "Media file id")),
    responses(
        (status = OK, description = "The chapters in order (empty without chapters)", body = Vec<ChapterDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such file, or no access", body = ErrorBody),
    )
)]
async fn list_chapters(
    user: AuthUser,
    State(state): State<AppState>,
    Path(file_id): Path<i64>,
) -> ApiResult<Json<Vec<ChapterDto>>> {
    accessible_file(&state, Some(&user), None, file_id).await?;
    let images = trickplay::chapters(&state.db, file_id).await?;
    let chapters = media::chapters(&state.db, file_id)
        .await?
        .into_iter()
        .map(|c| {
            let made = images
                .iter()
                .find(|i| i.idx == c.idx)
                .and_then(|i| i.image_at);
            ChapterDto {
                index: c.idx,
                start_ms: c.start_ms,
                end_ms: c.end_ms,
                title: c.title,
                image_url: made.map(|at| {
                    signed_url(
                        &state,
                        "chapter",
                        file_id,
                        user.user_id,
                        &format!("chapters/{}.jpg", c.idx),
                        at,
                    )
                }),
            }
        })
        .collect();
    Ok(Json(chapters))
}

/// The image of one chapter (`{index}.jpg`).
///
/// Use the signed URLs from the chapter list; alternatively authenticate normally.
#[utoipa::path(
    get,
    path = "/files/{file_id}/chapters/{chapter}",
    operation_id = "getChapterImage",
    tag = "playback",
    security(()),
    params(
        ("file_id" = i64, Path, description = "Media file id"),
        ("chapter" = String, Path, description = "Chapter number plus `.jpg`, e.g. `2.jpg`"),
        ImageQuery,
    ),
    responses(
        (status = OK, description = "The image", content_type = "image/jpeg"),
        (status = UNAUTHORIZED, description = "Neither a valid signature nor a valid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such file or chapter image, or no access", body = ErrorBody),
    )
)]
async fn get_chapter_image(
    user: Option<AuthUser>,
    State(state): State<AppState>,
    Path((file_id, chapter)): Path<(i64, String)>,
    Query(query): Query<ImageQuery>,
) -> ApiResult<Response> {
    const MISSING: &str = "Kapitelbild nicht gefunden.";
    let signed = signed_user(&state, "chapter", file_id, &query);
    accessible_file(&state, user.as_ref(), signed, file_id).await?;
    let chapter = jpeg_index(&chapter).ok_or_else(|| ApiError::not_found(MISSING))?;
    let made = trickplay::chapters(&state.db, file_id)
        .await?
        .iter()
        .any(|c| c.idx == chapter && c.image_at.is_some());
    if !made {
        return Err(ApiError::not_found(MISSING));
    }
    jpeg(
        flixnet_media::trickplay::chapter_image_path(&cache_dir(&state), file_id, chapter),
        MISSING,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::jpeg_index;

    #[test]
    fn only_numbered_jpegs() {
        assert_eq!(jpeg_index("0.jpg"), Some(0));
        assert_eq!(jpeg_index("12.jpg"), Some(12));
        for bad in ["-1.jpg", "1.png", "../1.jpg", "a.jpg", ".jpg", "1"] {
            assert_eq!(jpeg_index(bad), None, "{bad}");
        }
    }
}
