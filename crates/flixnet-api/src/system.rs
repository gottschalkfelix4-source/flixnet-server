//! `/api/v1/system/*` — public server information.

use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::AppState;

pub(crate) fn router() -> Router<AppState> {
    Router::new().route("/info", get(info))
}

/// Public, unauthenticated server information used by clients to discover the server.
#[derive(Debug, Serialize)]
pub struct SystemInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub api_version: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
}

async fn info() -> Json<SystemInfo> {
    Json(SystemInfo {
        name: flixnet_core::NAME,
        version: flixnet_core::VERSION,
        api_version: flixnet_core::API_VERSION,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
    })
}
