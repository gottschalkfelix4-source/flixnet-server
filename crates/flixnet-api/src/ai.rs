//! AI features: `/recommendations*` ("Für dich", home rows, onboarding, group mode),
//! `/items/{id}/similar` and the admin pages `/settings/ai`, `/ai/*`.
//!
//! Everything a profile gets is limited to its libraries and parental controls; the
//! group mode uses the intersection of all members' libraries and the strictest
//! combination of their restrictions (the caller is always a member).

use std::collections::HashSet;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use flixnet_ai::recommend::{self, Viewer};
use flixnet_ai::settings::{AiSettings, EmbedderKind};
use flixnet_core::jobs::priority;
use flixnet_db::{browse, jobs, users};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::{AdminUser, AuthUser};
use crate::browse::{ItemCardDto, card, ensure_visible, libraries_of, visible_libraries};
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::validate;

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_recommendations))
        .routes(routes!(get_recommendation_rows))
        .routes(routes!(get_onboarding, save_onboarding))
        .routes(routes!(get_group_recommendations))
        .routes(routes!(list_similar_items))
        .routes(routes!(get_ai_settings, update_ai_settings))
        .routes(routes!(get_ai_status))
        .routes(routes!(download_ai_model, delete_ai_model))
        .routes(routes!(start_embedding))
}

async fn viewer(state: &AppState, user: &AuthUser) -> ApiResult<Viewer> {
    Ok(Viewer {
        user_id: user.user_id,
        name: user.name.clone(),
        library_ids: visible_libraries(state, user, None).await?,
        restriction: user.restriction.clone(),
    })
}

/// Cards for item ids, in order (the viewer's watched/favorite state included).
async fn cards(state: &AppState, user_id: i64, ids: &[i64]) -> ApiResult<Vec<ItemCardDto>> {
    Ok(browse::summaries_ordered(&state.db, ids, Some(user_id))
        .await?
        .into_iter()
        .map(|s| card(state, s))
        .collect())
}

