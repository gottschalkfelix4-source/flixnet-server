//! Jellyfin authentication: clients identify themselves in
//! `Authorization: MediaBrowser Client="…", Device="…", DeviceId="…", Version="…", Token="…"`
//! (older ones in `X-Emby-Authorization`, scheme `Emby`). The token may also come as
//! `X-Emby-Token` / `X-MediaBrowser-Token` header or `api_key` / `ApiKey` query
//! parameter. Tokens are ordinary Flixnet device tokens.

use axum::extract::{FromRequestParts, OptionalFromRequestParts};
use axum::http::HeaderMap;
use axum::http::request::Parts;
use flixnet_api::auth::{AuthUser, authenticate_token};
use flixnet_api::error::ApiError;

use crate::JfState;
use crate::util::{Params, percent_decode_header};

/// What the client says about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientInfo {
    pub client: Option<String>,
    pub device: Option<String>,
    pub device_id: Option<String>,
    pub version: Option<String>,
    pub token: Option<String>,
}

/// Parses `MediaBrowser Client="Swiftfin", Device="iPhone", …` (values may be
/// percent-encoded, quotes are optional).
pub fn parse_authorization(value: &str) -> Option<ClientInfo> {
    let value = value.trim();
    let (scheme, rest) = value.split_once(' ').unwrap_or((value, ""));
    if !matches!(
        scheme.to_ascii_lowercase().as_str(),
        "mediabrowser" | "emby"
    ) {
        return None;
    }
    let mut info = ClientInfo::default();
    for part in rest.split(',') {
        let Some((key, raw)) = part.split_once('=') else {
            continue;
        };
        let raw = raw.trim();
        let raw = raw
            .strip_prefix('"')
            .and_then(|r| r.strip_suffix('"'))
            .unwrap_or(raw);
        let decoded = percent_decode_header(raw).trim().to_owned();
        let value = (!decoded.is_empty()).then_some(decoded);
        match key.trim().to_ascii_lowercase().as_str() {
            "client" => info.client = value,
            "device" => info.device = value,
            "deviceid" => info.device_id = value,
            "version" => info.version = value,
            "token" => info.token = value,
            _ => {}
        }
    }
    Some(info)
}

/// Client info from the authorization headers (empty when there are none).
pub fn client_info(headers: &HeaderMap) -> ClientInfo {
    ["authorization", "x-emby-authorization"]
        .iter()
        .filter_map(|name| headers.get(*name)?.to_str().ok())
        .find_map(parse_authorization)
        .unwrap_or_default()
}

/// The access token from any of the places Jellyfin clients put it.
pub fn token(parts: &Parts) -> Option<String> {
    let header = |name: &str| {
        parts
            .headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let bearer = header("authorization").and_then(|v| {
        v.strip_prefix("Bearer ")
            .or_else(|| v.strip_prefix("bearer "))
            .map(|t| t.trim().to_owned())
    });
    let query = Params::parse(parts.uri.query());
    client_info(&parts.headers)
        .token
        .or_else(|| header("x-emby-token"))
        .or_else(|| header("x-mediabrowser-token"))
        .or(bearer)
        .or_else(|| {
            ["api_key", "apikey", "x-emby-token"]
                .iter()
                .find_map(|name| query.get(name).map(str::to_owned))
        })
        .filter(|t| !t.is_empty() && t.len() <= 256)
}

/// A caller with a valid Flixnet device token.
#[derive(Debug, Clone)]
pub struct JfUser {
    pub user: AuthUser,
    pub client: ClientInfo,
}

impl FromRequestParts<JfState> for JfUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &JfState) -> Result<Self, ApiError> {
        let token = token(parts).ok_or_else(ApiError::unauthorized)?;
        let user = authenticate_token(&state.app, &token).await?;
        Ok(JfUser {
            user,
            client: client_info(&parts.headers),
        })
    }
}

impl OptionalFromRequestParts<JfState> for JfUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &JfState,
    ) -> Result<Option<Self>, Self::Rejection> {
        Ok(
            <JfUser as FromRequestParts<JfState>>::from_request_parts(parts, state)
                .await
                .ok(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_mediabrowser_header() {
        let info = parse_authorization(
            r#"MediaBrowser Client="Jellyfin Web", Device="Firefox%20on%20Windows", DeviceId="a+bc=", Version="10.9.0", Token="t0k""#,
        )
        .unwrap();
        assert_eq!(info.client.as_deref(), Some("Jellyfin Web"));
        assert_eq!(info.device.as_deref(), Some("Firefox on Windows"));
        assert_eq!(info.device_id.as_deref(), Some("a+bc="));
        assert_eq!(info.version.as_deref(), Some("10.9.0"));
        assert_eq!(info.token.as_deref(), Some("t0k"));

        let emby = parse_authorization(r#"Emby UserId="x", Client=Infuse, DeviceId=1"#).unwrap();
        assert_eq!(emby.client.as_deref(), Some("Infuse"));
        assert_eq!(emby.device_id.as_deref(), Some("1"));
        assert_eq!(emby.token, None);

        assert_eq!(parse_authorization("Bearer abc"), None);
        assert_eq!(
            parse_authorization(r#"MediaBrowser Token="""#)
                .unwrap()
                .token,
            None
        );
    }
}
