//! `/api/v1/collections`: automatic (TMDB) and manual collections of movies and series.
//!
//! Everyone sees a collection's members only as far as their library access and age
//! limit allow; a collection without any visible member does not exist for them
//! (administrators still see their empty manual collections, to fill them).

use std::collections::HashMap;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use flixnet_db::browse::{self, ItemFilter};
use flixnet_db::collections::{self, Collection, VisibleStats};
use flixnet_db::metadata::Image;
use flixnet_db::{items, playlists};
use flixnet_library::scan::sort_title;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::{AdminUser, AuthUser};
use crate::browse::{ItemCardDto, card, ensure_visible, visible_libraries};
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::validate;

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_collections, create_collection))
        .routes(routes!(
            get_collection,
            update_collection,
            delete_collection
        ))
        .routes(routes!(add_collection_items))
        .routes(routes!(remove_collection_item))
        .routes(routes!(get_collection_image))
        .routes(routes!(list_item_collections))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CollectionDto {
    pub id: i64,
    /// `auto` (from TMDB) or `manual` (curated by an administrator).
    pub kind: String,
    pub name: String,
    pub overview: Option<String>,
    /// TMDB collection id of automatic collections.
    pub tmdb_id: Option<i64>,
    /// Signed poster URL (the collection's own, else a member's), 342 px wide.
    pub poster_url: Option<String>,
    pub poster_blurhash: Option<String>,
    /// Signed backdrop URL, 1920 px wide.
    pub backdrop_url: Option<String>,
    pub backdrop_blurhash: Option<String>,
    /// Members the signed-in user may see.
    pub item_count: i64,
    /// Of those, how many the user has watched.
    pub played_count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CollectionDetailDto {
    #[serde(flatten)]
    pub collection: CollectionDto,
    /// Visible members in release order.
    pub items: Vec<ItemCardDto>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateCollectionRequest {
    pub name: String,
    pub overview: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateCollectionRequest {
    pub name: Option<String>,
    /// `null` clears the description.
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub overview: Option<Option<String>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CollectionItemsRequest {
    /// Movies or series to add.
    pub item_ids: Vec<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AddedDto {
    /// How many were new (already present ones are skipped).
    pub added: u64,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CollectionImageQuery {
    /// Desired width in pixels (snapped like `/images/{id}`).
    pub w: Option<u32>,
    /// Expiry of the signature (unix seconds), from URLs returned by the API.
    pub exp: Option<i64>,
    /// URL signature, from URLs returned by the API.
    pub sig: Option<String>,
}

/// The viewer's restrictions as a browse filter (libraries, age limit, watched state).
pub(crate) async fn viewer_filter(state: &AppState, user: &AuthUser) -> ApiResult<ItemFilter> {
    Ok(ItemFilter {
        library_ids: visible_libraries(state, user, None).await?,
        max_age: user.max_age_rating,
        user_id: Some(user.user_id),
        ..Default::default()
    })
}

/// Card details for items, in the given order.
pub(crate) async fn cards(
    state: &AppState,
    user: &AuthUser,
    ids: &[i64],
) -> ApiResult<Vec<ItemCardDto>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut by_id: HashMap<i64, browse::ItemSummary> =
        browse::summaries(&state.db, ids, Some(user.user_id))
            .await?
            .into_iter()
            .map(|s| (s.id, s))
            .collect();
    Ok(ids
        .iter()
        .filter_map(|id| by_id.remove(id))
        .map(|s| card(state, s))
        .collect())
}

fn image_scope(kind: &str) -> String {
    format!("collection-{kind}")
}

fn image_url(state: &AppState, id: i64, kind: &str, width: u32) -> String {
    let (expires, sig) = state.signer.sign(&image_scope(kind), id);
    format!("/api/v1/collections/{id}/images/{kind}?w={width}&exp={expires}&sig={sig}")
}

fn dto(
    state: &AppState,
    c: Collection,
    stats: Option<&VisibleStats>,
    fallback: Option<&ItemCardDto>,
) -> CollectionDto {
    let poster_url = match &c.poster_url {
        Some(_) => Some(image_url(state, c.id, "poster", 342)),
        None => fallback.and_then(|f| f.poster_url.clone()),
    };
    let poster_blurhash = match &c.poster_url {
        Some(_) => c.poster_blurhash.clone(),
        None => fallback.and_then(|f| f.poster_blurhash.clone()),
    };
    let backdrop_url = match &c.backdrop_url {
        Some(_) => Some(image_url(state, c.id, "backdrop", 1920)),
        None => fallback.and_then(|f| f.backdrop_url.clone()),
    };
    CollectionDto {
        id: c.id,
        tmdb_id: (c.provider.as_deref() == Some("tmdb"))
            .then(|| c.provider_id.as_deref().and_then(|v| v.parse().ok()))
            .flatten(),
        kind: c.kind,
        name: c.name,
        overview: c.overview,
        poster_url,
        poster_blurhash,
        backdrop_url,
        backdrop_blurhash: c.backdrop_url.and(c.backdrop_blurhash),
        item_count: stats.map_or(0, |s| s.item_count),
        played_count: stats.map_or(0, |s| s.played_count),
    }
}

/// Whether the collection exists for this viewer.
fn shown(user: &AuthUser, c: &Collection, stats: Option<&VisibleStats>) -> bool {
    stats.is_some() || (user.is_admin && c.kind == "manual")
}

/// A collection the viewer may see, with its stats; 404 otherwise.
async fn visible_collection(
    state: &AppState,
    user: &AuthUser,
    id: i64,
) -> ApiResult<(Collection, Option<VisibleStats>, ItemFilter)> {
    let not_found = || ApiError::not_found("Sammlung nicht gefunden.");
    let collection = collections::get(&state.db, id)
        .await?
        .ok_or_else(not_found)?;
    let filter = viewer_filter(state, user).await?;
    let stats = collections::visible_stats(&state.db, &filter, user.user_id, Some(id))
        .await?
        .pop();
    if !shown(user, &collection, stats.as_ref()) {
        return Err(not_found());
    }
    Ok((collection, stats, filter))
}

async fn detail(
    state: &AppState,
    user: &AuthUser,
    collection: Collection,
    stats: Option<VisibleStats>,
    filter: &ItemFilter,
) -> ApiResult<CollectionDetailDto> {
    let ids = collections::member_ids(&state.db, collection.id, filter).await?;
    let members = cards(state, user, &ids).await?;
    Ok(CollectionDetailDto {
        collection: dto(state, collection, stats.as_ref(), members.first()),
        items: members,
    })
}

fn collection_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 200 || name.chars().any(char::is_control) {
        return Err(ApiError::validation(
            "Der Name muss zwischen 1 und 200 Zeichen lang sein.",
        ));
    }
    Ok(name.to_owned())
}

fn overview(text: Option<&str>) -> Option<String> {
    text.map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

/// Collections with at least one visible member, by name.
#[utoipa::path(
    get,
    path = "/collections",
    operation_id = "listCollections",
    tag = "collections",
    responses(
        (status = OK, description = "Collections", body = Vec<CollectionDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn list_collections(
    user: AuthUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<CollectionDto>>> {
    let filter = viewer_filter(&state, &user).await?;
    let stats: HashMap<i64, VisibleStats> =
        collections::visible_stats(&state.db, &filter, user.user_id, None)
            .await?
            .into_iter()
            .map(|s| (s.collection_id, s))
            .collect();
    let shown_list: Vec<Collection> = collections::list(&state.db)
        .await?
        .into_iter()
        .filter(|c| shown(&user, c, stats.get(&c.id)))
        .collect();
    // Collections without own artwork borrow it from a member.
    let fallback_ids: Vec<i64> = shown_list
        .iter()
        .filter(|c| c.poster_url.is_none() || c.backdrop_url.is_none())
        .filter_map(|c| stats.get(&c.id).map(|s| s.first_item_id))
        .collect();
    let fallbacks: HashMap<i64, ItemCardDto> = cards(&state, &user, &fallback_ids)
        .await?
        .into_iter()
        .map(|c| (c.id, c))
        .collect();
    Ok(Json(
        shown_list
            .into_iter()
            .map(|c| {
                let s = stats.get(&c.id);
                let fallback = s.and_then(|s| fallbacks.get(&s.first_item_id));
                dto(&state, c, s, fallback)
            })
            .collect(),
    ))
}

/// Create a manual collection (administrators).
#[utoipa::path(
    post,
    path = "/collections",
    operation_id = "createCollection",
    tag = "collections",
    request_body = CreateCollectionRequest,
    responses(
        (status = CREATED, description = "Created", body = CollectionDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid name", body = ErrorBody),
    )
)]
async fn create_collection(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Json(body): Json<CreateCollectionRequest>,
) -> ApiResult<(StatusCode, Json<CollectionDto>)> {
    let name = collection_name(&body.name)?;
    let created = collections::create_manual(
        &state.db,
        &name,
        &sort_title(&name),
        overview(body.overview.as_deref()).as_deref(),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(dto(&state, created, None, None))))
}

/// A collection with its visible members in release order.
#[utoipa::path(
    get,
    path = "/collections/{id}",
    operation_id = "getCollection",
    tag = "collections",
    params(("id" = i64, Path, description = "Collection id")),
    responses(
        (status = OK, description = "The collection", body = CollectionDetailDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) collection", body = ErrorBody),
    )
)]
async fn get_collection(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<CollectionDetailDto>> {
    let (collection, stats, filter) = visible_collection(&state, &user, id).await?;
    Ok(Json(
        detail(&state, &user, collection, stats, &filter).await?,
    ))
}

async fn manual_collection(state: &AppState, id: i64) -> ApiResult<Collection> {
    let collection = collections::get(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Sammlung nicht gefunden."))?;
    if collection.kind != "manual" {
        return Err(ApiError::conflict(
            "collection_automatic",
            "Automatische Sammlungen werden aus TMDB gepflegt.",
        ));
    }
    Ok(collection)
}

/// Rename or re-describe a manual collection (administrators).
#[utoipa::path(
    patch,
    path = "/collections/{id}",
    operation_id = "updateCollection",
    tag = "collections",
    params(("id" = i64, Path, description = "Collection id")),
    request_body = UpdateCollectionRequest,
    responses(
        (status = OK, description = "Updated", body = CollectionDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such collection", body = ErrorBody),
        (status = CONFLICT, description = "Automatic collections cannot be edited", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid name", body = ErrorBody),
    )
)]
async fn update_collection(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<UpdateCollectionRequest>,
) -> ApiResult<Json<CollectionDto>> {
    manual_collection(&state, id).await?;
    let name = body.name.as_deref().map(collection_name).transpose()?;
    let sort = name.as_deref().map(sort_title);
    let description = body.overview.map(|o| overview(o.as_deref()));
    collections::update(
        &state.db,
        id,
        name.as_deref().zip(sort.as_deref()),
        description.as_ref().map(Option::as_deref),
    )
    .await?;
    let (collection, stats, filter) = visible_collection(&state, &user, id).await?;
    let first = collections::member_ids(&state.db, id, &filter).await?;
    let fallback = cards(&state, &user, &first[..first.len().min(1)]).await?;
    Ok(Json(dto(
        &state,
        collection,
        stats.as_ref(),
        fallback.first(),
    )))
}

/// Delete a collection (administrators). Automatic ones return on the next refresh.
#[utoipa::path(
    delete,
    path = "/collections/{id}",
    operation_id = "deleteCollection",
    tag = "collections",
    params(("id" = i64, Path, description = "Collection id")),
    responses(
        (status = NO_CONTENT, description = "Deleted"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such collection", body = ErrorBody),
    )
)]
async fn delete_collection(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    if collections::delete(&state.db, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("Sammlung nicht gefunden."))
    }
}

/// Add movies or series to a manual collection (administrators).
#[utoipa::path(
    post,
    path = "/collections/{id}/items",
    operation_id = "addCollectionItems",
    tag = "collections",
    params(("id" = i64, Path, description = "Collection id")),
    request_body = CollectionItemsRequest,
    responses(
        (status = OK, description = "Added", body = AddedDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such collection or item", body = ErrorBody),
        (status = CONFLICT, description = "Automatic collections cannot be edited", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Only movies and series", body = ErrorBody),
    )
)]
async fn add_collection_items(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<CollectionItemsRequest>,
) -> ApiResult<Json<AddedDto>> {
    manual_collection(&state, id).await?;
    if body.item_ids.is_empty() || body.item_ids.len() > 500 {
        return Err(ApiError::validation("1 bis 500 Einträge angeben."));
    }
    for item_id in &body.item_ids {
        ensure_visible(&state, &user, *item_id).await?;
        let item = items::get(&state.db, *item_id)
            .await?
            .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
        if item.kind != "movie" && item.kind != "series" {
            return Err(ApiError::validation(
                "Sammlungen enthalten nur Filme und Serien.",
            ));
        }
    }
    let identities: Vec<String> = playlists::item_identities(&state.db, &body.item_ids)
        .await?
        .into_iter()
        .map(|(_, identity)| identity)
        .collect();
    let added = collections::add_members(&state.db, id, &identities).await?;
    Ok(Json(AddedDto { added }))
}

/// Remove a movie or series from a manual collection (administrators).
#[utoipa::path(
    delete,
    path = "/collections/{id}/items/{item_id}",
    operation_id = "removeCollectionItem",
    tag = "collections",
    params(
        ("id" = i64, Path, description = "Collection id"),
        ("item_id" = i64, Path, description = "Item id of the member"),
    ),
    responses(
        (status = NO_CONTENT, description = "Removed"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "Not a member", body = ErrorBody),
        (status = CONFLICT, description = "Automatic collections cannot be edited", body = ErrorBody),
    )
)]
async fn remove_collection_item(
    _: AdminUser,
    State(state): State<AppState>,
    Path((id, item_id)): Path<(i64, i64)>,
) -> ApiResult<StatusCode> {
    manual_collection(&state, id).await?;
    let identity = playlists::item_identities(&state.db, &[item_id])
        .await?
        .pop()
        .map(|(_, identity)| identity);
    match identity {
        Some(identity) if collections::remove_member(&state.db, id, &identity).await? => {
            Ok(StatusCode::NO_CONTENT)
        }
        _ => Err(ApiError::not_found("Nicht in dieser Sammlung.")),
    }
}

/// Collections a movie or series belongs to, each with its visible members
/// ("Teil der Sammlung …" on detail pages).
#[utoipa::path(
    get,
    path = "/items/{id}/collections",
    operation_id = "listItemCollections",
    tag = "collections",
    params(("id" = i64, Path, description = "Item id")),
    responses(
        (status = OK, description = "Collections with members", body = Vec<CollectionDetailDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) item", body = ErrorBody),
    )
)]
async fn list_item_collections(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<CollectionDetailDto>>> {
    ensure_visible(&state, &user, id).await?;
    let mut out = Vec::new();
    for collection_id in collections::of_item(&state.db, id).await? {
        if let Ok((collection, stats, filter)) =
            visible_collection(&state, &user, collection_id).await
        {
            out.push(detail(&state, &user, collection, stats, &filter).await?);
        }
    }
    Ok(Json(out))
}

/// Collection artwork (poster or backdrop), resized and cached.
///
/// Use the signed URLs returned by the collection endpoints; alternatively
/// authenticate normally.
#[utoipa::path(
    get,
    path = "/collections/{id}/images/{kind}",
    operation_id = "getCollectionImage",
    tag = "collections",
    security(()),
    params(
        ("id" = i64, Path, description = "Collection id"),
        ("kind" = String, Path, description = "`poster` or `backdrop`"),
        CollectionImageQuery,
    ),
    responses(
        (status = OK, description = "The image (JPEG)", content_type = "image/jpeg"),
        (status = UNAUTHORIZED, description = "Neither a valid signature nor a valid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such image", body = ErrorBody),
    )
)]
async fn get_collection_image(
    user: Option<AuthUser>,
    State(state): State<AppState>,
    Path((id, kind)): Path<(i64, String)>,
    Query(query): Query<CollectionImageQuery>,
) -> ApiResult<Response> {
    let not_found = || ApiError::not_found("Bild nicht gefunden.");
    if kind != "poster" && kind != "backdrop" {
        return Err(not_found());
    }
    let signed = match (query.exp, query.sig.as_deref()) {
        (Some(exp), Some(sig)) => state.signer.verify(&image_scope(&kind), id, exp, sig),
        _ => false,
    };
    if !signed {
        let user = user.ok_or_else(ApiError::unauthorized)?;
        visible_collection(&state, &user, id).await?;
    }
    let collection = collections::get(&state.db, id)
        .await?
        .ok_or_else(not_found)?;
    let (url, blurhash) = if kind == "poster" {
        (collection.poster_url, collection.poster_blurhash)
    } else {
        (collection.backdrop_url, collection.backdrop_blurhash)
    };
    let url = url.ok_or_else(not_found)?;
    let image = Image {
        id,
        item_id: 0,
        kind: kind.clone(),
        source: "tmdb".to_owned(),
        path: None,
        url: Some(url),
        language: None,
        width: None,
        height: None,
        blurhash,
        score: 0.0,
    };
    let variant = state.images.variant(&image, query.w).await.map_err(|err| {
        tracing::warn!(collection = id, %err, "collection image unavailable");
        ApiError::not_found("Bild nicht verfügbar.")
    })?;
    if let Some((_, _, hash)) = &variant.blurhash {
        collections::set_blurhash(&state.db, id, &kind, hash).await?;
    }
    let bytes = tokio::fs::read(&variant.path)
        .await
        .map_err(ApiError::internal)?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, variant.content_type)
        .header(header::CACHE_CONTROL, "private, max-age=604800")
        .body(Body::from(bytes))
        .map_err(ApiError::internal)
}
