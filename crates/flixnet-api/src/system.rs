//! `/api/v1/system/*` — public server information.

use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(info))
}

/// Public server information used by clients to discover and identify the server.
#[derive(Debug, Serialize, ToSchema)]
pub struct SystemInfo {
    /// Product name, always `Flixnet`.
    #[schema(example = "Flixnet")]
    pub name: &'static str,
    /// Server version.
    #[schema(example = "0.1.0")]
    pub version: &'static str,
    /// API version that the routes are served under.
    #[schema(example = "v1")]
    pub api_version: &'static str,
    /// Operating system the server runs on.
    #[schema(example = "linux")]
    pub os: &'static str,
    /// CPU architecture.
    #[schema(example = "x86_64")]
    pub arch: &'static str,
}

/// Get server information.
///
/// Public endpoint; needs no authentication.
#[utoipa::path(
    get,
    path = "/info",
    operation_id = "getSystemInfo",
    tag = "system",
    security(()),
    responses((status = OK, description = "Server information", body = SystemInfo))
)]
async fn info() -> Json<SystemInfo> {
    Json(SystemInfo {
        name: flixnet_core::NAME,
        version: flixnet_core::VERSION,
        api_version: flixnet_core::API_VERSION,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
    })
}
