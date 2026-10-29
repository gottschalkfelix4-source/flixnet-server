//! `Items/{id}/Images/{type}[/{index}]`, served from Flixnet's image cache.
//!
//! Players load images without credentials, so the `tag` from `ImageTags` (a signature
//! of the item id) is the permission; without a valid tag a token is needed and the
//! item must be visible to its user.

use std::collections::HashMap;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use flixnet_api::browse::ensure_visible;
use flixnet_api::error::{ApiError, ApiResult};
use flixnet_api::images::serve_image;
use flixnet_db::{items, metadata};

use crate::JfState;
use crate::auth::JfUser;
use crate::convert::verify_image_tag;
use crate::ids::{self, Kind};
use crate::util::Params;

pub(crate) fn router() -> Router<JfState> {
    Router::new()
        .route("/items/{id}/images/{kind}", get(image))
        .route("/items/{id}/images/{kind}/{index}", get(image))
}

fn not_found() -> ApiError {
    ApiError::not_found("Bild nicht gefunden.")
}

/// Flixnet image kinds for a Jellyfin image type, in order of preference.
fn kinds_for(image_type: &str, item_kind: &str) -> &'static [&'static str] {
    match (image_type, item_kind) {
        ("primary", "episode") => &["thumb", "poster"],
        ("primary", _) => &["poster"],
        ("backdrop", _) => &["backdrop"],
        ("logo", _) => &["logo"],
        ("thumb", _) => &["thumb", "backdrop"],
        ("banner", _) => &["banner"],
        _ => &[],
    }
}

/// Target width: `maxWidth`/`fillWidth`/`width`, or derived from a height and the
/// image's aspect ratio.
fn target_width(params: &Params, image: &metadata::Image) -> Option<u32> {
    let width = ["maxwidth", "fillwidth", "width"]
        .iter()
        .find_map(|p| params.i64(p));
    let height = ["maxheight", "fillheight", "height"]
        .iter()
        .find_map(|p| params.i64(p));
    let width = match (width, height) {
        (Some(w), _) => w,
        (None, Some(h)) => {
            let ratio = match (image.width, image.height) {
                (Some(w), Some(h)) if w > 0 && h > 0 => w as f64 / h as f64,
                _ => match image.kind.as_str() {
                    "poster" => 2.0 / 3.0,
                    "logo" => 2.5,
                    _ => 16.0 / 9.0,
                },
            };
            (h as f64 * ratio).round() as i64
        }
        (None, None) => return None,
    };
    u32::try_from(width.clamp(1, 4096)).ok()
}

async fn image(
    user: Option<JfUser>,
    State(state): State<JfState>,
    Path(path): Path<HashMap<String, String>>,
    params: Params,
) -> ApiResult<Response> {
    let (kind, id) = path
        .get("id")
        .and_then(|id| ids::parse(id))
        .ok_or_else(not_found)?;
    let image_type = path.get("kind").cloned().unwrap_or_default();
    let index: usize = path
        .get("index")
        .and_then(|i| i.parse().ok())
        .or_else(|| {
            params
                .i64("imageindex")
                .and_then(|i| usize::try_from(i).ok())
        })
        .unwrap_or(0);
    let tag = params.get("tag");
    match kind {
        Kind::Item => item_image(&state, user, id, &image_type, index, tag, &params).await,
        Kind::Person if image_type == "primary" => person_image(&state, user, id, tag).await,
        _ => Err(not_found()),
    }
}

async fn item_image(
    state: &JfState,
    user: Option<JfUser>,
    item_id: i64,
    image_type: &str,
    index: usize,
    tag: Option<&str>,
    params: &Params,
) -> ApiResult<Response> {
    let db = &state.app.db;
    let item = items::get(db, item_id).await?.ok_or_else(not_found)?;
    let images = metadata::images(db, item_id).await?;
    let image = kinds_for(image_type, &item.kind)
        .iter()
        .find_map(|kind| {
            let mut of_kind = images.iter().filter(|i| i.kind == *kind);
            if image_type == "backdrop" {
                of_kind.nth(index)
            } else {
                of_kind.next()
            }
        })
        .ok_or_else(not_found)?;
    let tagged = tag.is_some_and(|t| verify_image_tag(state, item_id, t));
    if !tagged {
        let user = user.ok_or_else(ApiError::unauthorized)?;
        ensure_visible(&state.app, &user.user, item_id).await?;
    }
    let mut response = serve_image(&state.app, image, target_width(params, image)).await?;
    if tagged {
        // The URL carries a tag that changes with the image: cache it for long.
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
    }
    Ok(response)
}

/// People photos come from the metadata provider; redirect to a fitting size.
async fn person_image(
    state: &JfState,
    user: Option<JfUser>,
    person_id: i64,
    tag: Option<&str>,
) -> ApiResult<Response> {
    let tagged = tag.is_some_and(|t| state.app.signer.verify_tag("jfperson", person_id, t));
    if !tagged && user.is_none() {
        return Err(ApiError::unauthorized());
    }
    let person = metadata::person(&state.app.db, person_id)
        .await?
        .ok_or_else(not_found)?;
    let url = person
        .image_url
        .filter(|u| u.starts_with("https://"))
        .ok_or_else(not_found)?
        .replace("/t/p/original/", "/t/p/w342/");
    let location = HeaderValue::from_str(&url).map_err(|_| not_found())?;
    Ok((StatusCode::FOUND, [(header::LOCATION, location)]).into_response())
}
