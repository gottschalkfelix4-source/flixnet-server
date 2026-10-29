//! Request authentication.
//!
//! Clients send their device token as `Authorization: Bearer <token>`. Where headers
//! cannot be set (HLS segment URLs, `<img>` tags, TV clients) the `api_key` query
//! parameter is accepted as well. Handlers opt in by taking [`AuthUser`] (or
//! [`AdminUser`]) as their first argument, so authentication runs before body parsing.

use axum::extract::{FromRequestParts, OptionalFromRequestParts};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use flixnet_db::sessions;

use crate::AppState;
use crate::error::ApiError;

/// Token use is recorded at most this often per token, to keep reads write-free.
const TOUCH_INTERVAL_MS: i64 = 60_000;

/// The authenticated caller.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: i64,
    pub name: String,
    pub is_admin: bool,
    /// Content age limit of this profile (`None` = unrestricted).
    pub max_age_rating: Option<i64>,
    pub token_id: i64,
    pub device_id: i64,
}

/// An authenticated caller with administrator rights (403 otherwise).
#[derive(Debug, Clone)]
pub struct AdminUser(pub AuthUser);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = token_from_parts(parts).ok_or_else(ApiError::unauthorized)?;
        authenticate_token(state, &token).await
    }
}

/// Resolves a device token to its user (401 for unknown or revoked tokens). Shared
/// with the Jellyfin compatibility layer, which reads tokens from other headers.
pub async fn authenticate_token(state: &AppState, token: &str) -> Result<AuthUser, ApiError> {
    let hash = flixnet_core::auth::hash_token(token);
    let session = sessions::find_by_token_hash(&state.db, &hash)
        .await?
        .ok_or_else(ApiError::unauthorized)?;

    if flixnet_db::now_ms() - session.last_used_at > TOUCH_INTERVAL_MS {
        let db = state.db.clone();
        let (token_id, device_id) = (session.token_id, session.device_id);
        tokio::spawn(async move {
            if let Err(err) = sessions::touch(&db, token_id, device_id).await {
                tracing::warn!(%err, "recording token use failed");
            }
        });
    }

    Ok(AuthUser {
        user_id: session.user_id,
        name: session.user_name,
        is_admin: session.is_admin,
        max_age_rating: session.max_age_rating,
        token_id: session.token_id,
        device_id: session.device_id,
    })
}

/// `Option<AuthUser>`: `None` when the request carries no valid token (for routes
/// that also accept a URL signature).
impl OptionalFromRequestParts<AppState> for AuthUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Option<Self>, Self::Rejection> {
        Ok(
            <AuthUser as FromRequestParts<AppState>>::from_request_parts(parts, state)
                .await
                .ok(),
        )
    }
}

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let user =
            <AuthUser as FromRequestParts<AppState>>::from_request_parts(parts, state).await?;
        if user.is_admin {
            Ok(AdminUser(user))
        } else {
            Err(ApiError::forbidden())
        }
    }
}

fn token_from_parts(parts: &Parts) -> Option<String> {
    let from_header = parts
        .headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        })
        .map(str::trim)
        .filter(|token| !token.is_empty());

    from_header.map(str::to_owned).or_else(|| {
        parts.uri.query().and_then(|query| {
            query
                .split('&')
                .filter_map(|pair| pair.split_once('='))
                .find(|(key, _)| *key == "api_key")
                .map(|(_, value)| value.to_owned())
                .filter(|token| !token.is_empty())
        })
    })
}
