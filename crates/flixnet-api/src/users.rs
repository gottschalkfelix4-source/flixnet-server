//! `/api/v1/users/*`: the signed-in user's account and, for administrators,
//! managing all household members.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use flixnet_db::{sessions, users};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::{AdminUser, AuthUser};
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::session::{UserDto, hash_secret, verify_secret};
use crate::validate;

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(me))
        .routes(routes!(change_password))
        .routes(routes!(set_pin))
        .routes(routes!(list_devices))
        .routes(routes!(sign_out_device))
        .routes(routes!(list_users, create_user))
        .routes(routes!(update_user, delete_user))
        .routes(routes!(get_library_access, set_library_access))
}

/// Which libraries a user may see.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LibraryAccess {
    /// All current and future libraries.
    pub all_libraries: bool,
    /// Granted libraries when `all_libraries` is false.
    pub library_ids: Vec<i64>,
}

/// A user's library access (administrators only).
#[utoipa::path(
    get,
    path = "/{id}/libraries",
    operation_id = "getLibraryAccess",
    tag = "users",
    params(("id" = i64, Path, description = "User id")),
    responses(
        (status = OK, description = "Library access", body = LibraryAccess),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such user", body = ErrorBody),
    )
)]
async fn get_library_access(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<LibraryAccess>> {
    let user = users::get(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Nutzer nicht gefunden."))?;
    Ok(Json(LibraryAccess {
        all_libraries: user.all_libraries,
        library_ids: flixnet_db::libraries::granted_library_ids(&state.db, id).await?,
    }))
}

/// Set a user's library access (administrators only).
#[utoipa::path(
    put,
    path = "/{id}/libraries",
    operation_id = "setLibraryAccess",
    tag = "users",
    params(("id" = i64, Path, description = "User id")),
    request_body = LibraryAccess,
    responses(
        (status = OK, description = "Updated library access", body = LibraryAccess),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such user", body = ErrorBody),
    )
)]
async fn set_library_access(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<LibraryAccess>,
) -> ApiResult<Json<LibraryAccess>> {
    if users::get(&state.db, id).await?.is_none() {
        return Err(ApiError::not_found("Nutzer nicht gefunden."));
    }
    flixnet_db::libraries::set_access(&state.db, id, body.all_libraries, &body.library_ids).await?;
    Ok(Json(LibraryAccess {
        all_libraries: body.all_libraries,
        library_ids: flixnet_db::libraries::granted_library_ids(&state.db, id).await?,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetPinRequest {
    /// 4–8 digits, or `null` to remove the PIN.
    pub pin: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DeviceDto {
    pub id: i64,
    pub name: String,
    pub client: String,
    pub created_at: i64,
    pub last_seen: i64,
    /// The device making this request.
    pub is_current: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateUserRequest {
    pub name: String,
    /// Omit for a profile that cannot sign in remotely (only reachable by switching).
    pub password: Option<String>,
    pub pin: Option<String>,
    #[serde(default)]
    pub is_admin: bool,
    #[serde(default)]
    pub is_hidden: bool,
    pub max_age_rating: Option<i64>,
    /// With an age limit: show content without an age rating anyway.
    #[serde(default)]
    pub allow_unrated: bool,
    /// Genres to hide from the profile.
    #[serde(default)]
    pub blocked_genres: Vec<String>,
}

/// Every field is optional; `null` clears nullable fields (password, PIN, age limit).
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct UpdateUserRequest {
    pub name: Option<String>,
    pub is_admin: Option<bool>,
    pub is_hidden: Option<bool>,
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<i64>, nullable)]
    pub max_age_rating: Option<Option<i64>>,
    /// Setting or clearing the password signs the user out everywhere.
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub password: Option<Option<String>>,
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub pin: Option<Option<String>>,
    /// With an age limit: show content without an age rating anyway.
    pub allow_unrated: Option<bool>,
    /// Genres to hide from the profile (replaces the list; `[]` clears it).
    pub blocked_genres: Option<Vec<String>>,
}

/// The signed-in user.
#[utoipa::path(
    get,
    path = "/me",
    operation_id = "getCurrentUser",
    tag = "users",
    responses(
        (status = OK, description = "The signed-in user", body = UserDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn me(user: AuthUser, State(state): State<AppState>) -> ApiResult<Json<UserDto>> {
    let user = users::get(&state.db, user.user_id)
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    Ok(Json(user.into()))
}

/// Change the own password. Signs out all other devices.
#[utoipa::path(
    put,
    path = "/me/password",
    operation_id = "changePassword",
    tag = "users",
    request_body = ChangePasswordRequest,
    responses(
        (status = NO_CONTENT, description = "Password changed"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Wrong current password", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "New password too short", body = ErrorBody),
        (status = TOO_MANY_REQUESTS, description = "Too many failed attempts", body = ErrorBody),
    )
)]
async fn change_password(
    user: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<ChangePasswordRequest>,
) -> ApiResult<StatusCode> {
    validate::password(&body.new_password)?;
    let account = users::get(&state.db, user.user_id)
        .await?
        .ok_or_else(ApiError::unauthorized)?;

    let limit_key = format!("login:{}", account.name.to_lowercase());
    state.limits.login.check(&limit_key)?;
    // Profiles without a password may set one without proving an old one.
    if account.password_hash.is_some()
        && !verify_secret(body.current_password, account.password_hash).await?
    {
        state.limits.login.record_failure(&limit_key);
        // 403, not 401: the session is valid, only the re-entered password is wrong.
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "invalid_credentials",
            "Das aktuelle Passwort ist falsch.",
        ));
    }
    state.limits.login.reset(&limit_key);

    let hash = hash_secret(body.new_password).await?;
    users::update(
        &state.db,
        user.user_id,
        &users::UserPatch {
            password_hash: Some(Some(&hash)),
            ..Default::default()
        },
    )
    .await?;
    sessions::revoke_all_for_user(&state.db, user.user_id, Some(user.token_id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Set or remove the own profile PIN.
#[utoipa::path(
    put,
    path = "/me/pin",
    operation_id = "setPin",
    tag = "users",
    request_body = SetPinRequest,
    responses(
        (status = NO_CONTENT, description = "PIN updated"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "PIN is not 4–8 digits", body = ErrorBody),
    )
)]
async fn set_pin(
    user: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<SetPinRequest>,
) -> ApiResult<StatusCode> {
    let hash = match body.pin {
        Some(pin) => {
            validate::pin(&pin)?;
            Some(hash_secret(pin).await?)
        }
        None => None,
    };
    users::update(
        &state.db,
        user.user_id,
        &users::UserPatch {
            pin_hash: Some(hash.as_deref()),
            ..Default::default()
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Devices signed in to the own account.
#[utoipa::path(
    get,
    path = "/me/devices",
    operation_id = "listMyDevices",
    tag = "users",
    responses(
        (status = OK, description = "Signed-in devices, most recent first", body = Vec<DeviceDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn list_devices(
    user: AuthUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<DeviceDto>>> {
    let devices = sessions::list_devices(&state.db, user.user_id)
        .await?
        .into_iter()
        .map(|d| DeviceDto {
            is_current: d.id == user.device_id,
            id: d.id,
            name: d.name,
            client: d.client,
            created_at: d.created_at,
            last_seen: d.last_seen,
        })
        .collect();
    Ok(Json(devices))
}

/// Sign out one of the own devices.
#[utoipa::path(
    delete,
    path = "/me/devices/{id}",
    operation_id = "signOutDevice",
    tag = "users",
    params(("id" = i64, Path, description = "Device id")),
    responses(
        (status = NO_CONTENT, description = "Device signed out"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such signed-in device", body = ErrorBody),
    )
)]
async fn sign_out_device(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    if sessions::revoke_device(&state.db, user.user_id, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("Gerät nicht gefunden."))
    }
}

/// All users (administrators only).
#[utoipa::path(
    get,
    path = "",
    operation_id = "listUsers",
    tag = "users",
    responses(
        (status = OK, description = "All users", body = Vec<UserDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn list_users(_: AdminUser, State(state): State<AppState>) -> ApiResult<Json<Vec<UserDto>>> {
    let users = users::list(&state.db).await?;
    Ok(Json(users.into_iter().map(UserDto::from).collect()))
}

/// Create a user or household profile (administrators only).
#[utoipa::path(
    post,
    path = "",
    operation_id = "createUser",
    tag = "users",
    request_body = CreateUserRequest,
    responses(
        (status = CREATED, description = "User created", body = UserDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = CONFLICT, description = "Name is taken", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid input", body = ErrorBody),
    )
)]
async fn create_user(
    _: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<CreateUserRequest>,
) -> ApiResult<(StatusCode, Json<UserDto>)> {
    let name = validate::name(&body.name)?;
    validate::age_rating(body.max_age_rating)?;
    let blocked_genres = validate::genre_list(&body.blocked_genres)?;
    if body.is_admin && body.password.is_none() {
        return Err(ApiError::validation(
            "Administratoren brauchen ein Passwort.",
        ));
    }
    let password_hash = match body.password {
        Some(password) => {
            validate::password(&password)?;
            Some(hash_secret(password).await?)
        }
        None => None,
    };
    let pin_hash = match body.pin {
        Some(pin) => {
            validate::pin(&pin)?;
            Some(hash_secret(pin).await?)
        }
        None => None,
    };

    let user = users::create(
        &state.db,
        &users::NewUser {
            name: &name,
            password_hash: password_hash.as_deref(),
            pin_hash: pin_hash.as_deref(),
            is_admin: body.is_admin,
            is_hidden: body.is_hidden,
            max_age_rating: body.max_age_rating,
        },
    )
    .await
    .map_err(name_taken)?;
    let user = if body.allow_unrated || blocked_genres.is_some() {
        users::update(
            &state.db,
            user.id,
            &users::UserPatch {
                allow_unrated: Some(body.allow_unrated),
                blocked_genres_json: Some(blocked_genres.as_deref()),
                ..Default::default()
            },
        )
        .await?
        .unwrap_or(user)
    } else {
        user
    };
    tracing::info!(user = %user.name, admin = user.is_admin, "user created");
    Ok((StatusCode::CREATED, Json(user.into())))
}

/// Change a user (administrators only).
#[utoipa::path(
    patch,
    path = "/{id}",
    operation_id = "updateUser",
    tag = "users",
    params(("id" = i64, Path, description = "User id")),
    request_body = UpdateUserRequest,
    responses(
        (status = OK, description = "Updated user", body = UserDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such user", body = ErrorBody),
        (status = CONFLICT, description = "Name taken, or the last administrator would be lost", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid input", body = ErrorBody),
    )
)]
async fn update_user(
    AdminUser(admin): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<UpdateUserRequest>,
) -> ApiResult<Json<UserDto>> {
    let existing = users::get(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Nutzer nicht gefunden."))?;

    let name = body.name.as_deref().map(validate::name).transpose()?;
    if let Some(age) = body.max_age_rating {
        validate::age_rating(age)?;
    }
    let blocked_genres = body
        .blocked_genres
        .as_deref()
        .map(validate::genre_list)
        .transpose()?;
    let will_be_admin = body.is_admin.unwrap_or(existing.is_admin);
    if existing.is_admin && !will_be_admin && users::admin_count(&state.db).await? <= 1 {
        return Err(ApiError::conflict(
            "last_admin",
            "Der letzte Administrator kann nicht herabgestuft werden.",
        ));
    }
    let will_have_password = match &body.password {
        Some(new) => new.is_some(),
        None => existing.password_hash.is_some(),
    };
    if will_be_admin && !will_have_password {
        return Err(ApiError::validation(
            "Administratoren brauchen ein Passwort.",
        ));
    }

    let password_hash = match body.password.clone() {
        Some(Some(password)) => {
            validate::password(&password)?;
            Some(Some(hash_secret(password).await?))
        }
        Some(None) => Some(None),
        None => None,
    };
    let pin_hash = match body.pin {
        Some(Some(pin)) => {
            validate::pin(&pin)?;
            Some(Some(hash_secret(pin).await?))
        }
        Some(None) => Some(None),
        None => None,
    };

    let updated = users::update(
        &state.db,
        id,
        &users::UserPatch {
            name: name.as_deref(),
            is_admin: body.is_admin,
            is_hidden: body.is_hidden,
            max_age_rating: body.max_age_rating,
            password_hash: password_hash.as_ref().map(|h| h.as_deref()),
            pin_hash: pin_hash.as_ref().map(|h| h.as_deref()),
            allow_unrated: body.allow_unrated,
            blocked_genres_json: blocked_genres.as_ref().map(|g| g.as_deref()),
        },
    )
    .await
    .map_err(name_taken)?
    .ok_or_else(|| ApiError::not_found("Nutzer nicht gefunden."))?;

    if body.password.is_some() {
        // Keep the admin's own session if they changed their own password.
        let keep = (id == admin.user_id).then_some(admin.token_id);
        sessions::revoke_all_for_user(&state.db, id, keep).await?;
    }
    Ok(Json(updated.into()))
}

/// Delete a user (administrators only; not yourself).
#[utoipa::path(
    delete,
    path = "/{id}",
    operation_id = "deleteUser",
    tag = "users",
    params(("id" = i64, Path, description = "User id")),
    responses(
        (status = NO_CONTENT, description = "User deleted"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such user", body = ErrorBody),
        (status = CONFLICT, description = "Cannot delete yourself", body = ErrorBody),
    )
)]
async fn delete_user(
    AdminUser(admin): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    if id == admin.user_id {
        return Err(ApiError::conflict(
            "cannot_delete_self",
            "Du kannst dein eigenes Konto nicht löschen.",
        ));
    }
    if users::delete(&state.db, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("Nutzer nicht gefunden."))
    }
}

fn name_taken(err: flixnet_db::DbError) -> ApiError {
    match err {
        flixnet_db::DbError::Conflict => {
            ApiError::conflict("name_taken", "Dieser Name ist bereits vergeben.")
        }
        other => other.into(),
    }
}
