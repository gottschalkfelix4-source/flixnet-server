//! Trakt.tv: per-user connection (OAuth device code flow), scrobbling from playback
//! events and a two-way sync of watched history and ratings.
//!
//! The administrator registers a Trakt API app once (client id + secret); every user
//! then connects their own Trakt account under Einstellungen → Trakt.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use flixnet_core::events::{PlaybackEvent, ServerEvent};
use flixnet_core::jobs::{JobContext, JobHandler, Pool};
use flixnet_core::scheduler::TaskDef;
use flixnet_db::history::{self, ImportedState};
use flixnet_db::ops::{self as db_ops, TraktAccount};
use flixnet_db::{Db, items, settings};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::matching::{ExternalIds, LibraryIndex};
use super::time::{format_rfc3339, parse_rfc3339};
use crate::AppState;
use crate::auth::{AdminUser, AuthUser};
use crate::error::{ApiError, ApiResult, ErrorBody};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(
            get_trakt_status,
            update_trakt_options,
            disconnect_trakt
        ))
        .routes(routes!(start_trakt_connect))
        .routes(routes!(poll_trakt_connect))
        .routes(routes!(sync_trakt))
        .routes(routes!(get_trakt_settings, update_trakt_settings))
}

const SETTINGS_KEY: &str = "trakt";
/// Refresh tokens this long before they expire.
const REFRESH_MARGIN_MS: i64 = 24 * 60 * 60 * 1000;
const OOB: &str = "urn:ietf:wg:oauth:2.0:oob";

/// The Trakt API app (`settings` key `trakt`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TraktApp {
    client_id: Option<String>,
    client_secret: Option<String>,
}

impl TraktApp {
    async fn load(db: &Db) -> flixnet_db::Result<Self> {
        Ok(settings::get(db, SETTINGS_KEY).await?.unwrap_or_default())
    }

    fn credentials(&self) -> Option<(&str, &str)> {
        Some((self.client_id.as_deref()?, self.client_secret.as_deref()?))
    }
}

/// A started device code flow, waiting for the user to confirm on trakt.tv.
#[derive(Debug, Clone)]
struct PendingCode {
    device_code: String,
    user_code: String,
    verification_url: String,
    expires_at: i64,
    interval: i64,
}

/// In-memory state: device codes being confirmed, per user.
#[derive(Clone, Default)]
pub struct TraktState {
    pending: Arc<Mutex<HashMap<i64, PendingCode>>>,
}

#[derive(Debug)]
pub enum TraktError {
    NotConfigured,
    NotConnected,
    Expired,
    Http(u16, String),
    Network(String),
    Db(flixnet_db::DbError),
}

impl std::error::Error for TraktError {}

impl std::fmt::Display for TraktError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TraktError::NotConfigured => {
                write!(f, "Trakt ist auf diesem Server nicht eingerichtet.")
            }
            TraktError::NotConnected => write!(f, "Kein Trakt-Konto verbunden."),
            TraktError::Expired => write!(
                f,
                "Die Trakt-Verbindung ist abgelaufen. Bitte neu verbinden."
            ),
            TraktError::Http(status, body) => {
                write!(f, "Trakt antwortet mit HTTP {status}: {body}")
            }
            TraktError::Network(err) => write!(f, "Trakt ist nicht erreichbar: {err}"),
            TraktError::Db(err) => write!(f, "{err}"),
        }
    }
}

impl From<flixnet_db::DbError> for TraktError {
    fn from(err: flixnet_db::DbError) -> Self {
        TraktError::Db(err)
    }
}

impl From<reqwest::Error> for TraktError {
    fn from(err: reqwest::Error) -> Self {
        TraktError::Network(err.without_url().to_string())
    }
}

impl From<TraktError> for ApiError {
    fn from(err: TraktError) -> Self {
        match err {
            TraktError::NotConfigured => {
                ApiError::conflict("trakt_not_configured", err.to_string())
            }
            TraktError::NotConnected => ApiError::not_found(err.to_string()),
            TraktError::Expired => ApiError::conflict("trakt_expired", err.to_string()),
            TraktError::Http(..) | TraktError::Network(_) => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "trakt_unavailable",
                err.to_string(),
            ),
            TraktError::Db(err) => err.into(),
        }
    }
}

/// HTTP access to Trakt for one app.
pub struct TraktClient {
    http: reqwest::Client,
    base: String,
    client_id: String,
    client_secret: String,
}

