//! `/api/v1/setup` (first-run wizard) and `/api/v1/auth/*` (sign-in, sign-out,
//! household profile switching).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use flixnet_core::auth;
use flixnet_db::browse::Restriction;
use flixnet_db::{sessions, users};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::validate;

/// Mounted directly under `/api/v1` (paths carry their own `/setup` / `/auth` prefix,
/// so `POST /api/v1/setup` has no trailing slash).
pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(setup_status))
        .routes(routes!(complete_setup))
        .routes(routes!(login))
        .routes(routes!(logout))
        .routes(routes!(list_profiles))
        .routes(routes!(switch_profile))
}

/// The client installation that is signing in.
#[derive(Debug, Deserialize, ToSchema)]
pub struct DeviceInfo {
    /// Stable id generated and stored by the client (e.g. a UUID).
    #[schema(example = "6f1c2a6e-4c1d-4a8e-9a57-0d7f4f1b2c3d")]
    pub id: String,
    /// Human-readable device name.
    #[schema(example = "Chrome · Windows")]
    pub name: String,
    /// Client application.
    #[schema(example = "Flixnet Web")]
    pub client: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct Credentials {
    #[schema(example = "Felix")]
    pub name: String,
    pub password: String,
    pub device: DeviceInfo,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct UserDto {
    pub id: i64,
    pub name: String,
    pub is_admin: bool,
    /// Hidden users do not appear in the profile picker.
    pub is_hidden: bool,
    /// Can sign in with a password (profiles without one are reachable by switching only).
    pub has_password: bool,
    /// Profile switching to this user requires a PIN.
    pub has_pin: bool,
    /// Content age limit; `null` = unrestricted.
    pub max_age_rating: Option<i64>,
    /// With an age limit: content without an age rating is shown anyway.
    pub allow_unrated: bool,
    /// Genres hidden from this profile.
    pub blocked_genres: Vec<String>,
}

/// A newly issued device token plus the signed-in user.
#[derive(Debug, Serialize, ToSchema)]
pub struct SessionDto {
    /// Send as `Authorization: Bearer <token>`. Valid until logout or re-login on this device.
    pub token: String,
    pub user: UserDto,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SetupStatus {
    /// `true` until the first administrator account has been created.
    pub setup_required: bool,
}

/// What switching to a profile requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Unlock {
    /// Anyone on this device may switch.
    None,
    Pin,
    Password,
    /// Not reachable from the current (more restricted) profile: the target is less
    /// restricted and has neither PIN nor password.
    Locked,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProfileDto {
    pub id: i64,
    pub name: String,
    pub is_admin: bool,
    pub unlock: Unlock,
    /// The profile currently signed in on this device.
    pub is_current: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SwitchProfileRequest {
    pub user_id: i64,
    /// Required when the profile's `unlock` is `pin`.
    pub pin: Option<String>,
    /// Required when the profile's `unlock` is `password`.
    pub password: Option<String>,
    pub device: DeviceInfo,
}

/// Is first-run setup still required?
#[utoipa::path(
    get,
    path = "/setup/status",
    operation_id = "getSetupStatus",
    tag = "auth",
    security(()),
    responses((status = OK, description = "Setup state", body = SetupStatus))
)]
async fn setup_status(State(state): State<AppState>) -> ApiResult<Json<SetupStatus>> {
    let setup_required = users::count(&state.db).await? == 0;
    Ok(Json(SetupStatus { setup_required }))
}

/// Create the first administrator and sign in.
///
/// Only works while no user exists; afterwards it answers 409.
#[utoipa::path(
    post,
    path = "/setup",
    operation_id = "completeSetup",
    tag = "auth",
    security(()),
    request_body = Credentials,
    responses(
        (status = OK, description = "Administrator created and signed in", body = SessionDto),
        (status = CONFLICT, description = "Setup was already completed", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid name, password or device", body = ErrorBody),
    )
)]
async fn complete_setup(
    State(state): State<AppState>,
    Json(body): Json<Credentials>,
) -> ApiResult<Json<SessionDto>> {
    let name = validate::name(&body.name)?;
    validate::password(&body.password)?;
    validate_device(&body.device)?;

    let hash = hash_secret(body.password.clone()).await?;
    let user = users::create_first_admin(&state.db, &name, &hash)
        .await?
        .ok_or_else(|| {
            ApiError::conflict("setup_completed", "Flixnet ist bereits eingerichtet.")
        })?;

    tracing::info!(user = %user.name, "first administrator created");
    issue_session(&state, user, &body.device).await.map(Json)
}

/// Sign in with name and password.
#[utoipa::path(
    post,
    path = "/auth/login",
    operation_id = "login",
    tag = "auth",
    security(()),
    request_body = Credentials,
    responses(
        (status = OK, description = "Signed in", body = SessionDto),
        (status = UNAUTHORIZED, description = "Wrong name or password", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid device info", body = ErrorBody),
        (status = TOO_MANY_REQUESTS, description = "Too many failed attempts", body = ErrorBody),
    )
)]
async fn login(
    State(state): State<AppState>,
    Json(body): Json<Credentials>,
) -> ApiResult<Json<SessionDto>> {
    login_with_password(&state, &body.name, &body.password, &body.device)
        .await
        .map(Json)
}

