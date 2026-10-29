//! `/api/v1/setup` (first-run wizard) and `/api/v1/auth/*` (login/logout).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use flixnet_core::auth;
use flixnet_db::{sessions, users};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult, ErrorBody};

/// Mounted directly under `/api/v1` (paths carry their own `/setup` / `/auth` prefix,
/// so `POST /api/v1/setup` has no trailing slash).
pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(setup_status))
        .routes(routes!(complete_setup))
        .routes(routes!(login))
        .routes(routes!(logout))
}

/// The client installation that is signing in.
#[derive(Debug, Deserialize, ToSchema)]
pub struct DeviceInfo {
    /// Stable id generated and stored by the client (e.g. a UUID).
    #[schema(example = "6f1c2a6e-4c1d-4a8e-9a57-0d7f4f1b2c3d")]
    pub id: String,
    /// Human-readable device name.
    #[schema(example = "Chrome Â· Windows")]
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
    let name = validate_name(&body.name)?;
    validate_password(&body.password)?;
    validate_device(&body.device)?;

    let password = body.password.clone();
    let hash = tokio::task::spawn_blocking(move || auth::hash_password(&password)).await?;
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
    )
)]
async fn login(
    State(state): State<AppState>,
    Json(body): Json<Credentials>,
) -> ApiResult<Json<SessionDto>> {
    validate_device(&body.device)?;
    let user = users::find_by_name(&state.db, body.name.trim()).await?;

    let password = body.password.clone();
    let stored_hash = user.as_ref().and_then(|u| u.password_hash.clone());
    let valid = tokio::task::spawn_blocking(move || match stored_hash {
        Some(hash) => auth::verify_password(&password, &hash),
        None => {
            auth::verify_dummy(&password);
            false
        }
    })
    .await?;

    match user {
        Some(user) if valid => issue_session(&state, user, &body.device).await.map(Json),
        _ => Err(ApiError::invalid_credentials()),
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

async fn issue_session(
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

impl From<users::User> for UserDto {
    fn from(user: users::User) -> Self {
        Self {
            id: user.id,
            name: user.name,
            is_admin: user.is_admin,
        }
    }
}

fn validate_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(ApiError::validation(
            "Der Name muss zwischen 1 und 64 Zeichen lang sein.",
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(ApiError::validation(
            "Der Name enthÃ¤lt ungÃ¼ltige Zeichen.",
        ));
    }
    Ok(name.to_owned())
}

fn validate_password(password: &str) -> ApiResult<()> {
    let len = password.chars().count();
    if !(auth::PASSWORD_MIN_LEN..=auth::PASSWORD_MAX_LEN).contains(&len) {
        return Err(ApiError::validation(format!(
            "Das Passwort muss mindestens {} Zeichen lang sein.",
            auth::PASSWORD_MIN_LEN
        )));
    }
    Ok(())
}

fn validate_device(device: &DeviceInfo) -> ApiResult<()> {
    let ok = |s: &str| {
        let s = s.trim();
        !s.is_empty() && s.chars().count() <= 128 && !s.chars().any(char::is_control)
    };
    if ok(&device.id) && ok(&device.name) && ok(&device.client) {
        Ok(())
    } else {
        Err(ApiError::validation("UngÃ¼ltige GerÃ¤teangaben."))
    }
}
