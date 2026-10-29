//! Serves the React web UI embedded into the binary.
//!
//! Release builds embed `web/dist`; debug builds read it from disk, so `npm run build`
//! is picked up without recompiling. Unknown non-file paths fall back to `index.html`
//! so client-side routes (`/movies`, `/item/123`, …) survive a page reload.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;
use tower_http::compression::CompressionLayer;

#[derive(Embed)]
#[folder = "../../web/dist"]
#[allow_missing = true]
struct Assets;

/// Router that answers every request not handled by the API.
pub fn router() -> Router {
    Router::new()
        .fallback(handler)
        .layer(CompressionLayer::new())
}

async fn handler(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Unknown API routes must stay JSON-API 404s, never the HTML app.
    if path == "api" || path.starts_with("api/") {
        return StatusCode::NOT_FOUND.into_response();
    }

    if !path.is_empty()
        && let Some(response) = serve(path, &headers)
    {
        return response;
    }

    // Missing files with an extension (e.g. a stale /assets/old.js) are real 404s.
    if path
        .rsplit('/')
        .next()
        .is_some_and(|segment| segment.contains('.'))
    {
        return StatusCode::NOT_FOUND.into_response();
    }

    serve("index.html", &headers).unwrap_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Flixnet web UI is not built. Run `npm run build` in web/.",
        )
            .into_response()
    })
}

fn serve(path: &str, headers: &HeaderMap) -> Option<Response> {
    let asset = Assets::get(path)?;
    let etag = format!("\"{}\"", hex::encode(asset.metadata.sha256_hash()));

    // Vite fingerprints everything under assets/, so those never change.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };

    let not_modified = headers
        .get(header::IF_NONE_MATCH)
        .is_some_and(|value| value.as_bytes() == etag.as_bytes());

    let mut builder = Response::builder()
        .header(header::ETAG, &etag)
        .header(header::CACHE_CONTROL, HeaderValue::from_static(cache));

    if not_modified {
        return builder
            .status(StatusCode::NOT_MODIFIED)
            .body(Body::empty())
            .ok();
    }

    builder = builder.header(header::CONTENT_TYPE, asset.metadata.mimetype());
    builder.body(Body::from(asset.data.into_owned())).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn get(path: &str) -> Response {
        router()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn api_paths_are_never_answered_with_html() {
        assert_eq!(get("/api/v1/unknown").await.status(), StatusCode::NOT_FOUND);
        assert_eq!(get("/api").await.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn missing_files_with_extension_are_not_found() {
        assert_eq!(
            get("/assets/does-not-exist.js").await.status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn client_routes_fall_back_to_the_app() {
        let status = get("/movies").await.status();
        // 200 with a built UI, 503 with a hint when web/dist is missing.
        assert!(
            status == StatusCode::OK || status == StatusCode::SERVICE_UNAVAILABLE,
            "{status}"
        );
    }
}
