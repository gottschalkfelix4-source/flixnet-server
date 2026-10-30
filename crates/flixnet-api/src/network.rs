//! `/api/v1/settings/network` (administrators): the address devices on the network
//! reach the server at, and the DLNA server. `/api/v1/cast/config` hands the address
//! to players that cast (a Chromecast cannot load `localhost` URLs).

use axum::Json;
use axum::extract::State;
use flixnet_db::{settings, users};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::{AdminUser, AuthUser};
use crate::dlna::{self, DlnaSettings, DlnaStatus};
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::validate;

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_network_settings, update_network_settings))
        .routes(routes!(get_cast_config))
}

const NETWORK_KEY: &str = "network";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct NetworkSettings {
    public_url: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NetworkSettingsDto {
    /// Public or LAN address of the server (e.g. `http://192.168.1.10:8097`), used for
    /// absolute URLs (Chromecast); `null` = the address the browser uses.
    pub public_url: Option<String>,
    /// User whose libraries and parental controls DLNA uses; `null` = DLNA off.
    pub dlna_user_id: Option<i64>,
    /// Name DLNA clients show.
    pub dlna_name: String,
    pub dlna_port: u16,
    pub dlna_status: DlnaStatus,
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct UpdateNetworkSettings {
    /// `null` removes the address.
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub public_url: Option<Option<String>>,
    /// `null` switches DLNA off.
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<i64>, nullable)]
    pub dlna_user_id: Option<Option<i64>>,
    pub dlna_name: Option<String>,
    /// 1024–65535 (0: any free port).
    pub dlna_port: Option<u16>,
}

async fn load(state: &AppState) -> ApiResult<(NetworkSettings, DlnaSettings)> {
    let network = settings::get(&state.db, NETWORK_KEY)
        .await?
        .unwrap_or_default();
    let dlna = dlna::load_settings(&state.db).await?;
    Ok((network, dlna))
}

fn to_dto(network: NetworkSettings, dlna: &DlnaSettings, status: DlnaStatus) -> NetworkSettingsDto {
    NetworkSettingsDto {
        public_url: network.public_url,
        dlna_user_id: dlna.user_id,
        dlna_name: dlna.name(),
        dlna_port: dlna.port(),
        dlna_status: status,
    }
}

/// `http(s)://host[:port][/prefix]` without a trailing slash.
fn normalize_url(url: &str) -> ApiResult<String> {
    let url = url.trim().trim_end_matches('/');
    let invalid = || ApiError::validation("Adresse im Format „http://192.168.1.10:8097“ angeben.");
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(invalid)?;
    let host = rest.split('/').next().unwrap_or_default();
    let valid_host = !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'));
    if !valid_host || url.len() > 200 || url.contains(['?', '#', ' ', '"', '<', '>', '\\']) {
        return Err(invalid());
    }
    Ok(url.to_owned())
}

/// Network address and DLNA.
#[utoipa::path(
    get,
    path = "/settings/network",
    operation_id = "getNetworkSettings",
    tag = "settings",
    responses(
        (status = OK, description = "Current settings", body = NetworkSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn get_network_settings(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<NetworkSettingsDto>> {
    let (network, dlna) = load(&state).await?;
    Ok(Json(to_dto(network, &dlna, state.dlna.status())))
}

/// Change the network address or the DLNA server (starts, restarts or stops it).
#[utoipa::path(
    put,
    path = "/settings/network",
    operation_id = "updateNetworkSettings",
    tag = "settings",
    request_body = UpdateNetworkSettings,
    responses(
        (status = OK, description = "Updated settings with the DLNA status", body = NetworkSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid address, user, name or port", body = ErrorBody),
    )
)]
async fn update_network_settings(
    _: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<UpdateNetworkSettings>,
) -> ApiResult<Json<NetworkSettingsDto>> {
    let (mut network, mut dlna) = load(&state).await?;
    match body.public_url {
        Some(Some(url)) if !url.trim().is_empty() => {
            network.public_url = Some(normalize_url(&url)?)
        }
        Some(_) => network.public_url = None,
        None => {}
    }
    match body.dlna_user_id {
        Some(Some(user_id)) => {
            if users::get(&state.db, user_id).await?.is_none() {
                return Err(ApiError::validation("Diesen Benutzer gibt es nicht."));
            }
            dlna.user_id = Some(user_id);
        }
        Some(None) => dlna.user_id = None,
        None => {}
    }
    if let Some(name) = body.dlna_name {
        let name = name.trim();
        if name.chars().count() > 60 {
            return Err(ApiError::validation(
                "Der Name ist zu lang (höchstens 60 Zeichen).",
            ));
        }
        dlna.friendly_name = (!name.is_empty()).then(|| name.to_owned());
    }
    if let Some(port) = body.dlna_port {
        if port != 0 && port < 1024 {
            return Err(ApiError::validation(
                "Port zwischen 1024 und 65535 angeben.",
            ));
        }
        dlna.port = Some(port);
    }
    settings::set(&state.db, NETWORK_KEY, &network).await?;
    settings::set(&state.db, dlna::SETTINGS_KEY, &dlna).await?;
    let status = state.dlna.apply(&state).await;
    let dlna = dlna::load_settings(&state.db).await?;
    Ok(Json(to_dto(network, &dlna, status)))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CastConfigDto {
    /// Address a Chromecast reaches the server at (`null`: not configured, use the
    /// browser's address).
    pub base_url: Option<String>,
}

/// Where cast devices load media from.
#[utoipa::path(
    get,
    path = "/cast/config",
    operation_id = "getCastConfig",
    tag = "playback",
    responses(
        (status = OK, description = "Cast settings", body = CastConfigDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn get_cast_config(
    _: AuthUser,
    State(state): State<AppState>,
) -> ApiResult<Json<CastConfigDto>> {
    let (network, _) = load(&state).await?;
    Ok(Json(CastConfigDto {
        base_url: network.public_url,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_normalized_and_checked() {
        assert_eq!(
            normalize_url(" http://192.168.188.129:8097/ ").unwrap(),
            "http://192.168.188.129:8097"
        );
        assert_eq!(
            normalize_url("https://flix.example.org/flixnet").unwrap(),
            "https://flix.example.org/flixnet"
        );
        assert_eq!(
            normalize_url("http://[fe80::1]:8097").unwrap(),
            "http://[fe80::1]:8097"
        );
        for bad in [
            "192.168.1.2",
            "ftp://x",
            "http://",
            "http://a b",
            "http://x/?q",
            "javascript:alert(1)",
        ] {
            assert!(normalize_url(bad).is_err(), "{bad}");
        }
    }
}
