//! HTTP API. Every route lives under `/api/v1` and is documented in the OpenAPI spec,
//! which is served at `/api/v1/openapi.json` (interactive docs at `/api/v1/docs`).

use std::sync::Arc;

use axum::routing::get;
use axum::{Json, Router};
use flixnet_core::Config;
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobsHandle;
use flixnet_core::scheduler::TaskRegistry;
use flixnet_db::Db;
use flixnet_library::images::ImageCache;
use flixnet_library::watch::LibraryWatcher;
use flixnet_media::Ffprobe;
use flixnet_media::transcode::TranscodeManager;
use tower_http::trace::TraceLayer;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi};
use utoipa_axum::router::OpenApiRouter;
use utoipa_scalar::{Scalar, Servable};

pub mod auth;
mod browse;
pub mod error;
mod images;
mod invitations;
mod items;
mod jobs;
mod libraries;
pub mod limiter;
mod playback;
mod session;
mod settings;
pub mod signing;
mod system;
mod transcode;
mod users;
mod validate;

/// Path prefix of the current API version.
const API_PREFIX: &str = "/api/v1";

/// Shared state handed to every handler.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Db,
    pub limits: Arc<limiter::Limits>,
    pub events: EventBus,
    pub jobs: JobsHandle,
    pub tasks: TaskRegistry,
    pub images: ImageCache,
    pub signer: Arc<signing::UrlSigner>,
    pub watcher: Option<LibraryWatcher>,
    pub transcoder: TranscodeManager,
    pub ffprobe: Ffprobe,
}

/// Long-lived services the API talks to.
pub struct Services {
    pub events: EventBus,
    pub jobs: JobsHandle,
    pub tasks: TaskRegistry,
    pub images: ImageCache,
    /// Secret for signed media URLs (persisted, so URLs survive restarts).
    pub url_secret: Vec<u8>,
    /// File watcher, re-synced when libraries change (`None` = disabled).
    pub watcher: Option<LibraryWatcher>,
    pub transcoder: TranscodeManager,
    pub ffprobe: Ffprobe,
}

impl AppState {
    pub fn new(config: Config, db: Db, services: Services) -> Self {
        Self {
            config: Arc::new(config),
            db,
            limits: Arc::default(),
            events: services.events,
            jobs: services.jobs,
            tasks: services.tasks,
            images: services.images,
            signer: Arc::new(signing::UrlSigner::new(services.url_secret)),
            watcher: services.watcher,
            transcoder: services.transcoder,
            ffprobe: services.ffprobe,
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
    // Every operation requires a device token unless it declares `security(())`.
    security(("bearer" = []), ("api_key" = [])),
    modifiers(&SecuritySchemes),
    tags(
        (name = "system", description = "Server information"),
        (name = "auth", description = "First-run setup, sign-in, sign-out and profile switching"),
        (name = "users", description = "User accounts and household profiles"),
        (name = "invitations", description = "Invitation links for new accounts"),
        (name = "jobs", description = "Background jobs and scheduled tasks (administrators)"),
        (name = "libraries", description = "Media libraries, folder browsing and images"),
        (name = "items", description = "Movies, series, seasons and episodes"),
        (name = "playback", description = "Playback sources and media streams"),
        (name = "settings", description = "Server settings (administrators)"),
    )
)]
struct ApiDoc;

struct SecuritySchemes;

impl Modify for SecuritySchemes {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
        );
        components.add_security_scheme(
            "api_key",
            SecurityScheme::ApiKey(ApiKey::Query(ApiKeyValue::with_description(
                "api_key",
                "Device token as query parameter, for URLs that cannot carry headers (media, images).",
            ))),
        );
    }
}

/// Documented API routes plus the spec generated from them.
fn api() -> (Router<AppState>, utoipa::openapi::OpenApi) {
    let mut doc = ApiDoc::openapi();
    // utoipa copies the Cargo `license` field; until the project picks one, omit it
    // instead of publishing an empty license object.
    doc.info.license = None;

    OpenApiRouter::with_openapi(doc)
        .nest(&format!("{API_PREFIX}/system"), system::router())
        .nest(&format!("{API_PREFIX}/users"), users::router())
        .nest(&format!("{API_PREFIX}/invitations"), invitations::router())
        .nest(API_PREFIX, session::router())
        .nest(API_PREFIX, jobs::router())
        .nest(API_PREFIX, libraries::router())
        .nest(API_PREFIX, images::router())
        .nest(API_PREFIX, items::router())
        .nest(API_PREFIX, browse::router())
        .nest(API_PREFIX, playback::router())
        .nest(API_PREFIX, transcode::router())
        .nest(API_PREFIX, settings::router())
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
