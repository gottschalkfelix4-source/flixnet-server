//! HTTP API. Every route lives under `/api/v1` and is documented in the OpenAPI spec,
//! which is served at `/api/v1/openapi.json` (interactive docs at `/api/v1/docs`).

use std::sync::Arc;

use axum::routing::get;
use axum::{Json, Router};
use flixnet_core::Config;
use flixnet_db::Db;
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_scalar::{Scalar, Servable};

mod system;

/// Path prefix of the current API version.
const API_PREFIX: &str = "/api/v1";

/// Shared state handed to every handler.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Db,
}

impl AppState {
    pub fn new(config: Config, db: Db) -> Self {
        Self {
            config: Arc::new(config),
            db,
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Flixnet API",
        description = "API of the Flixnet media server. Used by the web UI and future native apps."
    ),
    servers((url = "/", description = "The Flixnet server serving this document")),
    tags((name = "system", description = "Server information"))
)]
struct ApiDoc;

/// Documented API routes plus the spec generated from them.
fn api() -> (Router<AppState>, utoipa::openapi::OpenApi) {
    let mut doc = ApiDoc::openapi();
    // utoipa copies the Cargo `license` field; until the project picks one, omit it
    // instead of publishing an empty license object.
    doc.info.license = None;

    OpenApiRouter::with_openapi(doc)
        .nest(&format!("{API_PREFIX}/system"), system::router())
        .split_for_parts()
}

/// The OpenAPI document describing every route (used by `flixnet-server openapi`).
pub fn openapi() -> utoipa::openapi::OpenApi {
    api().1
}

/// Builds the complete application router.
pub fn router(state: AppState) -> Router {
    let (api, spec) = api();
    let spec_json = Arc::new(spec.clone());

    api.route(
        &format!("{API_PREFIX}/openapi.json"),
        get(move || async move { Json(spec_json.as_ref().clone()) }),
    )
    .merge(Scalar::with_url(format!("{API_PREFIX}/docs"), spec))
    .layer(TraceLayer::new_for_http())
    .with_state(state)
}