impl TraktClient {
    async fn new(http: &reqwest::Client, base: &str, db: &Db) -> Result<Self, TraktError> {
        let app = TraktApp::load(db).await?;
        let (id, secret) = app.credentials().ok_or(TraktError::NotConfigured)?;
        Ok(Self {
            http: http.clone(),
            base: base.trim_end_matches('/').to_owned(),
            client_id: id.to_owned(),
            client_secret: secret.to_owned(),
        })
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        token: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("Content-Type", "application/json")
            .header("trakt-api-version", "2")
            .header("trakt-api-key", &self.client_id);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request
    }

    async fn json<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, TraktError> {
        let status = response.status();
        if !status.is_success() {
            let body: String = response
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect();
            return Err(TraktError::Http(status.as_u16(), body));
        }
        response
            .json()
            .await
            .map_err(|e| TraktError::Network(e.without_url().to_string()))
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, token: &str) -> Result<T, TraktError> {
        Self::json(
            self.request(reqwest::Method::GET, path, Some(token))
                .send()
                .await?,
        )
        .await
    }

    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        token: Option<&str>,
        body: &Value,
    ) -> Result<T, TraktError> {
        Self::json(
            self.request(reqwest::Method::POST, path, token)
                .json(body)
                .send()
                .await?,
        )
        .await
    }

    /// A valid access token for the account, refreshed (and stored) when it is
    /// about to expire.
    async fn token(&self, db: &Db, account: &TraktAccount) -> Result<String, TraktError> {
        if account.expires_at - flixnet_db::now_ms() > REFRESH_MARGIN_MS {
            return Ok(account.access_token.clone());
        }
        let response = self
            .request(reqwest::Method::POST, "/oauth/token", None)
            .json(&json!({
                "refresh_token": account.refresh_token,
                "client_id": self.client_id,
                "client_secret": self.client_secret,
                "redirect_uri": OOB,
                "grant_type": "refresh_token",
            }))
            .send()
            .await?;
        if matches!(response.status().as_u16(), 400 | 401) {
            return Err(TraktError::Expired);
        }
        let tokens: TokenResponse = Self::json(response).await?;
        db_ops::save_trakt_tokens(
            db,
            account.user_id,
            &tokens.access_token,
            &tokens.refresh_token,
            tokens.expires_at(),
            None,
        )
        .await?;
        Ok(tokens.access_token)
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
    created_at: Option<i64>,
}

impl TokenResponse {
    fn expires_at(&self) -> i64 {
        let created = self
            .created_at
            .map_or_else(flixnet_db::now_ms, |s| s * 1000);
        created + self.expires_in * 1000
    }
}

// ---- DTOs ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct TraktSettingsDto {
    pub client_id: Option<String>,
    /// Whether the client secret is stored (it is never returned).
    pub has_client_secret: bool,
    /// Redirect URI to enter when creating the Trakt app.
    pub redirect_uri: String,
}

