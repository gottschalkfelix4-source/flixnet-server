//! Sign-in (`Users/AuthenticateByName`), users and client sessions.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use flixnet_api::auth::AuthUser;
use flixnet_api::error::{ApiError, ApiResult};
use flixnet_api::session::{DeviceInfo, login_with_password};
use flixnet_db::{libraries, sessions, users};
use serde::Deserialize;

use crate::JfState;
use crate::auth::{ClientInfo, JfUser, client_info};
use crate::dto::{
    AuthenticationResult, PlayerStateInfo, SessionInfoDto, UserConfiguration, UserDto, UserPolicy,
};
use crate::ids::{self, Kind};
use crate::util::{Jf, JfJson, bad_request, iso};

pub(crate) fn router() -> Router<JfState> {
    Router::new()
        .route("/users/authenticatebyname", post(authenticate_by_name))
        .route("/users/public", get(public_users))
        .route("/users/me", get(me))
        .route("/users", get(list_users))
        .route("/users/{id}", get(get_user))
        .route("/users/configuration", post(no_content))
        .route("/users/{id}/configuration", post(no_content))
        .route("/sessions", get(list_sessions))
        .route("/sessions/capabilities", post(no_content))
        .route("/sessions/capabilities/full", post(no_content))
        .route("/sessions/logout", post(logout))
        // Findroid renames its device here (without catching errors).
        .route("/devices/options", post(no_content))
}

#[derive(Debug, Deserialize)]
struct AuthenticateByName {
    username: Option<String>,
    pw: Option<String>,
    /// Very old clients send `Password`.
    password: Option<String>,
}

pub(crate) async fn user_dto(state: &JfState, user: &users::User) -> ApiResult<UserDto> {
    let all = user.is_admin || user.all_libraries;
    let enabled_folders = if all {
        Vec::new()
    } else {
        libraries::granted_library_ids(&state.app.db, user.id)
            .await?
            .into_iter()
            .map(|id| ids::guid(Kind::Library, id))
            .collect()
    };
    Ok(UserDto {
        name: user.name.clone(),
        server_id: state.server_id.to_string(),
        server_name: state.server_name.to_string(),
        id: ids::guid(Kind::User, user.id),
        has_password: user.password_hash.is_some(),
        has_configured_password: user.password_hash.is_some(),
        has_configured_easy_password: false,
        enable_auto_login: false,
        last_login_date: None,
        last_activity_date: None,
        configuration: UserConfiguration::default(),
        policy: UserPolicy {
            is_administrator: user.is_admin,
            is_hidden: user.is_hidden,
            enable_collection_management: false,
            enable_subtitle_management: false,
            enable_lyric_management: false,
            is_disabled: false,
            max_parental_rating: user.max_age_rating,
            blocked_tags: Vec::new(),
            allowed_tags: Vec::new(),
            enable_user_preference_access: true,
            access_schedules: Vec::new(),
            block_unrated_items: Vec::new(),
            enable_remote_control_of_other_users: false,
            enable_shared_device_control: false,
            enable_remote_access: true,
            enable_live_tv_management: false,
            enable_live_tv_access: false,
            enable_media_playback: true,
            enable_audio_playback_transcoding: true,
            enable_video_playback_transcoding: true,
            enable_playback_remuxing: true,
            force_remote_source_transcoding: false,
            enable_content_deletion: false,
            enable_content_deletion_from_folders: Vec::new(),
            enable_content_downloading: true,
            enable_sync_transcoding: false,
            enable_media_conversion: false,
            enabled_devices: Vec::new(),
            enable_all_devices: true,
            enabled_channels: Vec::new(),
            enable_all_channels: false,
            enabled_folders,
            enable_all_folders: all,
            invalid_login_attempt_count: 0,
            login_attempts_before_lockout: -1,
            max_active_sessions: 0,
            enable_public_sharing: false,
            blocked_media_folders: Vec::new(),
            blocked_channels: Vec::new(),
            remote_client_bitrate_limit: 0,
            authentication_provider_id: "Jellyfin.Server.Implementations.Users.DefaultAuthenticationProvider",
            password_reset_provider_id: "Jellyfin.Server.Implementations.Users.DefaultPasswordResetProvider",
            sync_play_access: "None",
        },
    })
}