fn ai_error(err: anyhow::Error) -> ApiError {
    ApiError::internal(format!("{err:#}"))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RecommendedItemDto {
    pub item: ItemCardDto,
    /// Rule-based explanation, e.g. „Weil du „Alien“ gesehen hast“.
    pub reason: String,
    /// The title the explanation refers to.
    pub because_item_id: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RecommendationsDto {
    /// Semantic data exists (AI enabled and the library embedded).
    pub available: bool,
    /// No history and no onboarding picks yet: show the onboarding.
    pub needs_onboarding: bool,
    pub items: Vec<RecommendedItemDto>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LimitQuery {
    /// 1–100, default 40.
    pub limit: Option<i64>,
}

fn limit(query: &LimitQuery, default: i64) -> usize {
    query.limit.unwrap_or(default).clamp(1, 100) as usize
}

/// Personal recommendations ("Für dich").
#[utoipa::path(
    get,
    path = "/recommendations",
    operation_id = "getRecommendations",
    tag = "recommendations",
    params(LimitQuery),
    responses(
        (status = OK, description = "Recommendations for the signed-in profile", body = RecommendationsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn get_recommendations(
    user: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<LimitQuery>,
) -> ApiResult<Json<RecommendationsDto>> {
    let viewer = viewer(&state, &user).await?;
    let list = recommend::for_viewer(&state.ai, &viewer, limit(&query, 40))
        .await
        .map_err(ai_error)?;
    let ids: Vec<i64> = list.items.iter().map(|r| r.item_id).collect();
    let mut cards = cards(&state, user.user_id, &ids).await?;
    let mut items = Vec::new();
    for rec in list.items {
        if let Some(pos) = cards.iter().position(|c| c.id == rec.item_id) {
            items.push(RecommendedItemDto {
                item: cards.remove(pos),
                reason: rec.reason,
                because_item_id: rec.because_item_id,
            });
        }
    }
    Ok(Json(RecommendationsDto {
        available: list.available,
        needs_onboarding: list.needs_onboarding,
        items,
    }))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RecommendationRowDto {
    /// for_you | because
    pub kind: String,
    /// Row title, e.g. „Weil du „Dark“ gesehen hast“.
    pub title: String,
    /// For `because` rows: the title the row is based on.
    pub source_item_id: Option<i64>,
    pub items: Vec<ItemCardDto>,
}

/// Recommendation rows for the home page: "Für dich" and "Weil du X gesehen hast".
#[utoipa::path(
    get,
    path = "/recommendations/rows",
    operation_id = "getRecommendationRows",
    tag = "recommendations",
    responses(
        (status = OK, description = "Rows (empty without history or AI)", body = Vec<RecommendationRowDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn get_recommendation_rows(
    user: AuthUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<RecommendationRowDto>>> {
    let viewer = viewer(&state, &user).await?;
    let mut rows = Vec::new();
    let for_you = recommend::for_viewer(&state.ai, &viewer, 20)
        .await
        .map_err(ai_error)?;
    if !for_you.items.is_empty() {
        let ids: Vec<i64> = for_you.items.iter().map(|r| r.item_id).collect();
        rows.push(RecommendationRowDto {
            kind: "for_you".into(),
            title: "Für dich".into(),
            source_item_id: None,
            items: cards(&state, user.user_id, &ids).await?,
        });
    }
    for row in recommend::because_rows(&state.ai, &viewer, 2, 16)
        .await
        .map_err(ai_error)?
    {
        rows.push(RecommendationRowDto {
            kind: "because".into(),
            title: format!("Weil du „{}“ gesehen hast", row.title),
            source_item_id: Some(row.source_item_id),
            items: cards(&state, user.user_id, &row.items).await?,
        });
    }
    Ok(Json(rows))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OnboardingDto {
    /// Well-rated titles from across the library to pick from.
    pub items: Vec<ItemCardDto>,
    /// Titles picked earlier.
    pub selected: Vec<i64>,
}

/// Titles to choose from when a profile has no history yet.
#[utoipa::path(
    get,
    path = "/recommendations/onboarding",
    operation_id = "getOnboarding",
    tag = "recommendations",
    responses(
        (status = OK, description = "Candidates and earlier picks", body = OnboardingDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn get_onboarding(
    user: AuthUser,
    State(state): State<AppState>,
) -> ApiResult<Json<OnboardingDto>> {
    let viewer = viewer(&state, &user).await?;
    let ids = recommend::onboarding_candidates(&state.ai, &viewer, 24)
        .await
        .map_err(ai_error)?;
    let mut selected = Vec::new();
    for (id, _) in flixnet_db::ai::taste_seeds(&state.db, user.user_id).await? {
        if browse::is_visible(&state.db, id, &viewer.library_ids, &user.restriction).await? {
            selected.push(id);
        }
    }
    Ok(Json(OnboardingDto {
        items: cards(&state, user.user_id, &ids).await?,
        selected,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct OnboardingRequest {
    /// Liked titles (movies or series), at most 50; an empty list clears the picks.
    pub item_ids: Vec<i64>,
}

/// Store the titles a profile likes (replaces earlier picks).
#[utoipa::path(
    put,
    path = "/recommendations/onboarding",
    operation_id = "saveOnboarding",
    tag = "recommendations",
    request_body = OnboardingRequest,
    responses(
        (status = NO_CONTENT, description = "Saved"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "A title is not visible to this profile", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Too many titles", body = ErrorBody),
    )
)]
async fn save_onboarding(
    user: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<OnboardingRequest>,
) -> ApiResult<StatusCode> {
    if body.item_ids.len() > 50 {
        return Err(ApiError::validation("Höchstens 50 Titel auswählen."));
    }
    let mut ids: Vec<i64> = Vec::new();
    for id in body.item_ids {
        ensure_visible(&state, &user, id).await?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    flixnet_db::ai::set_taste_seeds(&state.db, user.user_id, &ids).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct GroupRequest {
    /// Household profiles watching together (the signed-in profile always counts).
    pub user_ids: Vec<i64>,
    /// Leave out what any member has already watched or started (default: yes).
    #[serde(default = "yes")]
    pub exclude_watched: bool,
    /// 1–50, default 20.
    pub limit: Option<i64>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GroupMemberDto {
    pub id: i64,
    pub name: String,
    /// The profile has history or onboarding picks that shape the suggestions.
    pub has_history: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GroupSuggestionDto {
    pub item: ItemCardDto,
    /// e.g. „Passt zu allen“, „Passt besonders zu Anna“.
    pub reason: String,
    /// Members this title suits especially well.
    pub matches: Vec<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GroupRecommendationsDto {
    pub available: bool,
    pub members: Vec<GroupMemberDto>,
    /// Age limit applied for the group (the lowest of all members), if any.
    pub max_age_rating: Option<i64>,
    pub items: Vec<GroupSuggestionDto>,
}

/// "Was schauen wir heute?": suggestions for several household profiles at once.
#[utoipa::path(
    post,
    path = "/recommendations/group",
    operation_id = "getGroupRecommendations",
    tag = "recommendations",
    request_body = GroupRequest,
    responses(
        (status = OK, description = "Suggestions for the group", body = GroupRecommendationsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown or hidden profile", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Too many profiles", body = ErrorBody),
    )
)]
async fn get_group_recommendations(
    user: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<GroupRequest>,
) -> ApiResult<Json<GroupRecommendationsDto>> {
    let mut ids = vec![user.user_id];
    for id in body.user_ids {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if ids.len() > 8 {
        return Err(ApiError::validation("Höchstens 8 Profile gleichzeitig."));
    }
    let mut members = Vec::new();
    let mut libraries: Option<HashSet<i64>> = None;
    for id in &ids {
        let member = users::get(&state.db, *id)
            .await?
            .filter(|u| !u.is_hidden || u.id == user.user_id)
            .ok_or_else(|| ApiError::not_found("Profil nicht gefunden."))?;
        let visible: HashSet<i64> = libraries_of(&state, member.id, member.is_admin, None)
            .await?
            .into_iter()
            .collect();
        libraries = Some(match libraries {
            Some(current) => current.intersection(&visible).copied().collect(),
            None => visible,
        });
        // The caller's restriction comes from the session (it may be stricter than
        // the stored profile, e.g. right after an admin lowered it).
        let restriction = if member.id == user.user_id {
            user.restriction.clone()
        } else {
            member.restriction()
        };
        members.push(Viewer {
            user_id: member.id,
            name: member.name,
            library_ids: Vec::new(),
            restriction,
        });
    }
    let mut library_ids: Vec<i64> = libraries.unwrap_or_default().into_iter().collect();
    library_ids.sort_unstable();
    let restrictions: Vec<_> = members.iter().map(|m| m.restriction.clone()).collect();
    let combined = recommend::strictest(&restrictions);
    for member in &mut members {
        member.library_ids = library_ids.clone();
    }

    let limit = body.limit.unwrap_or(20).clamp(1, 50) as usize;
    let list = recommend::for_group(
        &state.ai,
        &members,
        &library_ids,
        &combined,
        body.exclude_watched,
        limit,
    )
    .await
    .map_err(ai_error)?;
    let item_ids: Vec<i64> = list.items.iter().map(|s| s.item_id).collect();
    let mut cards = cards(&state, user.user_id, &item_ids).await?;
    let mut items = Vec::new();
    for suggestion in list.items {
        if let Some(pos) = cards.iter().position(|c| c.id == suggestion.item_id) {
            items.push(GroupSuggestionDto {
                item: cards.remove(pos),
                reason: suggestion.reason,
                matches: suggestion.matches,
            });
        }
    }
    Ok(Json(GroupRecommendationsDto {
        available: list.available,
        members: members
            .iter()
            .map(|m| GroupMemberDto {
                id: m.user_id,
                name: m.name.clone(),
                has_history: !list.without_history.contains(&m.user_id),
            })
            .collect(),
        max_age_rating: combined.max_age,
        items,
    }))
}

/// "Mehr wie das": titles similar to an item.
#[utoipa::path(
    get,
    path = "/items/{id}/similar",
    operation_id = "listSimilarItems",
    tag = "items",
    params(("id" = i64, Path, description = "Item id"), LimitQuery),
    responses(
        (status = OK, description = "Similar titles (empty without AI)", body = Vec<ItemCardDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) item", body = ErrorBody),
    )
)]
async fn list_similar_items(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(query): Query<LimitQuery>,
) -> ApiResult<Json<Vec<ItemCardDto>>> {
    ensure_visible(&state, &user, id).await?;
    let viewer = viewer(&state, &user).await?;
    let ids = recommend::similar(&state.ai, &viewer, id, limit(&query, 20))
        .await
        .map_err(ai_error)?;
    Ok(Json(cards(&state, user.user_id, &ids).await?))
}

// ---- Administration -----------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EmbedderChoice {
    /// multilingual-e5-base on this server (downloaded on first use).
    Local,
    /// An OpenAI-compatible endpoint (OpenAI, Ollama, …).
    Openai,
    /// No AI: full-text search only, no recommendations.
    Off,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AiSettingsDto {
    #[schema(inline)]
    pub embedder: EmbedderChoice,
    /// Download the local model automatically on first use.
    pub auto_download: bool,
    pub openai_base_url: Option<String>,
    pub openai_model: Option<String>,
    /// Whether an API key is stored (the key itself is never returned).
    pub has_openai_api_key: bool,
}

fn settings_dto(s: &AiSettings) -> AiSettingsDto {
    AiSettingsDto {
        embedder: match s.embedder {
            EmbedderKind::Local => EmbedderChoice::Local,
            EmbedderKind::Openai => EmbedderChoice::Openai,
            EmbedderKind::Off => EmbedderChoice::Off,
        },
        auto_download: s.auto_download,
        openai_base_url: s.openai_base_url.clone(),
        openai_model: s.openai_model.clone(),
        has_openai_api_key: s.openai_api_key.as_deref().is_some_and(|k| !k.is_empty()),
    }
}

/// AI settings.
#[utoipa::path(
    get,
    path = "/settings/ai",
    operation_id = "getAiSettings",
    tag = "settings",
    responses(
        (status = OK, description = "Current settings", body = AiSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn get_ai_settings(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<AiSettingsDto>> {
    let current = state.ai.settings().await.map_err(ai_error)?;
    Ok(Json(settings_dto(&current)))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct UpdateAiSettings {
    #[schema(inline)]
    pub embedder: Option<EmbedderChoice>,
    pub auto_download: Option<bool>,
    /// e.g. `https://api.openai.com/v1` or `http://192.168.1.10:11434/v1`.
    pub openai_base_url: Option<String>,
    pub openai_model: Option<String>,
    /// `null` removes the stored key.
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub openai_api_key: Option<Option<String>>,
}

/// Change the AI settings. Choosing an endpoint tests it with one embedding request.
#[utoipa::path(
    put,
    path = "/settings/ai",
    operation_id = "updateAiSettings",
    tag = "settings",
    request_body = UpdateAiSettings,
    responses(
        (status = OK, description = "Updated settings", body = AiSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid URL, model or unreachable endpoint", body = ErrorBody),
    )
)]
async fn update_ai_settings(
    _: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<UpdateAiSettings>,
) -> ApiResult<Json<AiSettingsDto>> {
    let before = state.ai.settings().await.map_err(ai_error)?;
    let mut next = before.clone();
    if let Some(choice) = body.embedder {
        next.embedder = match choice {
            EmbedderChoice::Local => EmbedderKind::Local,
            EmbedderChoice::Openai => EmbedderKind::Openai,
            EmbedderChoice::Off => EmbedderKind::Off,
        };
    }
    if let Some(auto) = body.auto_download {
        next.auto_download = auto;
    }
    if let Some(url) = body.openai_base_url {
        let url = url.trim().trim_end_matches('/').to_owned();
        if !url.is_empty() && !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(ApiError::validation(
                "Die Adresse muss mit http:// oder https:// beginnen.",
            ));
        }
        next.openai_base_url = (!url.is_empty()).then_some(url);
    }
    if let Some(model) = body.openai_model {
        let model = model.trim().to_owned();
        next.openai_model = (!model.is_empty()).then_some(model);
    }
    match body.openai_api_key {
        Some(Some(key)) if !key.trim().is_empty() => {
            next.openai_api_key = Some(key.trim().to_owned())
        }
        Some(_) => next.openai_api_key = None,
        None => {}
    }
    if next.embedder == EmbedderKind::Openai {
        let (Some(url), Some(model)) = (&next.openai_base_url, &next.openai_model) else {
            return Err(ApiError::validation(
                "Für einen Endpoint Adresse und Modell angeben.",
            ));
        };
        let probe = flixnet_ai::embed::openai::OpenAiEmbedder::new(
            flixnet_ai::http::client(),
            url,
            next.openai_api_key.clone(),
            model,
        );
        if let Err(err) = probe.embed(&["Flixnet".to_owned()]).await {
            tracing::warn!(%err, "embedding endpoint test failed");
            return Err(ApiError::validation(format!(
                "Der Endpoint liefert keine Embeddings: {err:#}"
            )));
        }
    }
    state.ai.save_settings(&next).await.map_err(ai_error)?;
    let changed_model = before.embedder != next.embedder
        || (next.embedder == EmbedderKind::Openai
            && (before.openai_model != next.openai_model
                || before.openai_base_url != next.openai_base_url));
    if changed_model && next.embedder != EmbedderKind::Off {
        flixnet_ai::jobs::queue_embedding(state.jobs.queue(), priority::NEW, None, false).await?;
    }
    Ok(Json(settings_dto(&next)))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AiFileDto {
    pub label: String,
    /// Bytes (0 = unknown).
    pub size: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AiJobDto {
    pub id: i64,
    /// queued | running
    pub state: String,
    pub progress: Option<f64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AiStatusDto {
    /// local | openai | off | stub
    pub embedder: String,
    pub model_key: Option<String>,
    /// Vectors can be computed now.
    pub ready: bool,
    /// Files the local model still needs.
    pub missing: Vec<AiFileDto>,
    /// The local model can run on this platform.
    pub platform_supported: bool,
    /// Titles with a vector of the current model.
    pub embedded: i64,
    /// Movies and series in total.
    pub total: i64,
    pub last_error: Option<String>,
    /// Running or queued model download.
    pub download: Option<AiJobDto>,
    /// Running or queued embedding run.
    pub embedding: Option<AiJobDto>,
}

/// State of the AI: model, downloads, how much is embedded.
#[utoipa::path(
    get,
    path = "/ai/status",
    operation_id = "getAiStatus",
    tag = "settings",
    responses(
        (status = OK, description = "AI status", body = AiStatusDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn get_ai_status(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<AiStatusDto>> {
    let status = state.ai.status().await.map_err(ai_error)?;
    let active = jobs::list(&state.db, None, 200).await?;
    let job = |kind: &str| {
        active
            .iter()
            .find(|j| j.kind == kind && matches!(j.state.as_str(), "queued" | "running"))
            .map(|j| AiJobDto {
                id: j.id,
                state: j.state.clone(),
                progress: j.progress,
            })
    };
    Ok(Json(AiStatusDto {
        embedder: status.embedder.to_owned(),
        model_key: status.model_key,
        ready: status.ready,
        missing: status
            .missing
            .into_iter()
            .map(|(label, size)| AiFileDto {
                label: label.to_owned(),
                size,
            })
            .collect(),
        platform_supported: status.platform_supported,
        embedded: status.embedded,
        total: status.total,
        last_error: status.last_error,
        download: job(<flixnet_ai::jobs::DownloadModel as flixnet_core::jobs::JobHandler>::KIND),
        embedding: job(<flixnet_ai::jobs::EmbedItems as flixnet_core::jobs::JobHandler>::KIND),
    }))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct QueuedJobDto {
    /// `null` when an equal job is already queued or running.
    pub job_id: Option<i64>,
}

/// Download the local model now (progress via the job list).
#[utoipa::path(
    post,
    path = "/ai/model",
    operation_id = "downloadAiModel",
    tag = "settings",
    responses(
        (status = ACCEPTED, description = "Download queued", body = QueuedJobDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "No ONNX Runtime for this platform", body = ErrorBody),
    )
)]
async fn download_ai_model(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<(StatusCode, Json<QueuedJobDto>)> {
    if !state.ai.models().platform_supported() {
        return Err(ApiError::validation(
            "Für diese Plattform gibt es kein ONNX Runtime; ai.onnxruntime_path setzen.",
        ));
    }
    let job_id =
        flixnet_ai::jobs::queue_download(state.jobs.queue(), priority::INTERACTIVE).await?;
    Ok((StatusCode::ACCEPTED, Json(QueuedJobDto { job_id })))
}

/// Delete the downloaded model files (frees the disk space).
#[utoipa::path(
    delete,
    path = "/ai/model",
    operation_id = "deleteAiModel",
    tag = "settings",
    responses(
        (status = NO_CONTENT, description = "Deleted"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn delete_ai_model(_: AdminUser, State(state): State<AppState>) -> ApiResult<StatusCode> {
    state.ai.forget_embedder().await;
    state.ai.models().remove_all().await.map_err(ai_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct EmbeddingRequest {
    /// Recompute every vector instead of only new and changed titles.
    #[serde(default)]
    pub rebuild: bool,
}

/// Compute embeddings now.
#[utoipa::path(
    post,
    path = "/ai/embeddings",
    operation_id = "startEmbedding",
    tag = "settings",
    request_body = EmbeddingRequest,
    responses(
        (status = ACCEPTED, description = "Embedding run queued", body = QueuedJobDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn start_embedding(
    _: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<EmbeddingRequest>,
) -> ApiResult<(StatusCode, Json<QueuedJobDto>)> {
    let job_id = flixnet_ai::jobs::queue_embedding(
        state.jobs.queue(),
        priority::INTERACTIVE,
        None,
        body.rebuild,
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(QueuedJobDto { job_id })))
}
