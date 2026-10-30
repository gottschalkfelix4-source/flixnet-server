//! `/api/v1/assistant/*`: the optional LLM features — natural-language search,
//! explanations from the viewer's own history, the chat assistant ("Frag Flixnet")
//! and spoiler-free recaps.
//!
//! The LLM only ever sees what the viewer may see: every query it triggers goes
//! through the browse filters with the viewer's library access and parental controls,
//! and every tool re-checks visibility. Without a configured LLM, search falls back
//! to keyword search, explanations are rule-based and recaps list earlier overviews;
//! only the chat needs the LLM.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use flixnet_db::browse::{self, ItemFilter, ItemSummary, Sort};
use flixnet_db::transcripts::{self, ItemChunk};
use flixnet_db::{collections, items, metadata, playstate, userdata, viewing};
use flixnet_llm::agent::{self, ToolBox};
use flixnet_llm::query::{self, CatalogQuery};
use flixnet_llm::recap::{self, RecapPart};
use flixnet_llm::semantic::rrf;
use flixnet_llm::{LlmClient, Message, ToolSpec, explain, prompts, text};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AuthUser;
use crate::browse::{ItemCardDto, card, ensure_visible};
use crate::collections::{cards, viewer_filter};
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::scenes;

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_status))
        .routes(routes!(natural_search))
        .routes(routes!(explain_item))
        .routes(routes!(chat))
        .routes(routes!(get_recap))
}

/// Requests per chat message (tool rounds plus the answer).
pub const CHAT_MAX_STEPS: usize = 6;
const CHAT_MAX_MESSAGES: usize = 12;
const CHAT_MAX_CHARS: usize = 2000;

pub(crate) async fn llm(state: &AppState) -> ApiResult<Option<LlmClient>> {
    Ok(flixnet_llm::settings::client(&state.db).await?)
}

fn llm_unavailable(err: impl std::fmt::Display) -> ApiError {
    tracing::warn!(%err, "LLM request failed");
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "llm_unavailable",
        "Das Sprachmodell ist gerade nicht erreichbar.",
    )
}

fn current_year() -> i32 {
    // Unix time → year (good enough for "aktuelles Jahr" in the prompt).
    (1970 + flixnet_db::now_ms() / 31_556_952_000) as i32
}

// ---- Status --------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct AssistantStatusDto {
    /// An LLM endpoint is configured and switched on (chat, generated texts).
    pub llm_enabled: bool,
    /// Scene search has indexed text.
    pub scene_search: bool,
    /// Local embeddings are available for semantic search.
    pub semantic: bool,
}

