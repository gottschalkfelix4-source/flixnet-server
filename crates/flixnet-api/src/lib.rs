//! HTTP API. Every route lives under `/api/v1`.

use std::sync::Arc;

use axum::Router;
use flixnet_core::Config;
use tower_http::trace::TraceLayer;

mod system;

/// Shared state handed to every handler.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

/// Builds the complete application router.
pub fn router(state: AppState) -> Router {
    let api_v1 = Router::new().nest("/system", system::router());

    Router::new()
        .nest(&format!("/api/{}", flixnet_core::API_VERSION), api_v1)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
