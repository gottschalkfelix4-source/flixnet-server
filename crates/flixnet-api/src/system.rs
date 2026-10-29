//! `/api/v1/system/*` — public server information and diagnostics (administrators).

use axum::Json;
use axum::extract::State;
use flixnet_media::hardware::TranscodingInfo;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AdminUser;
use crate::error::ErrorBody;

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(info))
        .routes(routes!(get_transcoding_info))
}

/// Hardware transcoding: what was detected at startup and what transcodes use.
#[utoipa::path(
    get,
    path = "/transcoding",
    operation_id = "getTranscodingInfo",
    tag = "system",
    responses(
        (status = OK, description = "Encoders, tone mapping and ffmpeg version", body = TranscodingInfo),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn get_transcoding_info(
    _: AdminUser,
    State(state): State<AppState>,
) -> Json<TranscodingInfo> {
    Json(state.transcoder.transcoding_info())
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