pub(crate) fn session_dto(
    state: &JfState,
    user: &AuthUser,
    client: &ClientInfo,
    now_ms: i64,
) -> SessionInfoDto {
    SessionInfoDto {
        play_state: PlayerStateInfo {
            position_ticks: None,
            can_seek: false,
            is_paused: false,
            is_muted: false,
            media_source_id: None,
            play_method: None,
            repeat_mode: "RepeatNone",
            playback_order: "Default",
        },
        additional_users: Vec::new(),
        playable_media_types: vec!["Video", "Audio"],
        id: ids::guid(Kind::Device, user.device_id),
        user_id: ids::guid(Kind::User, user.user_id),
        user_name: user.name.clone(),
        client: client.client.clone(),
        last_activity_date: iso(now_ms),
        last_playback_check_in: iso(now_ms),
        device_name: client.device.clone(),
        device_id: client.device_id.clone(),
        application_version: client.version.clone(),
        now_playing_item: None,
        is_active: true,
        supports_media_control: false,
        supports_remote_control: false,
        now_playing_queue: Vec::new(),
        has_custom_device_name: false,
        server_id: state.server_id.to_string(),
        supported_commands: Vec::new(),
    }
}

/// Signs in with name and password and issues a Flixnet device token for the
/// client's `DeviceId` (from the authorization header).
async fn authenticate_by_name(
    State(state): State<JfState>,
    headers: HeaderMap,
    JfJson(body): JfJson<AuthenticateByName>,
) -> ApiResult<Jf<AuthenticationResult>> {
    let client = client_info(&headers);
    let device_id = client
        .device_id
        .clone()
        .ok_or_else(|| bad_request("DeviceId fehlt im Authorization-Header."))?;
    let name = body
        .username
        .filter(|n| !n.trim().is_empty())
        .ok_or_else(|| bad_request("Username fehlt."))?;
    let password = body.pw.or(body.password).unwrap_or_default();
    let device = DeviceInfo {
        id: format!("jellyfin:{device_id}").chars().take(128).collect(),
        name: client
            .device
            .clone()
            .unwrap_or_else(|| "Jellyfin-Gerät".to_owned())
            .chars()
            .take(128)
            .collect(),
        client: match (&client.client, &client.version) {
            (Some(c), Some(v)) => format!("{c} {v}"),
            (Some(c), None) => c.clone(),
            _ => "Jellyfin-Client".to_owned(),
        }
        .chars()
        .take(128)
        .collect(),
    };
    let session = login_with_password(&state.app, &name, &password, &device).await?;
    let user = users::get(&state.app.db, session.user.id)
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    let auth = flixnet_api::auth::authenticate_token(&state.app, &session.token).await?;
    Ok(Jf(AuthenticationResult {
        user: user_dto(&state, &user).await?,
        session_info: session_dto(&state, &auth, &client, flixnet_db::now_ms()),
        access_token: session.token,
        server_id: state.server_id.to_string(),
    }))
}

/// No public user list: clients show the manual sign-in instead.
async fn public_users() -> Jf<Vec<UserDto>> {
    Jf(Vec::new())
}

async fn load_user(state: &JfState, id: i64) -> ApiResult<users::User> {
    users::get(&state.app.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Nutzer nicht gefunden."))
}

async fn me(jf: JfUser, State(state): State<JfState>) -> ApiResult<Jf<UserDto>> {
    let user = load_user(&state, jf.user.user_id).await?;
    Ok(Jf(user_dto(&state, &user).await?))
}

/// Administrators see everyone, others only themselves.
async fn list_users(jf: JfUser, State(state): State<JfState>) -> ApiResult<Jf<Vec<UserDto>>> {
    let mut out = Vec::new();
    for user in users::list(&state.app.db).await? {
        if jf.user.is_admin || user.id == jf.user.user_id {
            out.push(user_dto(&state, &user).await?);
        }
    }
    Ok(Jf(out))
}

async fn get_user(
    jf: JfUser,
    State(state): State<JfState>,
    Path(id): Path<String>,
) -> ApiResult<Jf<UserDto>> {
    let id = ids::parse_as(&id, Kind::User)
        .filter(|id| jf.user.is_admin || *id == jf.user.user_id)
        .ok_or_else(|| ApiError::not_found("Nutzer nicht gefunden."))?;
    let user = load_user(&state, id).await?;
    Ok(Jf(user_dto(&state, &user).await?))
}

/// The caller's own session (remote control of other devices is not supported).
async fn list_sessions(jf: JfUser, State(state): State<JfState>) -> Jf<Vec<SessionInfoDto>> {
    Jf(vec![session_dto(
        &state,
        &jf.user,
        &jf.client,
        flixnet_db::now_ms(),
    )])
}

async fn logout(jf: JfUser, State(state): State<JfState>) -> ApiResult<StatusCode> {
    sessions::revoke(&state.app.db, jf.user.token_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Accepted and ignored (capabilities, user configuration).
async fn no_content(_: JfUser) -> StatusCode {
    StatusCode::NO_CONTENT
}