/// Password sign-in with the login rate limit; issues a device token on success.
/// Shared with the Jellyfin compatibility layer (`Users/AuthenticateByName`).
pub async fn login_with_password(
    state: &AppState,
    name: &str,
    password: &str,
    device: &DeviceInfo,
) -> ApiResult<SessionDto> {
    validate_device(device)?;
    let name = name.trim();
    let limit_key = format!("login:{}", name.to_lowercase());
    state.limits.login.check(&limit_key)?;

    let user = users::find_by_name(&state.db, name).await?;
    let stored = user.as_ref().and_then(|u| u.password_hash.clone());
    let valid = verify_secret(password.to_owned(), stored).await?;

    match user {
        Some(user) if valid => {
            state.limits.login.reset(&limit_key);
            issue_session(state, user, device).await
        }
        _ => {
            state.limits.login.record_failure(&limit_key);
            Err(ApiError::invalid_credentials())
        }
    }
}

/// Sign out: revokes the token used for this request.
#[utoipa::path(
    post,
    path = "/auth/logout",
    operation_id = "logout",
    tag = "auth",
    responses(
        (status = NO_CONTENT, description = "Signed out"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn logout(user: AuthUser, State(state): State<AppState>) -> ApiResult<StatusCode> {
    sessions::revoke(&state.db, user.token_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Household profiles for the "Who's watching?" picker.
#[utoipa::path(
    get,
    path = "/auth/profiles",
    operation_id = "listProfiles",
    tag = "auth",
    responses(
        (status = OK, description = "Visible profiles", body = Vec<ProfileDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn list_profiles(
    user: AuthUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<ProfileDto>>> {
    let profiles = users::list(&state.db)
        .await?
        .into_iter()
        .filter(|u| !u.is_hidden || u.id == user.user_id)
        .map(|u| ProfileDto {
            is_current: u.id == user.user_id,
            unlock: unlock_for(&u, &user.restriction),
            id: u.id,
            name: u.name,
            is_admin: u.is_admin,
        })
        .collect();
    Ok(Json(profiles))
}

/// Switch this device to another household profile.
///
/// Issues a new token for the target profile and revokes the current one.
/// Profiles with a PIN need the PIN; administrators without a PIN need their password.
#[utoipa::path(
    post,
    path = "/auth/switch",
    operation_id = "switchProfile",
    tag = "auth",
    request_body = SwitchProfileRequest,
    responses(
        (status = OK, description = "Switched; use the new token from now on", body = SessionDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Wrong PIN or password for the target profile", body = ErrorBody),
        (status = NOT_FOUND, description = "No such visible profile", body = ErrorBody),
        (status = TOO_MANY_REQUESTS, description = "Too many failed attempts", body = ErrorBody),
    )
)]
async fn switch_profile(
    current: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<SwitchProfileRequest>,
) -> ApiResult<Json<SessionDto>> {
    validate_device(&body.device)?;
    let target = users::get(&state.db, body.user_id)
        .await?
        .filter(|u| !u.is_hidden || u.id == current.user_id)
        .ok_or_else(|| ApiError::not_found("Profil nicht gefunden."))?;

    let limit_key = format!("switch:{}", target.id);
    let unlock = if target.id == current.user_id {
        Unlock::None
    } else {
        unlock_for(&target, &current.restriction)
    };
    if unlock == Unlock::Locked {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "profile_locked",
            "Dieses Profil braucht eine PIN, bevor von hier aus gewechselt werden kann.",
        ));
    }
    if unlock != Unlock::None {
        state.limits.unlock.check(&limit_key)?;
        let (secret, stored) = match unlock {
            Unlock::Pin => (body.pin.clone(), target.pin_hash.clone()),
            _ => (body.password.clone(), target.password_hash.clone()),
        };
        let valid = match secret {
            Some(secret) => verify_secret(secret, stored).await?,
            None => false,
        };
        if !valid {
            state.limits.unlock.record_failure(&limit_key);
            // 403, not 401: the device's own session is fine, only the unlock failed.
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "invalid_credentials",
                match unlock {
                    Unlock::Pin => "Die PIN ist falsch.",
                    _ => "Das Passwort ist falsch.",
                },
            ));
        }
        state.limits.unlock.reset(&limit_key);
    }

    sessions::revoke(&state.db, current.token_id).await?;
    issue_session(&state, target, &body.device).await.map(Json)
}

/// What a device signed in with a profile restricted by `current` must provide to
/// switch to `user`. From a restricted profile, a less restricted one is never open:
/// it needs its PIN (or password); without either it cannot be reached by switching.
fn unlock_for(user: &users::User, current: &Restriction) -> Unlock {
    if user.pin_hash.is_some() {
        Unlock::Pin
    } else if user.is_admin {
        Unlock::Password
    } else if current.is_restricted() && !user.restriction().at_least_as_strict_as(current) {
        if user.password_hash.is_some() {
            Unlock::Password
        } else {
            Unlock::Locked
        }
    } else {
        Unlock::None
    }
}

pub(crate) async fn issue_session(
    state: &AppState,
    user: users::User,
    device: &DeviceInfo,
) -> ApiResult<SessionDto> {
    let token = auth::new_token();
    sessions::create(
        &state.db,
        &sessions::NewSession {
            user_id: user.id,
            device_uid: device.id.trim(),
            client: device.client.trim(),
            device_name: device.name.trim(),
            token_hash: &token.hash,
        },
    )
    .await?;
    Ok(SessionDto {
        token: token.token,
        user: UserDto::from(user),
    })
}

/// argon2 hashing off the async runtime.
pub(crate) async fn hash_secret(secret: String) -> ApiResult<String> {
    Ok(tokio::task::spawn_blocking(move || auth::hash_password(&secret)).await?)
}

/// argon2 verification off the async runtime; burns equal time when nothing is stored.
pub(crate) async fn verify_secret(secret: String, stored: Option<String>) -> ApiResult<bool> {
    Ok(tokio::task::spawn_blocking(move || match stored {
        Some(hash) => auth::verify_password(&secret, &hash),
        None => {
            auth::verify_dummy(&secret);
            false
        }
    })
    .await?)
}

impl From<users::User> for UserDto {
    fn from(user: users::User) -> Self {
        let restriction = user.restriction();
        Self {
            allow_unrated: restriction.allow_unrated,
            blocked_genres: restriction.blocked_genres,
            has_password: user.password_hash.is_some(),
            has_pin: user.pin_hash.is_some(),
            id: user.id,
            name: user.name,
            is_admin: user.is_admin,
            is_hidden: user.is_hidden,
            max_age_rating: user.max_age_rating,
        }
    }
}

pub(crate) fn validate_device(device: &DeviceInfo) -> ApiResult<()> {
    let ok = |s: &str| {
        let s = s.trim();
        !s.is_empty() && s.chars().count() <= 128 && !s.chars().any(char::is_control)
    };
    if ok(&device.id) && ok(&device.name) && ok(&device.client) {
        Ok(())
    } else {
        Err(ApiError::validation("Ungültige Geräteangaben."))
    }
}
