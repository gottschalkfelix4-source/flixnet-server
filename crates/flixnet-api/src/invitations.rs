//! `/api/v1/invitations/*`: single-use links that let someone create their own account.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use flixnet_core::auth;
use flixnet_db::invitations::{self, AcceptError, NewInvitation};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AdminUser;
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::session::{Credentials, SessionDto, hash_secret, issue_session, validate_device};
use crate::validate;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_invitations, create_invitation))
        .routes(routes!(delete_invitation))
        .routes(routes!(invitation_status, redeem_invitation))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InvitationDto {
    pub id: i64,
    pub note: Option<String>,
    pub max_age_rating: Option<i64>,
    pub created_at: i64,
    pub expires_at: i64,
    pub used_at: Option<i64>,
    pub used_by: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreatedInvitationDto {
    pub invitation: InvitationDto,
    /// The secret code; shown once. The web UI link is `/invite/{code}`.
    pub code: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateInvitationRequest {
    /// Free text, e.g. who the invitation is for.
    pub note: Option<String>,
    /// Age limit applied to the new account.
    pub max_age_rating: Option<i64>,
    /// Days until the link expires (1–30, default 7).
    pub valid_days: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InvitationStatus {
    pub valid: bool,
}

/// All invitations (administrators only).
#[utoipa::path(
    get,
    path = "",
    operation_id = "listInvitations",
    tag = "invitations",
    responses(
        (status = OK, description = "Invitations, newest first", body = Vec<InvitationDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn list_invitations(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<InvitationDto>>> {
    let list = invitations::list(&state.db).await?;
    Ok(Json(list.into_iter().map(InvitationDto::from).collect()))
}

/// Create an invitation link (administrators only).
#[utoipa::path(
    post,
    path = "",
    operation_id = "createInvitation",
    tag = "invitations",
    request_body = CreateInvitationRequest,
    responses(
        (status = CREATED, description = "Invitation created", body = CreatedInvitationDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid input", body = ErrorBody),
    )
)]
async fn create_invitation(
    AdminUser(admin): AdminUser,
    State(state): State<AppState>,
    Json(body): Json<CreateInvitationRequest>,
) -> ApiResult<(StatusCode, Json<CreatedInvitationDto>)> {
    validate::age_rating(body.max_age_rating)?;
    let days = body.valid_days.unwrap_or(7);
    if !(1..=30).contains(&days) {
        return Err(ApiError::validation(
            "Eine Einladung ist 1 bis 30 Tage gültig.",
        ));
    }
    let note = body
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    if note.is_some_and(|n| n.chars().count() > 200) {
        return Err(ApiError::validation("Die Notiz ist zu lang."));
    }

    let code = auth::new_token();
    let invitation = invitations::create(
        &state.db,
        &NewInvitation {
            code_hash: &code.hash,
            note,
            max_age_rating: body.max_age_rating,
            created_by: admin.user_id,
            expires_at: flixnet_db::now_ms() + days * DAY_MS,
        },
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedInvitationDto {
            invitation: invitation.into(),
            code: code.token,
        }),
    ))
}

/// Withdraw an invitation (administrators only).
#[utoipa::path(
    delete,
    path = "/{id}",
    operation_id = "deleteInvitation",
    tag = "invitations",
    params(("id" = i64, Path, description = "Invitation id")),
    responses(
        (status = NO_CONTENT, description = "Invitation deleted"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such invitation", body = ErrorBody),
    )
)]
async fn delete_invitation(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    if invitations::delete(&state.db, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("Einladung nicht gefunden."))
    }
}

/// Is this invitation code still usable?
#[utoipa::path(
    get,
    path = "/redeem/{code}",
    operation_id = "getInvitationStatus",
    tag = "invitations",
    security(()),
    params(("code" = String, Path, description = "Invitation code from the link")),
    responses((status = OK, description = "Whether the code can be redeemed", body = InvitationStatus))
)]
async fn invitation_status(
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> ApiResult<Json<InvitationStatus>> {
    let valid = invitations::find_valid(&state.db, &auth::hash_token(&code))
        .await?
        .is_some();
    Ok(Json(InvitationStatus { valid }))
}

/// Redeem an invitation: create the own account and sign in.
#[utoipa::path(
    post,
    path = "/redeem/{code}",
    operation_id = "redeemInvitation",
    tag = "invitations",
    security(()),
    params(("code" = String, Path, description = "Invitation code from the link")),
    request_body = Credentials,
    responses(
        (status = OK, description = "Account created and signed in", body = SessionDto),
        (status = NOT_FOUND, description = "Unknown, expired or used invitation", body = ErrorBody),
        (status = CONFLICT, description = "Name is taken", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid input", body = ErrorBody),
    )
)]
async fn redeem_invitation(
    State(state): State<AppState>,
    Path(code): Path<String>,
    Json(body): Json<Credentials>,
) -> ApiResult<Json<SessionDto>> {
    let name = validate::name(&body.name)?;
    validate::password(&body.password)?;
    validate_device(&body.device)?;

    let hash = hash_secret(body.password.clone()).await?;
    let user = invitations::accept(&state.db, &auth::hash_token(&code), &name, &hash)
        .await
        .map_err(|err| match err {
            AcceptError::Invalid => ApiError::new(
                StatusCode::NOT_FOUND,
                "invitation_invalid",
                "Diese Einladung ist ungültig, abgelaufen oder wurde bereits verwendet.",
            ),
            AcceptError::NameTaken => {
                ApiError::conflict("name_taken", "Dieser Name ist bereits vergeben.")
            }
            AcceptError::Db(err) => err.into(),
        })?;
    tracing::info!(user = %user.name, "invitation redeemed");
    issue_session(&state, user, &body.device).await.map(Json)
}

impl From<invitations::Invitation> for InvitationDto {
    fn from(i: invitations::Invitation) -> Self {
        Self {
            id: i.id,
            note: i.note,
            max_age_rating: i.max_age_rating,
            created_at: i.created_at,
            expires_at: i.expires_at,
            used_at: i.used_at,
            used_by: i.used_by,
        }
    }
}
