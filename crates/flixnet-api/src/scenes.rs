//! `/api/v1/scenes/search`: find a scene by what is said in it ("die Szene, wo sie
//! am Strand streiten") and jump there. Works without any LLM (FTS5 over subtitle
//! and Whisper chunks); local embeddings, when available, add semantic hits.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Query, State};
use flixnet_db::browse::ItemFilter;
use flixnet_db::transcripts::{self, ChunkHit};
use flixnet_llm::semantic::rrf;
use flixnet_llm::text;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AuthUser;
use crate::browse::{ItemCardDto, ensure_visible};
use crate::collections::{cards, viewer_filter};
use crate::error::{ApiResult, ErrorBody};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(search_scenes))
}

/// Hits closer than this in the same item count as one scene (other languages,
/// neighbouring chunks).
const SAME_SCENE_MS: i64 = 45_000;
const SNIPPET_CHARS: usize = 180;

/// A scene hit, visible to the viewer.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneHit {
    pub item_id: i64,
    pub file_id: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub snippet: String,
}

/// Scene search within the viewer's restrictions (`filter`), best first.
pub(crate) async fn search(
    state: &AppState,
    filter: &ItemFilter,
    query: &str,
    item_id: Option<i64>,
    limit: usize,
) -> ApiResult<Vec<SceneHit>> {
    let terms = text::scene_terms(query);
    let fts_hits: Vec<ChunkHit> = match text::scene_fts_query(query) {
        Some(fts) => {
            transcripts::search(&state.db, &fts, filter, item_id, (limit * 4) as i64).await?
        }
        None => Vec::new(),
    };
    let mut hits = fts_hits;
    if let Some(semantic) = &state.semantic
        && !query.trim().is_empty()
    {
        match semantic.search_chunks(query, limit * 4).await {
            Ok(found) => {
                let ids: Vec<i64> = found.into_iter().map(|(id, _)| id).collect();
                let mut by_id: HashMap<i64, ChunkHit> =
                    hits.drain(..).map(|h| (h.chunk_id, h)).collect();
                let order = rrf(
                    &[by_id.keys().copied().collect::<Vec<_>>(), ids.clone()],
                    60.0,
                );
                // Semantic chunks still pass the viewer's restrictions.
                for hit in transcripts::visible_chunks(&state.db, &ids, filter).await? {
                    if item_id.is_none_or(|id| hit.item_id == id) {
                        by_id.entry(hit.chunk_id).or_insert(hit);
                    }
                }
                hits = order
                    .into_iter()
                    .filter_map(|id| by_id.remove(&id))
                    .collect();
            }
            Err(err) => tracing::warn!(%err, "semantic scene search failed"),
        }
    }
    let mut out: Vec<SceneHit> = Vec::new();
    for hit in hits {
        if out
            .iter()
            .any(|o| o.item_id == hit.item_id && (o.start_ms - hit.start_ms).abs() < SAME_SCENE_MS)
        {
            continue;
        }
        out.push(SceneHit {
            item_id: hit.item_id,
            file_id: hit.file_id,
            start_ms: hit.start_ms,
            end_ms: hit.end_ms,
            snippet: text::snippet(&hit.text, &terms, SNIPPET_CHARS),
        });
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SceneSearchQuery {
    /// What is said or happens in the scene.
    pub q: Option<String>,
    /// Only in this movie or episode (series/season: its episodes).
    pub item_id: Option<i64>,
    /// 1–50, default 20.
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SceneHitDto {
    pub item: ItemCardDto,
    pub file_id: i64,
    /// Start of the matching dialogue window (ms).
    pub start_ms: i64,
    pub end_ms: i64,
    /// The dialogue around the match.
    pub snippet: String,
    /// Player link starting at the scene, e.g. `/play/12?start=93000`.
    pub play_url: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SceneSearchDto {
    /// The words searched for (after dropping filler words).
    pub terms: Vec<String>,
    pub hits: Vec<SceneHitDto>,
}

/// Search scenes by their dialogue (subtitles and Whisper transcripts).
#[utoipa::path(
    get,
    path = "/scenes/search",
    operation_id = "searchScenes",
    tag = "assistant",
    params(SceneSearchQuery),
    responses(
        (status = OK, description = "Matching scenes, best first", body = SceneSearchDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) item", body = ErrorBody),
    )
)]
async fn search_scenes(
    user: AuthUser,
    State(state): State<AppState>,
    Query(params): Query<SceneSearchQuery>,
) -> ApiResult<Json<SceneSearchDto>> {
    let query = params.q.as_deref().unwrap_or_default();
    if let Some(id) = params.item_id {
        ensure_visible(&state, &user, id).await?;
    }
    let limit = params.limit.unwrap_or(20).clamp(1, 50) as usize;
    let filter = viewer_filter(&state, &user).await?;
    let hits = search(&state, &filter, query, params.item_id, limit).await?;
    let ids: Vec<i64> = hits.iter().map(|h| h.item_id).collect();
    let mut by_id: HashMap<i64, ItemCardDto> = cards(&state, &user, &ids)
        .await?
        .into_iter()
        .map(|c| (c.id, c))
        .collect();
    let mut out = Vec::new();
    for hit in hits {
        // Cards are moved out; a second hit in the same item gets a fresh copy.
        let item = match by_id.remove(&hit.item_id) {
            Some(card) => card,
            None => match cards(&state, &user, &[hit.item_id]).await?.pop() {
                Some(card) => card,
                None => continue,
            },
        };
        out.push(SceneHitDto {
            play_url: format!("/play/{}?start={}", hit.item_id, hit.start_ms),
            item,
            file_id: hit.file_id,
            start_ms: hit.start_ms,
            end_ms: hit.end_ms,
            snippet: hit.snippet,
        });
    }
    Ok(Json(SceneSearchDto {
        terms: text::scene_terms(query),
        hits: out,
    }))
}