/// The Trakt API app. `client_secret` omitted or `null` keeps the stored one; `""`
/// removes it.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateTraktSettings {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DeviceCodeDto {
    /// Code the user enters at `verification_url`.
    pub user_code: String,
    pub verification_url: String,
    /// Unix milliseconds.
    pub expires_at: i64,
    /// Seconds to wait between polls.
    pub interval: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TraktSyncReportDto {
    /// Watched items taken over from Trakt.
    pub pulled_watched: usize,
    /// Ratings taken over from Trakt.
    pub pulled_ratings: usize,
    /// Watched items sent to Trakt.
    pub pushed_watched: usize,
    /// Ratings sent to Trakt.
    pub pushed_ratings: usize,
    /// Trakt entries without a match in the library.
    pub unmatched: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TraktStatusDto {
    /// The administrator registered a Trakt app.
    pub configured: bool,
    pub connected: bool,
    pub username: Option<String>,
    /// Report playback to Trakt while watching.
    pub scrobble: bool,
    /// Ratings are synced as well.
    pub sync_ratings: bool,
    pub last_sync_at: Option<i64>,
    pub last_sync: Option<TraktSyncReportDto>,
    /// A started connection that still waits for confirmation.
    pub pending: Option<DeviceCodeDto>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateTraktOptions {
    pub scrobble: Option<bool>,
    pub sync_ratings: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectState {
    /// Not confirmed yet; poll again after `interval` seconds.
    Pending,
    Connected,
    /// The code expired or was rejected; start again.
    Expired,
    Denied,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ConnectPollDto {
    pub state: ConnectState,
    pub status: TraktStatusDto,
}

/// Outcome of a sync, as stored with the account.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct StoredReport {
    pulled_watched: usize,
    pulled_ratings: usize,
    pushed_watched: usize,
    pushed_ratings: usize,
    unmatched: usize,
}

impl From<StoredReport> for TraktSyncReportDto {
    fn from(r: StoredReport) -> Self {
        Self {
            pulled_watched: r.pulled_watched,
            pulled_ratings: r.pulled_ratings,
            pushed_watched: r.pushed_watched,
            pushed_ratings: r.pushed_ratings,
            unmatched: r.unmatched,
        }
    }
}

async fn status(state: &AppState, user_id: i64) -> ApiResult<TraktStatusDto> {
    let app = TraktApp::load(&state.db).await?;
    let account = db_ops::trakt_account(&state.db, user_id).await?;
    let pending = state
        .ops
        .trakt
        .pending
        .lock()
        .expect("trakt lock")
        .get(&user_id)
        .filter(|p| p.expires_at > flixnet_db::now_ms())
        .map(|p| DeviceCodeDto {
            user_code: p.user_code.clone(),
            verification_url: p.verification_url.clone(),
            expires_at: p.expires_at,
            interval: p.interval,
        });
    Ok(TraktStatusDto {
        configured: app.credentials().is_some(),
        connected: account.is_some(),
        username: account.as_ref().and_then(|a| a.username.clone()),
        scrobble: account.as_ref().is_some_and(|a| a.scrobble),
        sync_ratings: account.as_ref().is_some_and(|a| a.sync_ratings),
        last_sync_at: account.as_ref().and_then(|a| a.last_sync_at),
        last_sync: account
            .as_ref()
            .and_then(|a| a.last_sync_report.as_deref())
            .and_then(|r| serde_json::from_str::<StoredReport>(r).ok())
            .map(Into::into),
        pending: pending.filter(|_| account.is_none()),
    })
}

fn base_url(state: &AppState) -> String {
    state.config.integrations.trakt_api_url.clone()
}

// ---- Routes: administrator settings -------------------------------------------------

fn settings_dto(app: &TraktApp) -> TraktSettingsDto {
    TraktSettingsDto {
        client_id: app.client_id.clone(),
        has_client_secret: app.client_secret.is_some(),
        redirect_uri: OOB.to_owned(),
    }
}

/// The Trakt API app of this server (administrators).
#[utoipa::path(
    get,
    path = "/settings/trakt",
    operation_id = "getTraktSettings",
    tag = "trakt",
    responses(
        (status = OK, description = "Current settings", body = TraktSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn get_trakt_settings(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<TraktSettingsDto>> {
    Ok(Json(settings_dto(&TraktApp::load(&state.db).await?)))
}

/// Register the Trakt API app (client id and secret from trakt.tv/oauth/applications).
#[utoipa::path(
    put,
    path = "/settings/trakt",
    operation_id = "updateTraktSettings",
    tag = "trakt",
    request_body = UpdateTraktSettings,
    responses(
        (status = OK, description = "Updated settings", body = TraktSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn update_trakt_settings(
    _: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<UpdateTraktSettings>,
) -> ApiResult<Json<TraktSettingsDto>> {
    let mut app = TraktApp::load(&state.db).await?;
    if let Some(id) = body.client_id {
        app.client_id = Some(id.trim().to_owned()).filter(|v| !v.is_empty());
    }
    match body.client_secret {
        Some(secret) if secret.trim().is_empty() => app.client_secret = None,
        Some(secret) => app.client_secret = Some(secret.trim().to_owned()),
        None => {}
    }
    settings::set(&state.db, SETTINGS_KEY, &app).await?;
    Ok(Json(settings_dto(&app)))
}

// ---- Routes: the user's connection --------------------------------------------------

/// The signed-in user's Trakt connection.
#[utoipa::path(
    get,
    path = "/trakt",
    operation_id = "getTraktStatus",
    tag = "trakt",
    responses(
        (status = OK, description = "Connection state", body = TraktStatusDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn get_trakt_status(
    user: AuthUser,
    State(state): State<AppState>,
) -> ApiResult<Json<TraktStatusDto>> {
    Ok(Json(status(&state, user.user_id).await?))
}

/// Change scrobbling and rating sync.
#[utoipa::path(
    put,
    path = "/trakt",
    operation_id = "updateTraktOptions",
    tag = "trakt",
    request_body = UpdateTraktOptions,
    responses(
        (status = OK, description = "Connection state", body = TraktStatusDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No Trakt account connected", body = ErrorBody),
    )
)]
async fn update_trakt_options(
    user: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<UpdateTraktOptions>,
) -> ApiResult<Json<TraktStatusDto>> {
    if !db_ops::set_trakt_options(&state.db, user.user_id, body.scrobble, body.sync_ratings).await?
    {
        return Err(TraktError::NotConnected.into());
    }
    Ok(Json(status(&state, user.user_id).await?))
}

/// Disconnect Trakt (the token is revoked at Trakt as well, if possible).
#[utoipa::path(
    delete,
    path = "/trakt",
    operation_id = "disconnectTrakt",
    tag = "trakt",
    responses(
        (status = NO_CONTENT, description = "Disconnected"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn disconnect_trakt(user: AuthUser, State(state): State<AppState>) -> ApiResult<StatusCode> {
    if let Some(account) = db_ops::trakt_account(&state.db, user.user_id).await?
        && let Ok(client) = TraktClient::new(&state.ops.http, &base_url(&state), &state.db).await
    {
        let revoke = client
            .request(reqwest::Method::POST, "/oauth/revoke", None)
            .json(&json!({
                "token": account.access_token,
                "client_id": client.client_id,
                "client_secret": client.client_secret,
            }))
            .send()
            .await;
        if let Err(err) = revoke {
            tracing::info!(err = %err.without_url(), "revoking the Trakt token failed");
        }
    }
    db_ops::delete_trakt_account(&state.db, user.user_id).await?;
    state
        .ops
        .trakt
        .pending
        .lock()
        .expect("trakt lock")
        .remove(&user.user_id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_url: String,
    expires_in: i64,
    interval: i64,
}

/// Start connecting: returns a code to enter on trakt.tv.
#[utoipa::path(
    post,
    path = "/trakt/connect",
    operation_id = "startTraktConnect",
    tag = "trakt",
    responses(
        (status = OK, description = "Code to confirm on trakt.tv", body = DeviceCodeDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = CONFLICT, description = "No Trakt app configured", body = ErrorBody),
        (status = BAD_GATEWAY, description = "Trakt unreachable", body = ErrorBody),
    )
)]
async fn start_trakt_connect(
    user: AuthUser,
    State(state): State<AppState>,
) -> ApiResult<Json<DeviceCodeDto>> {
    let client = TraktClient::new(&state.ops.http, &base_url(&state), &state.db).await?;
    let code: DeviceCodeResponse = client
        .post(
            "/oauth/device/code",
            None,
            &json!({ "client_id": client.client_id }),
        )
        .await?;
    let pending = PendingCode {
        expires_at: flixnet_db::now_ms() + code.expires_in * 1000,
        interval: code.interval.max(1),
        device_code: code.device_code,
        user_code: code.user_code,
        verification_url: code.verification_url,
    };
    let dto = DeviceCodeDto {
        user_code: pending.user_code.clone(),
        verification_url: pending.verification_url.clone(),
        expires_at: pending.expires_at,
        interval: pending.interval,
    };
    state
        .ops
        .trakt
        .pending
        .lock()
        .expect("trakt lock")
        .insert(user.user_id, pending);
    Ok(Json(dto))
}

#[derive(Debug, Deserialize)]
struct UserSettings {
    user: TraktUser,
}

#[derive(Debug, Deserialize)]
struct TraktUser {
    username: String,
}

/// Check whether the code was confirmed; stores the account once it is.
#[utoipa::path(
    post,
    path = "/trakt/connect/poll",
    operation_id = "pollTraktConnect",
    tag = "trakt",
    responses(
        (status = OK, description = "Current state of the connection attempt", body = ConnectPollDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No connection attempt running", body = ErrorBody),
        (status = BAD_GATEWAY, description = "Trakt unreachable", body = ErrorBody),
    )
)]
async fn poll_trakt_connect(
    user: AuthUser,
    State(state): State<AppState>,
) -> ApiResult<Json<ConnectPollDto>> {
    let pending = state
        .ops
        .trakt
        .pending
        .lock()
        .expect("trakt lock")
        .get(&user.user_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("Keine Trakt-Verbindung wird gerade eingerichtet."))?;
    let forget = || {
        state
            .ops
            .trakt
            .pending
            .lock()
            .expect("trakt lock")
            .remove(&user.user_id);
    };
    if pending.expires_at <= flixnet_db::now_ms() {
        forget();
        return Ok(Json(ConnectPollDto {
            state: ConnectState::Expired,
            status: status(&state, user.user_id).await?,
        }));
    }
    let client = TraktClient::new(&state.ops.http, &base_url(&state), &state.db).await?;
    let response = client
        .request(reqwest::Method::POST, "/oauth/device/token", None)
        .json(&json!({
            "code": pending.device_code,
            "client_id": client.client_id,
            "client_secret": client.client_secret,
        }))
        .send()
        .await
        .map_err(TraktError::from)?;
    let result = match response.status().as_u16() {
        200 => {
            let tokens: TokenResponse = TraktClient::json(response).await?;
            let username = client
                .get::<UserSettings>("/users/settings", &tokens.access_token)
                .await
                .ok()
                .map(|s| s.user.username);
            db_ops::save_trakt_tokens(
                &state.db,
                user.user_id,
                &tokens.access_token,
                &tokens.refresh_token,
                tokens.expires_at(),
                username.as_deref(),
            )
            .await?;
            forget();
            tracing::info!(user = user.user_id, "Trakt account connected");
            ConnectState::Connected
        }
        400 | 429 => ConnectState::Pending,
        418 => {
            forget();
            ConnectState::Denied
        }
        404 | 409 | 410 => {
            forget();
            ConnectState::Expired
        }
        other => {
            let body: String = response
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect();
            return Err(TraktError::Http(other, body).into());
        }
    };
    Ok(Json(ConnectPollDto {
        state: result,
        status: status(&state, user.user_id).await?,
    }))
}

/// Sync watched history (and ratings) with Trakt now, in both directions.
#[utoipa::path(
    post,
    path = "/trakt/sync",
    operation_id = "syncTrakt",
    tag = "trakt",
    responses(
        (status = OK, description = "What was synced", body = TraktSyncReportDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No Trakt account connected", body = ErrorBody),
        (status = CONFLICT, description = "No Trakt app, or the connection expired", body = ErrorBody),
        (status = BAD_GATEWAY, description = "Trakt unreachable", body = ErrorBody),
    )
)]
async fn sync_trakt(
    user: AuthUser,
    State(state): State<AppState>,
) -> ApiResult<Json<TraktSyncReportDto>> {
    let client = TraktClient::new(&state.ops.http, &base_url(&state), &state.db).await?;
    Ok(Json(
        sync_user(&client, &state.db, user.user_id).await?.into(),
    ))
}

// ---- Sync ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct TraktIds {
    tmdb: Option<i64>,
    imdb: Option<String>,
    tvdb: Option<i64>,
}

impl TraktIds {
    fn external(&self) -> ExternalIds {
        ExternalIds {
            tmdb: self.tmdb.map(|v| v.to_string()),
            imdb: self.imdb.clone(),
            tvdb: self.tvdb.map(|v| v.to_string()),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct TraktMedia {
    #[serde(default)]
    title: String,
    year: Option<i64>,
    #[serde(default)]
    ids: TraktIds,
}

#[derive(Debug, Deserialize)]
struct WatchedMovie {
    plays: i64,
    last_watched_at: Option<String>,
    movie: TraktMedia,
}

#[derive(Debug, Deserialize)]
struct WatchedShow {
    show: TraktMedia,
    #[serde(default)]
    seasons: Vec<WatchedSeason>,
}

#[derive(Debug, Deserialize)]
struct WatchedSeason {
    number: i64,
    #[serde(default)]
    episodes: Vec<WatchedEpisode>,
}

#[derive(Debug, Deserialize)]
struct WatchedEpisode {
    number: i64,
    plays: i64,
    last_watched_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RatedMovie {
    rating: i64,
    movie: TraktMedia,
}

#[derive(Debug, Deserialize)]
struct RatedEpisode {
    rating: i64,
    episode: RatedEpisodeInfo,
    show: TraktMedia,
}

#[derive(Debug, Deserialize)]
struct RatedEpisodeInfo {
    season: i64,
    number: i64,
}

fn ids_json(ids: &ExternalIds) -> Value {
    let mut map = serde_json::Map::new();
    if let Some(v) = ids.tmdb.as_deref().and_then(|v| v.parse::<i64>().ok()) {
        map.insert("tmdb".into(), json!(v));
    }
    if let Some(v) = &ids.imdb {
        map.insert("imdb".into(), json!(v));
    }
    if let Some(v) = ids.tvdb.as_deref().and_then(|v| v.parse::<i64>().ok()) {
        map.insert("tvdb".into(), json!(v));
    }
    Value::Object(map)
}

/// Movies and shows (with season/episode numbers) to send to Trakt.
#[derive(Default)]
struct Outgoing {
    movies: Vec<Value>,
    /// series item → season → episodes
    shows: HashMap<i64, HashMap<i64, Vec<Value>>>,
    count: usize,
}

impl Outgoing {
    fn add(&mut self, index: &LibraryIndex, item_id: i64, extra: Value) -> bool {
        let Some(item) = index.item(item_id) else {
            return false;
        };
        let mut entry = extra;
        match item.kind.as_str() {
            "movie" => {
                let ids = index.ids(item_id);
                if ids.is_empty() {
                    return false;
                }
                entry["ids"] = ids_json(&ids);
                self.movies.push(entry);
            }
            "episode" => {
                let (Some(series), Some(season), Some(number)) =
                    (item.series_id, item.season, item.episode)
                else {
                    return false;
                };
                if index.ids(series).is_empty() {
                    return false;
                }
                entry["number"] = json!(number);
                self.shows
                    .entry(series)
                    .or_default()
                    .entry(season)
                    .or_default()
                    .push(entry);
            }
            _ => return false,
        }
        self.count += 1;
        true
    }

    fn body(self, index: &LibraryIndex) -> Value {
        let shows: Vec<Value> = self
            .shows
            .into_iter()
            .map(|(series, seasons)| {
                json!({
                    "ids": ids_json(&index.ids(series)),
                    "seasons": seasons.into_iter().map(|(number, episodes)| json!({
                        "number": number,
                        "episodes": episodes,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        json!({ "movies": self.movies, "shows": shows })
    }
}

/// Two-way sync for one user. Watched state and ratings from Trakt are merged into
/// Flixnet (local ratings win); what only Flixnet knows is sent to Trakt.
pub async fn sync_user(
    client: &TraktClient,
    db: &Db,
    user_id: i64,
) -> Result<StoredReport, TraktError> {
    let account = db_ops::trakt_account(db, user_id)
        .await?
        .ok_or(TraktError::NotConnected)?;
    let token = client.token(db, &account).await?;
    let index = LibraryIndex::load(db).await?;
    let mut report = StoredReport::default();

    // Pull: watched movies and episodes.
    let mut states: HashMap<String, ImportedState> = HashMap::new();
    let mut remote_watched: HashSet<String> = HashSet::new();
    let movies: Vec<WatchedMovie> = client.get("/sync/watched/movies", &token).await?;
    for watched in movies {
        let Some((id, _)) = index.movie(
            &watched.movie.ids.external(),
            &watched.movie.title,
            watched.movie.year,
        ) else {
            report.unmatched += 1;
            continue;
        };
        let item = index.item(id).expect("indexed");
        remote_watched.insert(item.identity.clone());
        states.insert(
            item.identity.clone(),
            ImportedState {
                item_id: id,
                identity: item.identity.clone(),
                played: true,
                play_count: watched.plays,
                last_played_at: watched.last_watched_at.as_deref().and_then(parse_rfc3339),
                ..Default::default()
            },
        );
    }
    let shows: Vec<WatchedShow> = client.get("/sync/watched/shows", &token).await?;
    for watched in shows {
        let Some((series, _)) = index.series(
            &watched.show.ids.external(),
            &watched.show.title,
            watched.show.year,
        ) else {
            report.unmatched += watched
                .seasons
                .iter()
                .map(|s| s.episodes.len())
                .sum::<usize>();
            continue;
        };
        for season in &watched.seasons {
            for episode in &season.episodes {
                let Some(id) = index.episode_of(series, season.number, episode.number) else {
                    report.unmatched += 1;
                    continue;
                };
                let item = index.item(id).expect("indexed");
                remote_watched.insert(item.identity.clone());
                states.insert(
                    item.identity.clone(),
                    ImportedState {
                        item_id: id,
                        identity: item.identity.clone(),
                        played: true,
                        play_count: episode.plays,
                        last_played_at: episode.last_watched_at.as_deref().and_then(parse_rfc3339),
                        ..Default::default()
                    },
                );
            }
        }
    }

    // Pull: ratings.
    let mut remote_ratings: HashMap<String, i64> = HashMap::new();
    if account.sync_ratings {
        let rated: Vec<RatedMovie> = client.get("/sync/ratings/movies", &token).await?;
        for r in rated {
            if let Some((id, _)) =
                index.movie(&r.movie.ids.external(), &r.movie.title, r.movie.year)
            {
                let item = index.item(id).expect("indexed");
                remote_ratings.insert(item.identity.clone(), r.rating);
                states
                    .entry(item.identity.clone())
                    .or_insert_with(|| ImportedState {
                        item_id: id,
                        identity: item.identity.clone(),
                        ..Default::default()
                    })
                    .rating = Some(r.rating as f64);
            }
        }
        let rated: Vec<RatedEpisode> = client.get("/sync/ratings/episodes", &token).await?;
        for r in rated {
            let Some((series, _)) =
                index.series(&r.show.ids.external(), &r.show.title, r.show.year)
            else {
                continue;
            };
            if let Some(id) = index.episode_of(series, r.episode.season, r.episode.number) {
                let item = index.item(id).expect("indexed");
                remote_ratings.insert(item.identity.clone(), r.rating);
                states
                    .entry(item.identity.clone())
                    .or_insert_with(|| ImportedState {
                        item_id: id,
                        identity: item.identity.clone(),
                        ..Default::default()
                    })
                    .rating = Some(r.rating as f64);
            }
        }
    }

    // What Flixnet knows before merging, to decide what Trakt is missing.
    let local = history::local_states(db, user_id).await?;
    let local_watched: HashSet<String> = local
        .iter()
        .filter(|l| l.played)
        .filter_map(|l| index.item(l.item_id).map(|i| i.identity.clone()))
        .collect();
    let local_rated: HashSet<String> = local
        .iter()
        .filter(|l| l.rating.is_some())
        .filter_map(|l| index.item(l.item_id).map(|i| i.identity.clone()))
        .collect();
    report.pulled_watched = remote_watched.difference(&local_watched).count();
    report.pulled_ratings = remote_ratings
        .keys()
        .filter(|identity| !local_rated.contains(*identity))
        .count();
    let states: Vec<ImportedState> = states.into_values().collect();
    history::apply_import(db, user_id, "trakt", "Trakt", &states).await?;

    // Push: watched items Trakt does not have.
    let mut history_out = Outgoing::default();
    let mut ratings_out = Outgoing::default();
    for l in &local {
        let Some(item) = index.item(l.item_id) else {
            continue;
        };
        if l.played && !remote_watched.contains(&item.identity) {
            let watched_at = l
                .last_played_at
                .map_or_else(|| json!("released"), |ms| json!(format_rfc3339(ms)));
            history_out.add(&index, l.item_id, json!({ "watched_at": watched_at }));
        }
        if account.sync_ratings
            && let Some(rating) = l.rating
        {
            let rating = (rating.round() as i64).clamp(1, 10);
            if remote_ratings.get(&item.identity) != Some(&rating) {
                ratings_out.add(&index, l.item_id, json!({ "rating": rating }));
            }
        }
    }
    if history_out.count > 0 {
        report.pushed_watched = history_out.count;
        let _: Value = client
            .post("/sync/history", Some(&token), &history_out.body(&index))
            .await?;
    }
    if ratings_out.count > 0 {
        report.pushed_ratings = ratings_out.count;
        let _: Value = client
            .post("/sync/ratings", Some(&token), &ratings_out.body(&index))
            .await?;
    }

    db_ops::set_trakt_synced(
        db,
        user_id,
        flixnet_db::now_ms(),
        &serde_json::to_string(&report).expect("json"),
    )
    .await?;
    tracing::info!(user = user_id, ?report, "Trakt sync finished");
    Ok(report)
}

// ---- Scrobbling ---------------------------------------------------------------------

/// The body identifying an item for `/scrobble/*`; `None` without provider ids.
async fn scrobble_body(db: &Db, event: &PlaybackEvent) -> flixnet_db::Result<Option<Value>> {
    let Some(item) = items::get(db, event.item_id).await? else {
        return Ok(None);
    };
    let ids_of = |pairs: Vec<(String, String)>| {
        ExternalIds::from_pairs(pairs.iter().map(|(p, v)| (p.as_str(), v.as_str())))
    };
    let progress = (event.progress_percent() * 100.0).round() / 100.0;
    Ok(match item.kind.as_str() {
        "movie" => {
            let ids = ids_of(items::provider_ids(db, item.id).await?);
            (!ids.is_empty())
                .then(|| json!({ "movie": { "ids": ids_json(&ids) }, "progress": progress }))
        }
        "episode" => match (item.series_id, item.parent_index_number, item.index_number) {
            (Some(series), Some(season), Some(number)) => {
                let ids = ids_of(items::provider_ids(db, series).await?);
                (!ids.is_empty()).then(|| {
                    json!({
                        "show": { "ids": ids_json(&ids) },
                        "episode": { "season": season, "number": number },
                        "progress": progress,
                    })
                })
            }
            _ => None,
        },
        _ => None,
    })
}

async fn scrobble(
    http: &reqwest::Client,
    base: &str,
    db: &Db,
    event: &ServerEvent,
) -> Result<(), TraktError> {
    let (action, playback) = match event {
        ServerEvent::PlaybackStarted(p) | ServerEvent::PlaybackResumed(p) => ("start", p),
        ServerEvent::PlaybackPaused(p) => ("pause", p),
        ServerEvent::PlaybackStopped(p) => ("stop", p),
        _ => return Ok(()),
    };
    let Some(account) = db_ops::trakt_account(db, playback.user_id).await? else {
        return Ok(());
    };
    if !account.scrobble {
        return Ok(());
    }
    let Some(body) = scrobble_body(db, playback).await? else {
        return Ok(());
    };
    let client = TraktClient::new(http, base, db).await?;
    let token = client.token(db, &account).await?;
    let response = client
        .request(
            reqwest::Method::POST,
            &format!("/scrobble/{action}"),
            Some(&token),
        )
        .json(&body)
        .send()
        .await?;
    // 409: Trakt already recorded this (e.g. a repeated stop) – fine.
    if !response.status().is_success() && response.status().as_u16() != 409 {
        let status = response.status().as_u16();
        let text: String = response
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect();
        return Err(TraktError::Http(status, text));
    }
    Ok(())
}

/// Scrobbles playback events of connected users (needs a Tokio runtime).
pub(crate) fn spawn_scrobbler(state: &AppState) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let mut events = state.events.subscribe();
    let db = state.db.clone();
    let http = state.ops.http.clone();
    let base = base_url(state);
    runtime.spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if let Err(err) = scrobble(&http, &base, &db, &event).await {
                        tracing::warn!(%err, "Trakt scrobble failed");
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

// ---- Scheduled sync -----------------------------------------------------------------

/// Syncs every connected user (scheduled task `trakt_sync`).
pub struct SyncAllTrakt {
    pub http: reqwest::Client,
    pub base_url: String,
}

impl JobHandler for SyncAllTrakt {
    const KIND: &'static str = "trakt.sync_all";
    const POOL: Pool = Pool::Io;
    const MAX_ATTEMPTS: i64 = 1;
    type Payload = ();

    async fn run(&self, ctx: JobContext, _: ()) -> anyhow::Result<()> {
        let users = db_ops::trakt_user_ids(ctx.db()).await?;
        if users.is_empty() {
            return Ok(());
        }
        let client = match TraktClient::new(&self.http, &self.base_url, ctx.db()).await {
            Ok(client) => client,
            Err(TraktError::NotConfigured) => return Ok(()),
            Err(err) => return Err(anyhow::anyhow!("{err}")),
        };
        for user in users {
            if ctx.is_cancelled() {
                break;
            }
            if let Err(err) = sync_user(&client, ctx.db(), user).await {
                tracing::warn!(user, %err, "Trakt sync failed");
            }
        }
        Ok(())
    }
}

pub fn tasks() -> Vec<TaskDef> {
    vec![TaskDef::new::<SyncAllTrakt>(
        "trakt_sync",
        "Gesehen-Status und Bewertungen mit Trakt abgleichen",
        Duration::from_secs(6 * 60 * 60),
        &(),
    )]
}
