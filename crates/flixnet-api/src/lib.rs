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
use flixnet_media::subtitles::SubtitleCache;
use flixnet_media::transcode::TranscodeManager;
use tower_http::trace::TraceLayer;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi};
use utoipa_axum::router::OpenApiRouter;
use utoipa_scalar::{Scalar, Servable};

pub mod active_sessions;
mod ai;
mod ai_settings;
mod assistant;
mod audiobooks;
pub mod auth;
pub mod browse;
mod collections;
pub mod dlna;
pub mod error;
pub mod images;
mod invitations;
mod items;
mod jobs;
mod libraries;
pub mod limiter;
mod livetv;
mod music;
mod network;
pub mod ops;
mod photos;
pub mod playback;
mod playlists;
pub mod playstate;
mod scenes;
mod segments;
mod semantic_bridge;
pub mod session;
mod settings;
pub mod signing;
mod subtitle_search;
pub mod syncplay;
mod system;
pub mod tracks;
mod transcode;
mod trickplay;
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
    pub subtitles: SubtitleCache,
    /// Active playback sessions (dashboard, resume bookkeeping).
    pub playback: active_sessions::ActiveSessions,
    /// Live TV streams (shared per channel).
    pub livetv: flixnet_livetv::LiveTvManager,
    /// Notifications, Trakt, backups, import, dashboard, single sign-on.
    pub ops: ops::Ops,
    /// Local embeddings for semantic search (flixnet-ai), if available; without it
    /// natural-language and scene search use FTS5 only.
    pub semantic: Option<Arc<dyn flixnet_llm::semantic::SemanticIndex>>,
    /// "Gemeinsam schauen" groups.
    pub syncplay: syncplay::SyncPlay,
    /// The DLNA server (started by [`dlna::DlnaService::apply`]).
    pub dlna: dlna::DlnaService,
    /// Embeddings, semantic search and recommendations.
    pub ai: flixnet_ai::Ai,
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
    /// WebVTT conversions of text subtitles (`<config_dir>/cache/subtitles`).
    pub subtitles: SubtitleCache,
    /// Live TV streams (`<transcode_dir>/live`).
    pub livetv: flixnet_livetv::LiveTvManager,
}

impl AppState {
    pub fn new(config: Config, db: Db, services: Services) -> Self {
        let playback = active_sessions::ActiveSessions::default();
        playback.spawn_reaper(db.clone(), services.events.clone());
        let syncplay = syncplay::SyncPlay::default();
        syncplay.spawn_ticker();
        let ai = flixnet_ai::Ai::new(db.clone(), &config);
        ai.set_busy_probe(playback_busy_probe(&services.transcoder, &services.livetv));
        let state = Self {
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
            subtitles: services.subtitles,
            livetv: services.livetv,
            playback,
            ops: ops::Ops::default(),
            // Local embeddings back natural-language search whenever they exist.
            semantic: Some(Arc::new(semantic_bridge::AiSemantic(ai.clone()))),
            syncplay,
            dlna: dlna::DlnaService::default(),
            ai,
        };
        ops::spawn_listeners(&state);
        state
    }

    /// Plugs in a semantic index (local embeddings).
    pub fn with_semantic(mut self, index: Arc<dyn flixnet_llm::semantic::SemanticIndex>) -> Self {
        self.semantic = Some(index);
        self
    }

    /// Uses `ai` (shared with the job handlers) instead of the default instance.
    pub fn with_ai(mut self, ai: flixnet_ai::Ai) -> Self {
        self.ai = ai;
        self
    }
}

/// "Someone is transcoding": AI jobs wait while a playback transcode or a live TV
/// stream runs, so they never take CPU away from playback.
pub fn playback_busy_probe(
    transcoder: &TranscodeManager,
    livetv: &flixnet_livetv::LiveTvManager,
) -> flixnet_ai::BusyProbe {
    let (transcoder, livetv) = (transcoder.clone(), livetv.clone());
    std::sync::Arc::new(move || {
        let (transcoder, livetv) = (transcoder.clone(), livetv.clone());
        Box::pin(async move {
            !livetv.streams().is_empty()
                || transcoder.sessions().await.iter().any(|s| s.transcoding)
        })
    })
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
        (name = "collections", description = "Movie collections (automatic from TMDB and manual)"),
        (name = "playlists", description = "Personal and smart playlists"),
        (name = "settings", description = "Server settings (administrators)"),
        (name = "livetv", description = "Live TV: tuners, channels, guide and live playback"),
        (name = "notifications", description = "Webhooks, ntfy, Discord and e-mail notifications (administrators)"),
        (name = "trakt", description = "Trakt.tv connection, scrobbling and history sync"),
        (name = "backups", description = "Database backups (administrators)"),
        (name = "import", description = "Watch history import from Plex and Jellyfin (administrators)"),
        (name = "dashboard", description = "Server overview and logs (administrators)"),
        (name = "assistant", description = "Scene search and the optional LLM assistant: natural-language search, explanations, chat, recaps"),
        (name = "music", description = "Music: albums, artists, tracks, lyrics and audio playback"),
        (name = "syncplay", description = "Watch together: synchronized playback groups"),
        (name = "photos", description = "Photo libraries: timeline, albums and pictures"),
        (name = "audiobooks", description = "Audiobook libraries: books, chapters and listening progress"),
        (name = "recommendations", description = "Personal recommendations, onboarding and group mode"),
    ),
    components(schemas(syncplay::SyncPlayClientMessage))
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
        .nest(API_PREFIX, collections::router())
        .nest(API_PREFIX, playlists::router())
        .nest(API_PREFIX, playback::router())
        .nest(API_PREFIX, playstate::router())
        .nest(API_PREFIX, transcode::router())
        .nest(API_PREFIX, tracks::router())
        .nest(API_PREFIX, trickplay::router())
        .nest(API_PREFIX, settings::router())
        .nest(API_PREFIX, livetv::router())
        .nest(API_PREFIX, music::router())
        .nest(API_PREFIX, segments::router())
        .nest(API_PREFIX, subtitle_search::router())
        .nest(API_PREFIX, ops::router())
        .nest(API_PREFIX, assistant::router())
        .nest(API_PREFIX, scenes::router())
        .nest(API_PREFIX, ai_settings::router())
        .nest(API_PREFIX, syncplay::router())
        .nest(API_PREFIX, photos::router())
        .nest(API_PREFIX, audiobooks::router())
        .nest(API_PREFIX, network::router())
        .nest(API_PREFIX, ai::router())
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
    .layer(axum::middleware::from_fn_with_state(
        state.clone(),
        ops::bandwidth::track,
    ))
    .layer(TraceLayer::new_for_http())
    .with_state(state)
}
