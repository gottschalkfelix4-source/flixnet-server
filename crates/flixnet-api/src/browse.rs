//! Browsing: `/api/v1/items` (grids, search), `/items/letters`, `/items/{id}`, `/genres`.
//!
//! Every query is limited to the libraries the user may see and to their age limit.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use flixnet_db::browse::{self, ItemFilter, ItemSummary, Sort};
use flixnet_db::{items, libraries, media, metadata, userdata};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult, ErrorBody};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_items))
        .routes(routes!(letters))
        .routes(routes!(get_item))
        .routes(routes!(list_genres))
        .routes(routes!(mark_played, mark_unplayed))
        .routes(routes!(add_favorite, remove_favorite))
        .routes(routes!(get_person))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PersonCreditDto {
    pub item: ItemCardDto,
    pub role: String,
    pub character: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PersonDetailDto {
    pub id: i64,
    pub name: String,
    pub image_url: Option<String>,
    pub tmdb_id: Option<i64>,
    /// Everything in the visible libraries this person worked on.
    pub credits: Vec<PersonCreditDto>,
}

/// A person and their movies/series in the library.
#[utoipa::path(
    get,
    path = "/people/{id}",
    operation_id = "getPerson",
    tag = "items",
    params(("id" = i64, Path, description = "Person id")),
    responses(
        (status = OK, description = "The person", body = PersonDetailDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such person", body = ErrorBody),
    )
)]
async fn get_person(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<PersonDetailDto>> {
    let person = metadata::person(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Person nicht gefunden."))?;
    let library_ids = visible_libraries(&state, &user, None).await?;
    let mut credits = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (item_id, role, character) in metadata::person_credits(&state.db, id).await? {
        // One entry per item (an actor-director appears once, as actor first).
        if !seen.insert(item_id)
            || !browse::is_visible(&state.db, item_id, &library_ids, user.max_age_rating).await?
        {
            continue;
        }
        if let Some(summary) = browse::summaries(&state.db, &[item_id], Some(user.user_id))
            .await?
            .pop()
        {
            credits.push(PersonCreditDto {
                item: card(&state, summary),
                role,
                character,
            });
        }
    }
    Ok(Json(PersonDetailDto {
        id: person.id,
        name: person.name,
        image_url: person
            .image_url
            .map(|u| u.replace("/t/p/original/", "/t/p/w342/")),
        tmdb_id: person.tmdb_id,
        credits,
    }))
}

async fn ensure_visible(state: &AppState, user: &AuthUser, id: i64) -> ApiResult<()> {
    let library_ids = visible_libraries(state, user, None).await?;
    if browse::is_visible(&state.db, id, &library_ids, user.max_age_rating).await? {
        Ok(())
    } else {
        Err(ApiError::not_found("Eintrag nicht gefunden."))
    }
}

/// Mark as watched (series/seasons: all their episodes).
#[utoipa::path(
    put,
    path = "/items/{id}/played",
    operation_id = "markPlayed",
    tag = "items",
    params(("id" = i64, Path, description = "Item id")),
    responses(
        (status = NO_CONTENT, description = "Marked as watched"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) item", body = ErrorBody),
    )
)]
async fn mark_played(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<axum::http::StatusCode> {
    ensure_visible(&state, &user, id).await?;
    userdata::set_played(&state.db, user.user_id, id, true).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Mark as not watched.
#[utoipa::path(
    delete,
    path = "/items/{id}/played",
    operation_id = "markUnplayed",
    tag = "items",
    params(("id" = i64, Path, description = "Item id")),
    responses(
        (status = NO_CONTENT, description = "Marked as not watched"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) item", body = ErrorBody),
    )
)]
async fn mark_unplayed(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<axum::http::StatusCode> {
    ensure_visible(&state, &user, id).await?;
    userdata::set_played(&state.db, user.user_id, id, false).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Add to favorites.
#[utoipa::path(
    put,
    path = "/items/{id}/favorite",
    operation_id = "addFavorite",
    tag = "items",
    params(("id" = i64, Path, description = "Item id")),
    responses(
        (status = NO_CONTENT, description = "Added"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) item", body = ErrorBody),
    )
)]
async fn add_favorite(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<axum::http::StatusCode> {
    ensure_visible(&state, &user, id).await?;
    userdata::set_favorite(&state.db, user.user_id, id, true).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Remove from favorites.
#[utoipa::path(
    delete,
    path = "/items/{id}/favorite",
    operation_id = "removeFavorite",
    tag = "items",
    params(("id" = i64, Path, description = "Item id")),
    responses(
        (status = NO_CONTENT, description = "Removed"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) item", body = ErrorBody),
    )
)]
async fn remove_favorite(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<axum::http::StatusCode> {
    ensure_visible(&state, &user, id).await?;
    userdata::set_favorite(&state.db, user.user_id, id, false).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[derive(Debug, Clone, Copy, Default, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SortField {
    #[default]
    Title,
    DateAdded,
    Year,
    Rating,
    PremiereDate,
    Relevance,
    /// Season/episode number; the default when listing children (`parent_id`/`series_id`).
    Index,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    #[default]
    Asc,
    Desc,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ItemsQuery {
    pub library_id: Option<i64>,
    /// Comma-separated: `movie`, `series`, `season`, `episode`.
    pub kind: Option<String>,
    pub parent_id: Option<i64>,
    pub series_id: Option<i64>,
    /// Free-text search over titles and descriptions.
    pub search: Option<String>,
    pub genre: Option<String>,
    pub year_from: Option<i64>,
    pub year_to: Option<i64>,
    /// `A`–`Z` or `#`.
    pub letter: Option<String>,
    #[param(inline)]
    pub sort: Option<SortField>,
    #[param(inline)]
    pub order: Option<SortOrder>,
    /// Only watched (`true`) or unwatched (`false`) items.
    pub played: Option<bool>,
    /// Only favorites.
    pub favorite: Option<bool>,
    /// 1–200, default 60.
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// Enough to draw a poster card.
#[derive(Debug, Serialize, ToSchema)]
pub struct ItemCardDto {
    pub id: i64,
    pub library_id: i64,
    pub kind: String,
    pub parent_id: Option<i64>,
    pub series_id: Option<i64>,
    pub title: String,
    pub year: Option<i64>,
    pub index_number: Option<i64>,
    pub index_end: Option<i64>,
    pub parent_index_number: Option<i64>,
    pub premiere_date: Option<String>,
    pub runtime_ms: Option<i64>,
    pub community_rating: Option<f64>,
    pub official_rating: Option<String>,
    /// Short description (episode lists show it).
    pub overview: Option<String>,
    pub date_added: i64,
    /// Signed poster URL (episodes: still image), 342 px wide.
    pub poster_url: Option<String>,
    /// Blurhash placeholder for the poster.
    pub poster_blurhash: Option<String>,
    /// Signed backdrop URL, 1280 px wide.
    pub backdrop_url: Option<String>,
    /// Seasons of a series / episodes of a season.
    pub child_count: i64,
    /// Watched by the signed-in user.
    pub played: bool,
    pub favorite: bool,
    /// Resume position in milliseconds (0 = from the start).
    pub position_ms: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ItemPage {
    pub items: Vec<ItemCardDto>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LetterDto {
    /// `A`–`Z` or `#`.
    pub letter: String,
    pub count: i64,
    /// Offset of the first item with this letter in title order.
    pub offset: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GenreDto {
    pub name: String,
    pub count: i64,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct GenresQuery {
    pub library_id: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PersonDto {
    pub person_id: i64,
    pub name: String,
    /// actor | director | writer | creator | producer
    pub role: String,
    pub character: Option<String>,
    /// Profile photo (provider URL, small size).
    pub image_url: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ArtworkDto {
    pub poster_url: Option<String>,
    pub poster_blurhash: Option<String>,
    pub backdrop_url: Option<String>,
    pub backdrop_blurhash: Option<String>,
    pub logo_url: Option<String>,
    pub thumb_url: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VideoStreamDto {
    pub codec: Option<String>,
    pub profile: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub fps: Option<f64>,
    pub bit_depth: Option<i64>,
    /// sdr | hdr10 | hlg | dovi
    pub hdr: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AudioStreamDto {
    pub index: i64,
    pub codec: Option<String>,
    pub channels: Option<i64>,
    pub channel_layout: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_default: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SubtitleStreamDto {
    pub index: i64,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_forced: bool,
    pub is_sdh: bool,
    pub is_default: bool,
    pub is_external: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ChapterDto {
    pub start_ms: i64,
    pub end_ms: i64,
    pub title: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MediaFileDto {
    pub id: i64,
    /// main | extra
    pub role: String,
    pub extra_kind: Option<String>,
    pub part: Option<i64>,
    pub container: Option<String>,
    pub duration_ms: Option<i64>,
    pub bitrate: Option<i64>,
    pub size: i64,
    /// File name (without folders).
    pub file_name: String,
    pub video: Option<VideoStreamDto>,
    pub audio: Vec<AudioStreamDto>,
    pub subtitles: Vec<SubtitleStreamDto>,
    pub chapters: Vec<ChapterDto>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ItemDetailDto {
    #[serde(flatten)]
    pub card: ItemCardDto,
    pub original_title: Option<String>,
    pub tagline: Option<String>,
    pub age_rating: Option<i64>,
    pub genres: Vec<String>,
    pub studios: Vec<String>,
    pub people: Vec<PersonDto>,
    pub artwork: ArtworkDto,
    pub files: Vec<MediaFileDto>,
    /// provider → id, e.g. `{"tmdb": "603", "imdb": "tt0133093"}`.
    pub provider_ids: BTreeMap<String, String>,
    /// unmatched | matched | manual | failed (administrators only).
    pub match_state: Option<String>,
    pub series_title: Option<String>,
    pub season_title: Option<String>,
}

/// Libraries the user may see, optionally narrowed to one.
async fn visible_libraries(
    state: &AppState,
    user: &AuthUser,
    only: Option<i64>,
) -> ApiResult<Vec<i64>> {
    let all = if user.is_admin {
        libraries::list(&state.db).await?
    } else {
        libraries::list_for_user(&state.db, user.user_id).await?
    };
    Ok(all
        .into_iter()
        .map(|l| l.id)
        .filter(|id| only.is_none_or(|o| o == *id))
        .collect())
}

fn filter_from(
    query: &ItemsQuery,
    library_ids: Vec<i64>,
    user: &AuthUser,
) -> ApiResult<ItemFilter> {
    let kinds = query
        .kind
        .as_deref()
        .map(|k| {
            k.split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(bad) = kinds
        .iter()
        .find(|k| !matches!(k.as_str(), "movie" | "series" | "season" | "episode"))
    {
        return Err(ApiError::validation(format!("Unbekannter Typ „{bad}“.")));
    }
    let letter = match query.letter.as_deref() {
        None | Some("") => None,
        Some(l) => {
            let c = l.chars().next().unwrap_or('#');
            if l.chars().count() != 1 || !(c.is_ascii_alphabetic() || c == '#') {
                return Err(ApiError::validation("Buchstabe A–Z oder # angeben."));
            }
            Some(c.to_ascii_lowercase())
        }
    };
    let search = query
        .search
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| browse::fts_query(s).unwrap_or_else(|| "\"\"".to_owned()));
    let default_sort = if search.is_some() {
        SortField::Relevance
    } else if query.parent_id.is_some() || query.series_id.is_some() {
        SortField::Index
    } else {
        SortField::Title
    };
    let sort = match query.sort.unwrap_or(default_sort) {
        SortField::Title => Sort::Title,
        SortField::DateAdded => Sort::DateAdded,
        SortField::Year => Sort::Year,
        SortField::Rating => Sort::Rating,
        SortField::PremiereDate => Sort::PremiereDate,
        SortField::Relevance => Sort::Relevance,
        SortField::Index => Sort::Index,
    };
    Ok(ItemFilter {
        library_ids,
        kinds,
        parent_id: query.parent_id,
        series_id: query.series_id,
        search,
        genre: query.genre.clone().filter(|g| !g.is_empty()),
        year_from: query.year_from,
        year_to: query.year_to,
        letter,
        max_age: user.max_age_rating,
        user_id: Some(user.user_id),
        is_played: query.played,
        is_favorite: query.favorite,
        sort,
        descending: matches!(query.order, Some(SortOrder::Desc)),
        limit: query.limit.unwrap_or(60).clamp(1, 200),
        offset: query.offset.unwrap_or(0).max(0),
    })
}

fn card(state: &AppState, s: ItemSummary) -> ItemCardDto {
    ItemCardDto {
        poster_url: s.poster_id.map(|id| state.signer.image_url(id, Some(342))),
        poster_blurhash: s.poster_blurhash,
        backdrop_url: s
            .backdrop_id
            .map(|id| state.signer.image_url(id, Some(1280))),
        id: s.id,
        library_id: s.library_id,
        kind: s.kind,
        parent_id: s.parent_id,
        series_id: s.series_id,
        title: s.title,
        year: s.year,
        index_number: s.index_number,
        index_end: s.index_end,
        parent_index_number: s.parent_index_number,
        premiere_date: s.premiere_date,
        runtime_ms: s.runtime_ms,
        community_rating: s.community_rating,
        official_rating: s.official_rating,
        overview: s.overview,
        date_added: s.date_added,
        child_count: s.child_count,
        played: s.played,
        favorite: s.favorite,
        position_ms: s.position_ms,
    }
}

/// Browse and search items.
#[utoipa::path(
    get,
    path = "/items",
    operation_id = "listItems",
    tag = "items",
    params(ItemsQuery),
    responses(
        (status = OK, description = "One page of items", body = ItemPage),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid filter", body = ErrorBody),
    )
)]
async fn list_items(
    user: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<ItemsQuery>,
) -> ApiResult<Json<ItemPage>> {
    let library_ids = visible_libraries(&state, &user, query.library_id).await?;
    let filter = filter_from(&query, library_ids, &user)?;
    let (rows, total) = browse::list(&state.db, &filter).await?;
    Ok(Json(ItemPage {
        items: rows.into_iter().map(|r| card(&state, r)).collect(),
        total,
        offset: filter.offset,
        limit: filter.limit,
    }))
}

/// Jump bar: items per first letter (title order) with the offset to jump to.
#[utoipa::path(
    get,
    path = "/items/letters",
    operation_id = "listItemLetters",
    tag = "items",
    params(ItemsQuery),
    responses(
        (status = OK, description = "Letters in title order", body = Vec<LetterDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid filter", body = ErrorBody),
    )
)]
async fn letters(
    user: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<ItemsQuery>,
) -> ApiResult<Json<Vec<LetterDto>>> {
    let library_ids = visible_libraries(&state, &user, query.library_id).await?;
    let filter = filter_from(&query, library_ids, &user)?;
    let mut offset = 0;
    let out = browse::letters(&state.db, &filter)
        .await?
        .into_iter()
        .map(|(letter, count)| {
            let dto = LetterDto {
                letter: letter.to_ascii_uppercase().to_string(),
                count,
                offset,
            };
            offset += count;
            dto
        })
        .collect();
    Ok(Json(out))
}

/// Genres used in the visible libraries.
#[utoipa::path(
    get,
    path = "/genres",
    operation_id = "listGenres",
    tag = "items",
    params(GenresQuery),
    responses(
        (status = OK, description = "Genres with item counts", body = Vec<GenreDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn list_genres(
    user: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<GenresQuery>,
) -> ApiResult<Json<Vec<GenreDto>>> {
    let library_ids = visible_libraries(&state, &user, query.library_id).await?;
    let genres = browse::genres(&state.db, &library_ids).await?;
    Ok(Json(
        genres
            .into_iter()
            .map(|(name, count)| GenreDto { name, count })
            .collect(),
    ))
}

/// Everything about one item: metadata, people, artwork and files.
#[utoipa::path(
    get,
    path = "/items/{id}",
    operation_id = "getItem",
    tag = "items",
    params(("id" = i64, Path, description = "Item id")),
    responses(
        (status = OK, description = "The item", body = ItemDetailDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) item", body = ErrorBody),
    )
)]
async fn get_item(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<ItemDetailDto>> {
    let library_ids = visible_libraries(&state, &user, None).await?;
    if !browse::is_visible(&state.db, id, &library_ids, user.max_age_rating).await? {
        return Err(ApiError::not_found("Eintrag nicht gefunden."));
    }
    let details = items::details(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
    let summary = browse::summaries(&state.db, &[id], Some(user.user_id))
        .await?
        .pop()
        .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;

    let images = metadata::images(&state.db, id).await?;
    let best = |kind: &str| images.iter().find(|i| i.kind == kind);
    let artwork = ArtworkDto {
        poster_url: best("poster").map(|i| state.signer.image_url(i.id, Some(500))),
        poster_blurhash: best("poster").and_then(|i| i.blurhash.clone()),
        backdrop_url: best("backdrop").map(|i| state.signer.image_url(i.id, Some(1920))),
        backdrop_blurhash: best("backdrop").and_then(|i| i.blurhash.clone()),
        logo_url: best("logo").map(|i| state.signer.image_url(i.id, Some(780))),
        thumb_url: best("thumb").map(|i| state.signer.image_url(i.id, Some(780))),
    };

    let people = metadata::credits(&state.db, id)
        .await?
        .into_iter()
        .map(|c| PersonDto {
            person_id: c.person_id,
            name: c.name,
            role: c.role,
            character: c.character,
            image_url: c
                .image_url
                .map(|u| u.replace("/t/p/original/", "/t/p/w185/")),
        })
        .collect();

    let mut files = Vec::new();
    for file in media::files_for_item(&state.db, id).await? {
        let streams = media::streams(&state.db, file.id).await?;
        let chapters = media::chapters(&state.db, file.id).await?;
        files.push(MediaFileDto {
            file_name: std::path::Path::new(&file.path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            video: streams
                .iter()
                .find(|s| s.kind == "video")
                .map(|s| VideoStreamDto {
                    codec: s.codec.clone(),
                    profile: s.profile.clone(),
                    width: s.width,
                    height: s.height,
                    fps: s.fps,
                    bit_depth: s.bit_depth,
                    hdr: s.hdr.clone(),
                }),
            audio: streams
                .iter()
                .filter(|s| s.kind == "audio")
                .map(|s| AudioStreamDto {
                    index: s.idx,
                    codec: s.codec.clone(),
                    channels: s.channels,
                    channel_layout: s.channel_layout.clone(),
                    language: s.language.clone(),
                    title: s.title.clone(),
                    is_default: s.is_default,
                })
                .collect(),
            subtitles: streams
                .iter()
                .filter(|s| s.kind == "subtitle")
                .map(|s| SubtitleStreamDto {
                    index: s.idx,
                    codec: s.codec.clone(),
                    language: s.language.clone(),
                    title: s.title.clone(),
                    is_forced: s.is_forced,
                    is_sdh: s.is_sdh,
                    is_default: s.is_default,
                    is_external: s.is_external,
                })
                .collect(),
            chapters: chapters
                .into_iter()
                .map(|c| ChapterDto {
                    start_ms: c.start_ms,
                    end_ms: c.end_ms,
                    title: c.title,
                })
                .collect(),
            id: file.id,
            role: file.role,
            extra_kind: file.extra_kind,
            part: file.part,
            container: file.container,
            duration_ms: file.duration_ms,
            bitrate: file.bitrate,
            size: file.size,
        });
    }

    let title_of = |id: Option<i64>| {
        let db = state.db.clone();
        async move {
            match id {
                Some(id) => items::get(&db, id).await.map(|i| i.map(|i| i.title)),
                None => Ok(None),
            }
        }
    };
    let series_title = title_of(details.series_id.filter(|s| *s != id)).await?;
    let season_title = if details.kind == "episode" {
        title_of(details.parent_id).await?
    } else {
        None
    };

    Ok(Json(ItemDetailDto {
        card: card(&state, summary),
        original_title: details.original_title,
        tagline: details.tagline,
        age_rating: details.age_rating,
        genres: metadata::genres(&state.db, id).await?,
        studios: metadata::studios(&state.db, id).await?,
        people,
        artwork,
        files,
        provider_ids: items::provider_ids(&state.db, id)
            .await?
            .into_iter()
            .collect(),
        match_state: user.is_admin.then_some(details.match_state),
        series_title,
        season_title,
    }))
}
