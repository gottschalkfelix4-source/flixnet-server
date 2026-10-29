//! `/api/v1/users/*`.

use axum::Json;
use axum::extract::State;
use flixnet_db::users;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::session::UserDto;

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(me))
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
