//! `/api/v1/playlists`: personal playlists and smart playlists.
//!
//! A playlist belongs to the user who created it. It is private (other users get 404)
//! unless shared, which makes it visible read-only to every profile of the household.
//! Entries reference items by identity key, so they survive rescans and renames; an
//! entry whose item is gone (or not visible to the viewer) is skipped, not deleted.
//! Smart playlists store a filter and are evaluated live through the browse queries.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use flixnet_db::browse::{self, ItemFilter, Sort};
use flixnet_db::items;
use flixnet_db::playlists::{self, Playlist};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AuthUser;
use crate::browse::{ItemCardDto, ensure_visible};
use crate::collections::{AddedDto, cards, viewer_filter};
use crate::error::{ApiError, ApiResult, ErrorBody};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_playlists, create_playlist))
        .routes(routes!(get_playlist, update_playlist, delete_playlist))
        .routes(routes!(add_playlist_items))
        .routes(routes!(remove_playlist_entry))
        .routes(routes!(reorder_playlist))
}

/// Default and maximum number of items a smart playlist shows.
const SMART_DEFAULT_LIMIT: i64 = 100;
const SMART_MAX_LIMIT: i64 = 500;
const MAX_ENTRIES: i64 = 5000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SmartSort {
    #[default]
    Title,
    DateAdded,
    Year,
    Rating,
    PremiereDate,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SmartOrder {
    #[default]
    Asc,
    Desc,
}

/// Saved filter of a smart playlist (the fields of `GET /items`). Empty lists and
/// missing values do not filter.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct SmartFilterDto {
    /// `movie`, `series`, `season`, `episode`; empty = movies and series.
    pub kinds: Vec<String>,
    /// Any of these genres.
    pub genres: Vec<String>,
    /// Only these libraries (still limited to what the viewer may see).
    pub library_ids: Vec<i64>,
    pub year_from: Option<i64>,
    pub year_to: Option<i64>,
    /// Watched (`true`) or unwatched (`false`) by the viewer.
    pub played: Option<bool>,
    /// Only the viewer's favorites.
    pub favorite: Option<bool>,
    /// Minimum community rating, 0–10.
    pub min_rating: Option<f64>,
    pub sort: SmartSort,
    pub order: SmartOrder,
    /// Maximum number of items, 1–500 (default 100).
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PlaylistDto {
    pub id: i64,
    pub name: String,
    /// Visible to every profile of the household (read-only for them).
    pub shared: bool,
    pub smart: bool,
    /// The filter of a smart playlist.
    pub smart_filter: Option<SmartFilterDto>,
    pub owner_id: i64,
    pub owner_name: String,
    /// The signed-in user owns (and may edit) the playlist.
    pub is_owner: bool,
    /// Items the signed-in user may see.
    pub item_count: i64,
    /// Up to four signed poster URLs of the first items (for a collage).
    pub poster_urls: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PlaylistEntryDto {
    /// Id of the entry (for removing and reordering); `null` in smart playlists.
    pub entry_id: Option<i64>,
    pub item: ItemCardDto,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PlaylistDetailDto {
    #[serde(flatten)]
    pub playlist: PlaylistDto,
    pub entries: Vec<PlaylistEntryDto>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreatePlaylistRequest {
    pub name: String,
    #[serde(default)]
    pub shared: bool,
    /// Makes it a smart playlist.
    pub smart_filter: Option<SmartFilterDto>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdatePlaylistRequest {
    pub name: Option<String>,
    pub shared: Option<bool>,
    /// New filter (smart playlists only).
    pub smart_filter: Option<SmartFilterDto>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PlaylistItemsRequest {
    /// Movies and episodes; series and seasons are added as all their episodes.
    pub item_ids: Vec<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PlaylistOrderRequest {
    /// Entry ids in the new order. Entries not listed keep their relative order after them.
    pub entry_ids: Vec<i64>,
}

fn playlist_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(ApiError::validation(
            "Der Name muss zwischen 1 und 100 Zeichen lang sein.",
        ));
    }
    Ok(name.to_owned())
}

fn check_filter(filter: &SmartFilterDto) -> ApiResult<String> {
    if let Some(bad) = filter
        .kinds
        .iter()
        .find(|k| !matches!(k.as_str(), "movie" | "series" | "season" | "episode"))
    {
        return Err(ApiError::validation(format!("Unbekannter Typ „{bad}“.")));
    }
    if filter
        .limit
        .is_some_and(|l| !(1..=SMART_MAX_LIMIT).contains(&l))
    {
        return Err(ApiError::validation(format!(
            "Das Limit muss zwischen 1 und {SMART_MAX_LIMIT} liegen."
        )));
    }
    if filter
        .min_rating
        .is_some_and(|r| !(0.0..=10.0).contains(&r))
    {
        return Err(ApiError::validation(
            "Die Mindestbewertung muss zwischen 0 und 10 liegen.",
        ));
    }
    if let (Some(from), Some(to)) = (filter.year_from, filter.year_to)
        && from > to
    {
        return Err(ApiError::validation(
            "Das Startjahr liegt nach dem Endjahr.",
        ));
    }
    if filter.genres.len() > 50 || filter.library_ids.len() > 100 {
        return Err(ApiError::validation("Zu viele Filterwerte."));
    }
    serde_json::to_string(filter).map_err(ApiError::internal)
}

fn parse_filter(playlist: &Playlist) -> Option<SmartFilterDto> {
    let json = playlist.smart_filter_json.as_deref()?;
    // A filter that no longer parses (older format) behaves like an empty one.
    Some(serde_json::from_str(json).unwrap_or_default())
}

/// The smart filter applied for one viewer.
async fn smart_item_filter(
    state: &AppState,
    user: &AuthUser,
    smart: &SmartFilterDto,
    limit: i64,
) -> ApiResult<ItemFilter> {
    let mut filter = viewer_filter(state, user).await?;
    if !smart.library_ids.is_empty() {
        filter
            .library_ids
            .retain(|id| smart.library_ids.contains(id));
    }
    filter.kinds = if smart.kinds.is_empty() {
        vec!["movie".to_owned(), "series".to_owned()]
    } else {
        smart.kinds.clone()
    };
    filter.genres = smart.genres.clone();
    filter.year_from = smart.year_from;
    filter.year_to = smart.year_to;
    filter.is_played = smart.played;
    filter.is_favorite = smart.favorite;
    filter.min_rating = smart.min_rating;
    filter.sort = match smart.sort {
        SmartSort::Title => Sort::Title,
        SmartSort::DateAdded => Sort::DateAdded,
        SmartSort::Year => Sort::Year,
        SmartSort::Rating => Sort::Rating,
        SmartSort::PremiereDate => Sort::PremiereDate,
    };
    filter.descending = smart.order == SmartOrder::Desc;
    filter.limit = limit;
    Ok(filter)
}

/// The playlist's items for this viewer: (entry id, item id), at most `limit`.
async fn contents(
    state: &AppState,
    user: &AuthUser,
    playlist: &Playlist,
    limit: Option<i64>,
) -> ApiResult<(Vec<(Option<i64>, i64)>, i64)> {
    if let Some(smart) = parse_filter(playlist) {
        let cap = smart.limit.unwrap_or(SMART_DEFAULT_LIMIT);
        let filter = smart_item_filter(state, user, &smart, limit.unwrap_or(cap).min(cap)).await?;
        let (rows, total) = browse::list(&state.db, &filter).await?;
        return Ok((
            rows.into_iter().map(|r| (None, r.id)).collect(),
            total.min(cap),
        ));
    }
    let filter = viewer_filter(state, user).await?;
    let resolved = playlists::resolve(&state.db, playlist.id, &filter).await?;
    let total = resolved.len() as i64;
    Ok((
        resolved
            .into_iter()
            .take(limit.map_or(usize::MAX, |l| l as usize))
            .map(|(entry, item)| (Some(entry), item))
            .collect(),
        total,
    ))
}

async fn dto(state: &AppState, user: &AuthUser, playlist: Playlist) -> ApiResult<PlaylistDto> {
    let (first, total) = contents(state, user, &playlist, Some(8)).await?;
    let ids: Vec<i64> = first.iter().map(|(_, id)| *id).collect();
    let poster_urls = cards(state, user, &ids)
        .await?
        .into_iter()
        .filter_map(|c| c.poster_url)
        .take(4)
        .collect();
    Ok(PlaylistDto {
        smart: playlist.smart_filter_json.is_some(),
        smart_filter: parse_filter(&playlist),
        is_owner: playlist.user_id == user.user_id,
        id: playlist.id,
        name: playlist.name,
        shared: playlist.shared,
        owner_id: playlist.user_id,
        owner_name: playlist.owner_name,
        item_count: total,
        poster_urls,
        created_at: playlist.created_at,
        updated_at: playlist.updated_at,
    })
}

/// A playlist the user may see (own or shared); 404 otherwise.
async fn visible(state: &AppState, user: &AuthUser, id: i64) -> ApiResult<Playlist> {
    playlists::get(&state.db, id)
        .await?
        .filter(|p| p.user_id == user.user_id || p.shared)
        .ok_or_else(|| ApiError::not_found("Playlist nicht gefunden."))
}

/// A playlist the user owns; 404 if invisible, 403 if only shared with them.
async fn owned(state: &AppState, user: &AuthUser, id: i64) -> ApiResult<Playlist> {
    let playlist = visible(state, user, id).await?;
    if playlist.user_id != user.user_id {
        return Err(ApiError::forbidden());
    }
    Ok(playlist)
}

/// Own playlists and those shared by other profiles.
#[utoipa::path(
    get,
    path = "/playlists",
    operation_id = "listPlaylists",
    tag = "playlists",
    responses(
        (status = OK, description = "Playlists, own first", body = Vec<PlaylistDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn list_playlists(
    user: AuthUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<PlaylistDto>>> {
    let mut out = Vec::new();
    for playlist in playlists::list_for_user(&state.db, user.user_id).await? {
        out.push(dto(&state, &user, playlist).await?);
    }
    Ok(Json(out))
}

/// Create a playlist (with `smart_filter`: a smart playlist).
#[utoipa::path(
    post,
    path = "/playlists",
    operation_id = "createPlaylist",
    tag = "playlists",
    request_body = CreatePlaylistRequest,
    responses(
        (status = CREATED, description = "Created", body = PlaylistDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid name or filter", body = ErrorBody),
    )
)]
async fn create_playlist(
    user: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<CreatePlaylistRequest>,
) -> ApiResult<(StatusCode, Json<PlaylistDto>)> {
    let name = playlist_name(&body.name)?;
    let json = body.smart_filter.as_ref().map(check_filter).transpose()?;
    let created =
        playlists::create(&state.db, user.user_id, &name, body.shared, json.as_deref()).await?;
    Ok((
        StatusCode::CREATED,
        Json(dto(&state, &user, created).await?),
    ))
}

/// A playlist with its items (smart playlists: evaluated now, for the signed-in user).
#[utoipa::path(
    get,
    path = "/playlists/{id}",
    operation_id = "getPlaylist",
    tag = "playlists",
    params(("id" = i64, Path, description = "Playlist id")),
    responses(
        (status = OK, description = "The playlist", body = PlaylistDetailDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) playlist", body = ErrorBody),
    )
)]
async fn get_playlist(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<PlaylistDetailDto>> {
    let playlist = visible(&state, &user, id).await?;
    let (rows, _) = contents(&state, &user, &playlist, None).await?;
    let ids: Vec<i64> = rows.iter().map(|(_, id)| *id).collect();
    let mut by_id: std::collections::HashMap<i64, ItemCardDto> = cards(&state, &user, &ids)
        .await?
        .into_iter()
        .map(|c| (c.id, c))
        .collect();
    let entries = rows
        .iter()
        .filter_map(|(entry, item)| {
            let card = by_id.remove(item)?;
            Some(PlaylistEntryDto {
                entry_id: *entry,
                item: card,
            })
        })
        .collect();
    Ok(Json(PlaylistDetailDto {
        playlist: dto(&state, &user, playlist).await?,
        entries,
    }))
}

/// Rename, share/unshare, or change a smart playlist's filter (owner only).
#[utoipa::path(
    patch,
    path = "/playlists/{id}",
    operation_id = "updatePlaylist",
    tag = "playlists",
    params(("id" = i64, Path, description = "Playlist id")),
    request_body = UpdatePlaylistRequest,
    responses(
        (status = OK, description = "Updated", body = PlaylistDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Shared by someone else (read-only)", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) playlist", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid name or filter", body = ErrorBody),
    )
)]
async fn update_playlist(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<UpdatePlaylistRequest>,
) -> ApiResult<Json<PlaylistDto>> {
    let playlist = owned(&state, &user, id).await?;
    let name = body.name.as_deref().map(playlist_name).transpose()?;
    let json = match &body.smart_filter {
        Some(_) if playlist.smart_filter_json.is_none() => {
            return Err(ApiError::validation(
                "Nur Smart-Playlists haben einen Filter.",
            ));
        }
        Some(filter) => Some(check_filter(filter)?),
        None => None,
    };
    playlists::update(&state.db, id, name.as_deref(), body.shared, json.as_deref()).await?;
    let updated = owned(&state, &user, id).await?;
    Ok(Json(dto(&state, &user, updated).await?))
}

/// Delete a playlist (owner only).
#[utoipa::path(
    delete,
    path = "/playlists/{id}",
    operation_id = "deletePlaylist",
    tag = "playlists",
    params(("id" = i64, Path, description = "Playlist id")),
    responses(
        (status = NO_CONTENT, description = "Deleted"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Shared by someone else (read-only)", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) playlist", body = ErrorBody),
    )
)]
async fn delete_playlist(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    owned(&state, &user, id).await?;
    playlists::delete(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Episodes (in season/episode order) a series or season expands to for this viewer.
async fn episodes_of(state: &AppState, user: &AuthUser, item: &items::Item) -> ApiResult<Vec<i64>> {
    let mut filter = viewer_filter(state, user).await?;
    filter.kinds = vec!["episode".to_owned()];
    if item.kind == "series" {
        filter.series_id = Some(item.id);
    } else {
        filter.parent_id = Some(item.id);
    }
    filter.sort = Sort::Index;
    filter.limit = MAX_ENTRIES;
    let (rows, _) = browse::list(&state.db, &filter).await?;
    Ok(rows.into_iter().map(|r| r.id).collect())
}

/// Add movies and episodes (series and seasons as all their episodes) at the end
/// (owner only). Items already in the playlist are skipped.
#[utoipa::path(
    post,
    path = "/playlists/{id}/items",
    operation_id = "addPlaylistItems",
    tag = "playlists",
    params(("id" = i64, Path, description = "Playlist id")),
    request_body = PlaylistItemsRequest,
    responses(
        (status = OK, description = "Added", body = AddedDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Shared by someone else (read-only)", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) playlist or item", body = ErrorBody),
        (status = CONFLICT, description = "Smart playlists fill themselves", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "No or too many items", body = ErrorBody),
    )
)]
async fn add_playlist_items(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<PlaylistItemsRequest>,
) -> ApiResult<Json<AddedDto>> {
    let playlist = owned(&state, &user, id).await?;
    if playlist.smart_filter_json.is_some() {
        return Err(ApiError::conflict(
            "smart_playlist",
            "Smart-Playlists füllen sich selbst über ihren Filter.",
        ));
    }
    if body.item_ids.is_empty() || body.item_ids.len() > 500 {
        return Err(ApiError::validation("1 bis 500 Einträge angeben."));
    }
    let mut targets = Vec::new();
    for item_id in &body.item_ids {
        ensure_visible(&state, &user, *item_id).await?;
        let item = items::get(&state.db, *item_id)
            .await?
            .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
        match item.kind.as_str() {
            "series" | "season" => targets.extend(episodes_of(&state, &user, &item).await?),
            _ => targets.push(item.id),
        }
    }
    if playlist.entry_count + targets.len() as i64 > MAX_ENTRIES {
        return Err(ApiError::validation(format!(
            "Eine Playlist fasst höchstens {MAX_ENTRIES} Einträge."
        )));
    }
    let identities: Vec<String> = playlists::item_identities(&state.db, &targets)
        .await?
        .into_iter()
        .map(|(_, identity)| identity)
        .collect();
    let added = playlists::add_entries(&state.db, id, &identities).await?;
    Ok(Json(AddedDto { added }))
}

/// Remove one entry (owner only).
#[utoipa::path(
    delete,
    path = "/playlists/{id}/items/{entry_id}",
    operation_id = "removePlaylistEntry",
    tag = "playlists",
    params(
        ("id" = i64, Path, description = "Playlist id"),
        ("entry_id" = i64, Path, description = "Entry id (from the playlist's entries)"),
    ),
    responses(
        (status = NO_CONTENT, description = "Removed"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Shared by someone else (read-only)", body = ErrorBody),
        (status = NOT_FOUND, description = "No such playlist or entry", body = ErrorBody),
    )
)]
async fn remove_playlist_entry(
    user: AuthUser,
    State(state): State<AppState>,
    Path((id, entry_id)): Path<(i64, i64)>,
) -> ApiResult<StatusCode> {
    owned(&state, &user, id).await?;
    if playlists::remove_entry(&state.db, id, entry_id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("Eintrag nicht gefunden."))
    }
}

/// Reorder entries (owner only).
#[utoipa::path(
    put,
    path = "/playlists/{id}/order",
    operation_id = "reorderPlaylist",
    tag = "playlists",
    params(("id" = i64, Path, description = "Playlist id")),
    request_body = PlaylistOrderRequest,
    responses(
        (status = NO_CONTENT, description = "Reordered"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Shared by someone else (read-only)", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) playlist", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Unknown or repeated entry ids", body = ErrorBody),
    )
)]
async fn reorder_playlist(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<PlaylistOrderRequest>,
) -> ApiResult<StatusCode> {
    owned(&state, &user, id).await?;
    let current = playlists::entry_ids(&state.db, id).await?;
    let mut seen = std::collections::HashSet::new();
    if body
        .entry_ids
        .iter()
        .any(|e| !current.contains(e) || !seen.insert(*e))
    {
        return Err(ApiError::validation(
            "Unbekannte oder doppelte Einträge in der Reihenfolge.",
        ));
    }
    let order: Vec<i64> = body
        .entry_ids
        .iter()
        .copied()
        .chain(current.into_iter().filter(|e| !seen.contains(e)))
        .collect();
    playlists::set_order(&state.db, id, &order).await?;
    Ok(StatusCode::NO_CONTENT)
}