/// Which assistant features are available.
#[utoipa::path(
    get,
    path = "/assistant/status",
    operation_id = "getAssistantStatus",
    tag = "assistant",
    responses(
        (status = OK, description = "Available features", body = AssistantStatusDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn get_status(
    _: AuthUser,
    State(state): State<AppState>,
) -> ApiResult<Json<AssistantStatusDto>> {
    let settings = flixnet_llm::settings::load(&state.db).await?;
    Ok(Json(AssistantStatusDto {
        llm_enabled: settings.is_configured(),
        scene_search: transcripts::stats(&state.db).await?.chunks > 0,
        semantic: state.semantic.is_some(),
    }))
}

// ---- Catalogue queries (natural-language search and the chat tool) ---------------------

/// The filter as understood, shown as chips above the results.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct InterpretedQueryDto {
    pub kinds: Vec<String>,
    pub genres: Vec<String>,
    pub year_from: Option<i64>,
    pub year_to: Option<i64>,
    pub min_rating: Option<f64>,
    pub min_runtime_minutes: Option<i64>,
    pub max_runtime_minutes: Option<i64>,
    /// `false` = only unwatched.
    pub played: Option<bool>,
    pub favorite: Option<bool>,
    /// People found in the library.
    pub people: Vec<String>,
    pub search_text: Option<String>,
    pub sort: Option<String>,
}

impl From<&CatalogQuery> for InterpretedQueryDto {
    fn from(q: &CatalogQuery) -> Self {
        Self {
            kinds: q.kinds.clone(),
            genres: q.genres.clone(),
            year_from: q.year_from,
            year_to: q.year_to,
            min_rating: q.min_rating,
            min_runtime_minutes: q.min_runtime_minutes,
            max_runtime_minutes: q.max_runtime_minutes,
            played: q.played,
            favorite: q.favorite,
            people: Vec::new(),
            search_text: q.search_text.clone(),
            sort: q.sort.clone(),
        }
    }
}

/// Genres the viewer may see (offered to the model).
async fn visible_genres(
    state: &AppState,
    filter: &ItemFilter,
    user: &AuthUser,
) -> ApiResult<Vec<String>> {
    Ok(
        browse::genres(&state.db, &filter.library_ids, &user.restriction)
            .await?
            .into_iter()
            .map(|(name, _)| name)
            .collect(),
    )
}

struct CatalogResult {
    rows: Vec<ItemSummary>,
    total: i64,
    people: Vec<String>,
    unresolved_people: Vec<String>,
}

/// Runs a model-made query within the viewer's restrictions (`base`).
async fn run_query(
    state: &AppState,
    base: &ItemFilter,
    q: &CatalogQuery,
    default_limit: i64,
) -> ApiResult<CatalogResult> {
    let mut f = base.clone();
    f.kinds = if q.kinds.is_empty() {
        vec!["movie".to_owned(), "series".to_owned()]
    } else {
        q.kinds.clone()
    };
    f.genres = q.genres.clone();
    f.year_from = q.year_from;
    f.year_to = q.year_to;
    f.min_rating = q.min_rating;
    f.min_runtime_ms = q.min_runtime_minutes.map(|m| m * 60_000);
    f.max_runtime_ms = q.max_runtime_minutes.map(|m| m * 60_000);
    f.is_played = q.played;
    f.is_favorite = q.favorite;
    let mut people = Vec::new();
    let mut unresolved = Vec::new();
    for name in &q.people {
        let (found, _) = browse::people(&state.db, &base.library_ids, Some(name), 5, 0).await?;
        // Only people credited on something the viewer may see.
        let mut resolved = None;
        for candidate in found {
            let mut probe = base.clone();
            probe.person_ids = vec![candidate.id];
            probe.limit = 1;
            if browse::list(&state.db, &probe).await?.1 > 0 {
                resolved = Some(candidate);
                break;
            }
        }
        match resolved {
            Some(p) => {
                f.person_ids.push(p.id);
                people.push(p.name);
            }
            None => unresolved.push(name.clone()),
        }
    }
    f.search = q.search_text.as_deref().and_then(text::scene_fts_query);
    f.sort = match q.sort.as_deref() {
        Some("title") => Sort::Title,
        Some("year") => Sort::Year,
        Some("rating") => Sort::Rating,
        Some("date_added") => Sort::DateAdded,
        _ if f.search.is_some() => Sort::Relevance,
        // Without keywords: the best rated first.
        _ => Sort::Rating,
    };
    f.descending = q.descending.unwrap_or(matches!(
        f.sort,
        Sort::Rating | Sort::Year | Sort::DateAdded
    ));
    f.limit = q.limit.unwrap_or(default_limit).clamp(1, query::MAX_LIMIT);
    f.offset = 0;
    if !unresolved.is_empty() && f.person_ids.is_empty() {
        // Someone the library does not know: no results rather than everything.
        return Ok(CatalogResult {
            rows: Vec::new(),
            total: 0,
            people,
            unresolved_people: unresolved,
        });
    }
    let (rows, total) = browse::list(&state.db, &f).await?;
    Ok(CatalogResult {
        rows,
        total,
        people,
        unresolved_people: unresolved,
    })
}

/// Keyword search (no LLM): stopwords dropped, any word may match, merged with
/// semantic hits when local embeddings are available.
async fn keyword_search(
    state: &AppState,
    base: &ItemFilter,
    text_query: &str,
    limit: i64,
) -> ApiResult<(Vec<ItemSummary>, i64)> {
    let mut f = base.clone();
    f.kinds = vec![
        "movie".to_owned(),
        "series".to_owned(),
        "episode".to_owned(),
    ];
    f.search = text::scene_fts_query(text_query);
    f.sort = Sort::Relevance;
    f.limit = limit;
    let (rows, total) = if f.search.is_some() {
        browse::list(&state.db, &f).await?
    } else {
        (Vec::new(), 0)
    };
    let Some(semantic) = &state.semantic else {
        return Ok((rows, total));
    };
    let hits = match semantic.search_items(text_query, 50).await {
        Ok(hits) => hits,
        Err(err) => {
            tracing::warn!(%err, "semantic item search failed");
            return Ok((rows, total));
        }
    };
    // A title the keyword search did not find must match the meaning better than the
    // best keyword hit: looking up a name shows that title, not merely similar ones.
    let keyword_ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let keyword_best = hits
        .iter()
        .filter(|(id, _)| keyword_ids.contains(id))
        .map(|(_, score)| *score)
        .fold(None, |best: Option<f32>, s| {
            Some(best.map_or(s, |b| b.max(s)))
        });
    let semantic_ids: Vec<i64> = hits
        .into_iter()
        .filter(|(id, score)| {
            keyword_ids.contains(id)
                || (keyword_ids.is_empty() || keyword_best.is_some_and(|k| *score > k))
        })
        .map(|(id, _)| id)
        .collect();
    let merged = rrf(&[rows.iter().map(|r| r.id).collect(), semantic_ids], 60.0);
    if merged.is_empty() {
        return Ok((rows, total));
    }
    // Semantic candidates still pass the viewer's restrictions.
    let mut g = base.clone();
    g.kinds = f.kinds.clone();
    g.ids = merged.clone();
    g.limit = merged.len() as i64;
    let (visible, _) = browse::list(&state.db, &g).await?;
    let mut by_id: HashMap<i64, ItemSummary> = visible.into_iter().map(|r| (r.id, r)).collect();
    let ordered: Vec<ItemSummary> = merged.iter().filter_map(|id| by_id.remove(id)).collect();
    let total = ordered.len() as i64;
    Ok((ordered.into_iter().take(limit as usize).collect(), total))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct NaturalSearchQuery {
    /// What the viewer wants, in their own words.
    pub q: Option<String>,
    /// 1–50, default 24.
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NaturalSearchDto {
    /// `llm` (filter from the language model) or `keyword` (fallback).
    pub mode: String,
    /// The filter the model chose (`llm` mode).
    pub interpreted: Option<InterpretedQueryDto>,
    pub items: Vec<ItemCardDto>,
    pub total: i64,
    /// Why the fallback was used, or what could not be applied.
    pub notice: Option<String>,
}

/// Natural-language search ("lustige Filme aus den 90ern unter 2 Stunden, die ich
/// noch nicht gesehen habe"). With an LLM the text becomes a browse filter; without
/// one (or when it fails) a keyword search runs instead.
#[utoipa::path(
    get,
    path = "/assistant/search",
    operation_id = "naturalSearch",
    tag = "assistant",
    params(NaturalSearchQuery),
    responses(
        (status = OK, description = "Results", body = NaturalSearchDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn natural_search(
    user: AuthUser,
    State(state): State<AppState>,
    Query(params): Query<NaturalSearchQuery>,
) -> ApiResult<Json<NaturalSearchDto>> {
    let text_query = params.q.as_deref().map(str::trim).unwrap_or_default();
    let limit = params.limit.unwrap_or(24).clamp(1, query::MAX_LIMIT);
    let empty = |mode: &str| NaturalSearchDto {
        mode: mode.to_owned(),
        interpreted: None,
        items: Vec::new(),
        total: 0,
        notice: None,
    };
    if text_query.is_empty() {
        return Ok(Json(empty("keyword")));
    }
    let base = viewer_filter(&state, &user).await?;
    let mut notice = None;
    if let Some(client) = llm(&state).await? {
        let genres = visible_genres(&state, &base, &user).await?;
        match query::natural_query(&client, text_query, &genres, current_year()).await {
            Ok(q) if !q.is_empty() => {
                let result = run_query(&state, &base, &q, limit).await?;
                let mut interpreted = InterpretedQueryDto::from(&q);
                interpreted.people = result.people;
                if !result.unresolved_people.is_empty() {
                    notice = Some(format!(
                        "Nicht in der Bibliothek: {}",
                        result.unresolved_people.join(", ")
                    ));
                }
                return Ok(Json(NaturalSearchDto {
                    mode: "llm".to_owned(),
                    interpreted: Some(interpreted),
                    items: result.rows.into_iter().map(|r| card(&state, r)).collect(),
                    total: result.total,
                    notice,
                }));
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(%err, "natural search fell back to keywords");
                notice = Some(
                    "Das Sprachmodell hat nicht geantwortet – Ergebnisse der Stichwortsuche."
                        .to_owned(),
                );
            }
        }
    }
    let (rows, total) = keyword_search(&state, &base, text_query, limit).await?;
    Ok(Json(NaturalSearchDto {
        mode: "keyword".to_owned(),
        interpreted: None,
        items: rows.into_iter().map(|r| card(&state, r)).collect(),
        total,
        notice,
    }))
}

// ---- Explanations ----------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct ExplanationDto {
    /// "Weil du „X“ und „Y“ gesehen hast …"; `null` without related history.
    pub text: Option<String>,
    /// Phrased by the LLM (else rule-based).
    pub generated: bool,
    /// Titles of the viewer's history the explanation is based on.
    pub based_on: Vec<String>,
}

/// Why a title fits the viewer, based only on their own (visible) history.
#[utoipa::path(
    get,
    path = "/assistant/explain/{id}",
    operation_id = "explainItem",
    tag = "assistant",
    params(("id" = i64, Path, description = "Item id (movie, series or episode)")),
    responses(
        (status = OK, description = "The explanation", body = ExplanationDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) item", body = ErrorBody),
    )
)]
async fn explain_item(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<ExplanationDto>> {
    ensure_visible(&state, &user, id).await?;
    let mut item = items::get(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
    // Episodes and seasons are explained by their series.
    if let Some(series) = item.series_id.filter(|_| item.kind != "series") {
        ensure_visible(&state, &user, series).await?;
        item = items::get(&state.db, series)
            .await?
            .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
    }
    let details = items::details(&state.db, item.id).await?;
    let genres = metadata::genres(&state.db, item.id).await?;
    let people: HashMap<i64, String> = viewing::people_of(&state.db, item.id, 15)
        .await?
        .into_iter()
        .collect();
    let target_collections: HashSet<i64> = collections::of_item(&state.db, item.id)
        .await?
        .into_iter()
        .collect();
    let target = explain::Target {
        title: item.title.clone(),
        year: item.year,
        kind: item.kind.clone(),
        genres: genres.clone(),
        overview: details.and_then(|d| d.overview),
    };

    let filter = viewer_filter(&state, &user).await?;
    let mut entries = Vec::new();
    for h in viewing::recent(&state.db, user.user_id, &filter, 30).await? {
        if h.id == item.id {
            continue;
        }
        let their_genres = metadata::genres(&state.db, h.id).await?;
        let shared_genres: Vec<String> = their_genres
            .into_iter()
            .filter(|g| genres.contains(g))
            .collect();
        let shared_people: Vec<String> = viewing::people_of(&state.db, h.id, 15)
            .await?
            .into_iter()
            .filter(|(pid, _)| people.contains_key(pid))
            .map(|(_, name)| name)
            .take(3)
            .collect();
        let same_collection = collections::of_item(&state.db, h.id)
            .await?
            .iter()
            .any(|c| target_collections.contains(c));
        entries.push(explain::HistoryEntry {
            title: h.title,
            year: h.year,
            favorite: h.favorite,
            shared_genres,
            shared_people,
            same_collection,
        });
    }
    entries.retain(|e| e.weight() > 0);
    entries.sort_by_key(|e| std::cmp::Reverse(e.weight()));
    entries.truncate(8);
    let client = llm(&state).await?;
    let explanation = explain::explain(client.as_ref(), &target, &entries).await;
    Ok(Json(ExplanationDto {
        generated: explanation.as_ref().is_some_and(|e| e.generated),
        text: explanation.map(|e| e.text),
        based_on: entries.into_iter().map(|e| e.title).collect(),
    }))
}

// ---- Recaps ----------------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RecapQuery {
    /// Current position in the item (ms); default: the viewer's resume point.
    pub position_ms: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RecapDto {
    /// `llm` (generated), `overviews` (earlier overviews, no LLM) or `empty`.
    pub mode: String,
    pub text: Option<String>,
    /// The position the recap stops at (ms in the item).
    pub position_ms: i64,
    /// The parts it covers ("Staffel 1, Folge 2: …").
    pub covers: Vec<String>,
}

fn episode_label(e: &ItemSummary) -> String {
    match (e.parent_index_number, e.index_number) {
        (Some(s), Some(n)) => format!("Staffel {s}, Folge {n}: {}", e.title),
        _ => e.title.clone(),
    }
}

fn clock(ms: i64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn dialogue(chunks: Vec<ItemChunk>) -> Vec<(i64, String)> {
    chunks.into_iter().map(|c| (c.start_ms, c.text)).collect()
}

/// Earlier episodes whose overview goes into a recap, and of those, how many also
/// contribute dialogue.
const RECAP_OVERVIEWS: usize = 6;
const RECAP_DIALOGUE_EPISODES: usize = 2;

/// "Was bisher geschah": a spoiler-free recap up to the viewer's position. Only
/// material from before the position is used: earlier episodes of the series and
/// the current item's dialogue up to the position — never anything after it.
#[utoipa::path(
    get,
    path = "/assistant/recap/{id}",
    operation_id = "getRecap",
    tag = "assistant",
    params(("id" = i64, Path, description = "Episode or movie id"), RecapQuery),
    responses(
        (status = OK, description = "The recap", body = RecapDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) item", body = ErrorBody),
        (status = BAD_GATEWAY, description = "The language model failed", body = ErrorBody),
    )
)]
async fn get_recap(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(params): Query<RecapQuery>,
) -> ApiResult<Json<RecapDto>> {
    ensure_visible(&state, &user, id).await?;
    let item = items::get(&state.db, id)
        .await?
        .filter(|i| i.kind == "episode" || i.kind == "movie")
        .ok_or_else(|| ApiError::not_found("Zusammenfassungen gibt es für Filme und Folgen."))?;
    let position = match params.position_ms {
        Some(p) => p.max(0),
        None => {
            userdata::get(&state.db, user.user_id, id)
                .await?
                .position_ms
        }
    };
    let mut parts = Vec::new();
    let mut title = item.title.clone();
    if let Some(series_id) = item.series_id.filter(|_| item.kind == "episode") {
        let mut f = viewer_filter(&state, &user).await?;
        f.series_id = Some(series_id);
        f.kinds = vec!["episode".to_owned()];
        f.sort = Sort::Index;
        f.limit = 2000;
        let (episodes, _) = browse::list(&state.db, &f).await?;
        if let Some(series) = items::get(&state.db, series_id).await? {
            title = series.title;
        }
        let current = episodes.iter().position(|e| e.id == id);
        let earlier: Vec<&ItemSummary> = match current {
            Some(index) => episodes[..index]
                .iter()
                // Specials only count for specials.
                .filter(|e| item.parent_index_number == Some(0) || e.parent_index_number != Some(0))
                .collect(),
            None => Vec::new(),
        };
        let start = earlier.len().saturating_sub(RECAP_OVERVIEWS);
        let dialogue_from = earlier.len().saturating_sub(RECAP_DIALOGUE_EPISODES);
        for (offset, episode) in earlier[start..].iter().enumerate() {
            let with_dialogue = start + offset >= dialogue_from;
            parts.push(RecapPart {
                label: episode_label(episode),
                overview: episode.overview.clone(),
                dialogue: if with_dialogue {
                    dialogue(transcripts::item_chunks(&state.db, episode.id, None).await?)
                } else {
                    Vec::new()
                },
            });
        }
    }
    let current_dialogue = dialogue(transcripts::item_chunks(&state.db, id, Some(position)).await?);
    if !current_dialogue.is_empty() {
        let label = match items::get(&state.db, id).await? {
            Some(i) if i.kind == "episode" => format!(
                "{} (bis {})",
                match (i.parent_index_number, i.index_number) {
                    (Some(s), Some(n)) => format!("Staffel {s}, Folge {n}: {}", i.title),
                    _ => i.title.clone(),
                },
                clock(position)
            ),
            _ => format!("{} (bis {})", item.title, clock(position)),
        };
        // No overview for the current item: it may tell how it ends.
        parts.push(RecapPart {
            label,
            overview: None,
            dialogue: current_dialogue,
        });
    }
    let covers: Vec<String> = parts.iter().map(|p| p.label.clone()).collect();
    if parts.is_empty() {
        return Ok(Json(RecapDto {
            mode: "empty".to_owned(),
            text: None,
            position_ms: position,
            covers,
        }));
    }
    let (mode, text) = match llm(&state).await? {
        Some(client) => (
            "llm",
            Some(
                recap::generate(&client, &title, &parts)
                    .await
                    .map_err(llm_unavailable)?,
            ),
        ),
        None => match recap::fallback(&parts) {
            Some(text) => ("overviews", Some(text)),
            None => ("empty", None),
        },
    };
    Ok(Json(RecapDto {
        mode: mode.to_owned(),
        text,
        position_ms: position,
        covers,
    }))
}

// ---- Chat --------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ChatMessageDto {
    pub role: ChatRole,
    pub content: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ChatRequest {
    /// The conversation so far, oldest first, ending with the user's message.
    pub messages: Vec<ChatMessageDto>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ChatActionDto {
    /// `play`
    pub kind: String,
    pub item_id: i64,
    pub title: String,
    /// Start position in ms (scene hits), else from the resume point.
    pub start_ms: Option<i64>,
    /// Player link, e.g. `/play/12?start=93000`.
    pub url: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ChatResponseDto {
    pub reply: String,
    /// Playback links the assistant offers.
    pub actions: Vec<ChatActionDto>,
    /// Titles the tools looked at (for cards under the answer).
    pub items: Vec<ItemCardDto>,
    /// Requests made to the model.
    pub steps: usize,
    /// The step limit was reached.
    pub hit_limit: bool,
}

/// The assistant's tools, bound to one viewer.
struct ViewerTools {
    state: AppState,
    user: AuthUser,
    filter: ItemFilter,
    genres: Vec<String>,
    actions: Mutex<Vec<ChatActionDto>>,
    seen: Mutex<Vec<i64>>,
}

fn minutes(ms: Option<i64>) -> Option<i64> {
    ms.map(|m| (m + 30_000) / 60_000)
}

fn summary_json(s: &ItemSummary) -> Value {
    json!({
        "id": s.id,
        "kind": s.kind,
        "title": s.title,
        "series": s.series_title,
        "season": s.parent_index_number,
        "episode": s.index_number,
        "year": s.year,
        "runtime_minutes": minutes(s.runtime_ms),
        "rating": s.community_rating.map(|r| (r * 10.0).round() / 10.0),
        "watched": s.played,
    })
}

impl ViewerTools {
    fn remember(&self, ids: impl IntoIterator<Item = i64>) {
        let mut seen = self.seen.lock().unwrap();
        for id in ids {
            if !seen.contains(&id) && seen.len() < 12 {
                seen.push(id);
            }
        }
    }

    async fn visible(&self, id: i64) -> ApiResult<bool> {
        Ok(browse::is_visible(
            &self.state.db,
            id,
            &self.filter.library_ids,
            &self.user.restriction,
        )
        .await?)
    }

    async fn search_catalog(&self, args: Value) -> ApiResult<Value> {
        let q = query::parse_query(&args, &self.genres).map_err(ApiError::validation)?;
        let result = run_query(&self.state, &self.filter, &q, 10).await?;
        self.remember(result.rows.iter().map(|r| r.id));
        Ok(json!({
            "total": result.total,
            "items": result.rows.iter().map(summary_json).collect::<Vec<_>>(),
            "unknown_people": result.unresolved_people,
        }))
    }

    async fn item_details(&self, args: Value) -> ApiResult<Value> {
        let id = args["item_id"]
            .as_i64()
            .ok_or_else(|| ApiError::validation("item_id fehlt."))?;
        if !self.visible(id).await? {
            return Ok(json!({ "error": "Nicht gefunden." }));
        }
        let Some(summary) = browse::summaries(&self.state.db, &[id], Some(self.user.user_id))
            .await?
            .pop()
        else {
            return Ok(json!({ "error": "Nicht gefunden." }));
        };
        self.remember([id]);
        let mut out = summary_json(&summary);
        out["overview"] = json!(
            summary
                .overview
                .as_deref()
                .map(|o| o.chars().take(600).collect::<String>())
        );
        out["genres"] = json!(metadata::genres(&self.state.db, id).await?);
        out["people"] = json!(
            viewing::people_of(&self.state.db, id, 6)
                .await?
                .into_iter()
                .map(|(_, name)| name)
                .collect::<Vec<_>>()
        );
        out["resume_minutes"] =
            json!((summary.position_ms > 0).then_some(summary.position_ms / 60_000));
        if summary.kind == "series" {
            out["seasons"] = json!(summary.child_count);
        }
        Ok(out)
    }

    async fn watch_history(&self) -> ApiResult<Value> {
        let visibility = playstate::Visibility {
            library_ids: self.filter.library_ids.clone(),
            restriction: self.user.restriction.clone(),
        };
        let db = &self.state.db;
        let resume = playstate::resume_items(db, self.user.user_id, &visibility, 8).await?;
        let since = flixnet_db::now_ms() - playstate::NEXT_UP_MAX_AGE_MS;
        let next = playstate::next_up(db, self.user.user_id, &visibility, since, 8).await?;
        let recent = viewing::recent(db, self.user.user_id, &self.filter, 10).await?;
        let resume_rows = browse::summaries(db, &resume, Some(self.user.user_id)).await?;
        let next_rows = browse::summaries(db, &next, Some(self.user.user_id)).await?;
        self.remember(resume.iter().chain(&next).copied());
        Ok(json!({
            "continue_watching": resume_rows.iter().map(|s| {
                let mut v = summary_json(s);
                v["resume_minutes"] = json!(s.position_ms / 60_000);
                v
            }).collect::<Vec<_>>(),
            "next_up": next_rows.iter().map(summary_json).collect::<Vec<_>>(),
            "recently_watched": recent.iter().map(|h| json!({
                "id": h.id, "kind": h.kind, "title": h.title, "year": h.year,
                "favorite": h.favorite, "watched": h.played,
            })).collect::<Vec<_>>(),
        }))
    }

    async fn scene_search(&self, args: Value) -> ApiResult<Value> {
        let text_query = args["query"].as_str().unwrap_or_default();
        let item = args["item_id"].as_i64();
        if let Some(id) = item
            && !self.visible(id).await?
        {
            return Ok(json!({ "error": "Nicht gefunden." }));
        }
        let hits = scenes::search(&self.state, &self.filter, text_query, item, 5).await?;
        self.remember(hits.iter().map(|h| h.item_id));
        let summaries = browse::summaries(
            &self.state.db,
            &hits.iter().map(|h| h.item_id).collect::<Vec<_>>(),
            None,
        )
        .await?;
        Ok(json!({
            "hits": hits.iter().map(|h| {
                let s = summaries.iter().find(|s| s.id == h.item_id);
                json!({
                    "item_id": h.item_id,
                    "title": s.map(|s| s.title.clone()),
                    "series": s.and_then(|s| s.series_title.clone()),
                    "start_ms": h.start_ms,
                    "time": clock(h.start_ms),
                    "text": h.snippet,
                })
            }).collect::<Vec<_>>()
        }))
    }

    async fn play(&self, args: Value) -> ApiResult<Value> {
        let id = args["item_id"]
            .as_i64()
            .ok_or_else(|| ApiError::validation("item_id fehlt."))?;
        if !self.visible(id).await? {
            return Ok(json!({ "error": "Nicht gefunden." }));
        }
        let Some(item) = items::get(&self.state.db, id).await? else {
            return Ok(json!({ "error": "Nicht gefunden." }));
        };
        // Series play their next episode.
        let target = if item.kind == "series" || item.kind == "season" {
            let series = item.series_id.unwrap_or(item.id);
            match playstate::series_next(&self.state.db, self.user.user_id, series).await? {
                Some(next) => Some(next.episode_id),
                None => playstate::first_episode(&self.state.db, series).await?,
            }
        } else {
            Some(item.id)
        };
        let Some(target) = target else {
            return Ok(json!({ "error": "Keine Folgen vorhanden." }));
        };
        if !self.visible(target).await? {
            return Ok(json!({ "error": "Nicht gefunden." }));
        }
        let start_ms = args["start_ms"].as_i64().filter(|s| *s >= 0);
        let url = match start_ms {
            Some(start) => format!("/play/{target}?start={start}"),
            None => format!("/play/{target}"),
        };
        let title = items::get(&self.state.db, target)
            .await?
            .map_or(item.title.clone(), |i| i.title);
        let mut actions = self.actions.lock().unwrap();
        if !actions.iter().any(|a| a.url == url) {
            actions.push(ChatActionDto {
                kind: "play".to_owned(),
                item_id: target,
                title,
                start_ms,
                url: url.clone(),
            });
        }
        Ok(json!({ "ok": true, "url": url }))
    }
}

impl ToolBox for ViewerTools {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: "search_catalog".to_owned(),
                description: "Sucht Filme, Serien oder Folgen in der Bibliothek nach Filtern."
                    .to_owned(),
                parameters: query::schema(&self.genres),
            },
            ToolSpec {
                name: "get_item_details".to_owned(),
                description: "Details zu einem Titel (Inhalt, Genres, Personen, gesehen?)."
                    .to_owned(),
                parameters: json!({"type": "object", "properties": {"item_id": {"type": "integer"}}, "required": ["item_id"]}),
            },
            ToolSpec {
                name: "get_watch_history".to_owned(),
                description: "Weiterschauen, nächste Folgen und zuletzt Gesehenes des Nutzers."
                    .to_owned(),
                parameters: json!({"type": "object", "properties": {}}),
            },
            ToolSpec {
                name: "search_scenes".to_owned(),
                description: "Sucht Szenen über die Dialoge (Untertitel), optional in einem Titel."
                    .to_owned(),
                parameters: json!({"type": "object", "properties": {
                    "query": {"type": "string", "description": "Worte aus dem Dialog oder eine Beschreibung"},
                    "item_id": {"type": "integer"}
                }, "required": ["query"]}),
            },
            ToolSpec {
                name: "play_item".to_owned(),
                description:
                    "Bietet dem Nutzer einen Abspiel-Link an (optional ab einer Stelle in ms)."
                        .to_owned(),
                parameters: json!({"type": "object", "properties": {
                    "item_id": {"type": "integer"}, "start_ms": {"type": "integer", "minimum": 0}
                }, "required": ["item_id"]}),
            },
        ]
    }

    async fn call(&self, name: &str, args: Value) -> Value {
        let result = match name {
            "search_catalog" => self.search_catalog(args).await,
            "get_item_details" => self.item_details(args).await,
            "get_watch_history" => self.watch_history().await,
            "search_scenes" => self.scene_search(args).await,
            "play_item" => self.play(args).await,
            other => Ok(json!({ "error": format!("Unbekanntes Werkzeug {other}.") })),
        };
        result.unwrap_or_else(|err| json!({ "error": err.message() }))
    }
}

/// "Frag Flixnet": one turn of the chat assistant (needs a configured LLM). The
/// conversation is kept by the client; only user and assistant messages are taken.
#[utoipa::path(
    post,
    path = "/assistant/chat",
    operation_id = "assistantChat",
    tag = "assistant",
    request_body = ChatRequest,
    responses(
        (status = OK, description = "The assistant's answer", body = ChatResponseDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = CONFLICT, description = "No LLM configured", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "No user message", body = ErrorBody),
        (status = BAD_GATEWAY, description = "The language model failed", body = ErrorBody),
    )
)]
async fn chat(
    user: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<ChatRequest>,
) -> ApiResult<Json<ChatResponseDto>> {
    let Some(client) = llm(&state).await? else {
        return Err(ApiError::conflict(
            "llm_not_configured",
            "Für den Assistenten muss ein Sprachmodell eingerichtet sein.",
        ));
    };
    if body
        .messages
        .last()
        .is_none_or(|m| m.role != ChatRole::User || m.content.trim().is_empty())
    {
        return Err(ApiError::validation("Bitte eine Frage stellen."));
    }
    let start = body.messages.len().saturating_sub(CHAT_MAX_MESSAGES);
    let mut messages = vec![Message::system(prompts::chat(&user.name, CHAT_MAX_STEPS))];
    for m in &body.messages[start..] {
        let content: String = m.content.chars().take(CHAT_MAX_CHARS).collect();
        messages.push(match m.role {
            ChatRole::User => Message::user(content),
            ChatRole::Assistant => Message::assistant(content),
        });
    }
    let filter = viewer_filter(&state, &user).await?;
    let genres = visible_genres(&state, &filter, &user).await?;
    let tools = ViewerTools {
        state: state.clone(),
        user: user.clone(),
        filter,
        genres,
        actions: Mutex::default(),
        seen: Mutex::default(),
    };
    let outcome = agent::run(&client, messages, &tools, CHAT_MAX_STEPS)
        .await
        .map_err(llm_unavailable)?;
    let seen = tools.seen.lock().unwrap().clone();
    let actions = tools.actions.lock().unwrap().clone();
    Ok(Json(ChatResponseDto {
        reply: outcome.reply,
        actions,
        items: cards(&state, &user, &seen).await?,
        steps: outcome.steps,
        hit_limit: outcome.hit_limit,
    }))
}
