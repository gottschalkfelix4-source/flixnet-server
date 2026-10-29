//! Browsing: views, item queries, single items, seasons/episodes, latest, genres,
//! people, similar items and search hints. Every query goes through Flixnet's browse
//! layer with the user's visible libraries and age limit.

use std::collections::HashMap;

use axum::Router;
use axum::extract::{Path, State};
use axum::routing::get;
use flixnet_api::auth::AuthUser;
use flixnet_api::browse::{ensure_visible, visible_libraries};
use flixnet_api::error::{ApiError, ApiResult};
use flixnet_db::browse::{self, ItemFilter, PersonRow, Sort};
use flixnet_db::{items, libraries, metadata};

use crate::JfState;
use crate::auth::JfUser;
use crate::convert::{
    Fields, base_dto, flixnet_kind, genre_dto, image_tag, item_detail, item_dtos, item_ids,
    library_dto, person_dto, person_tag,
};
use crate::dto::{BaseItemDto, QueryResult, SearchHint, SearchHintResult};
use crate::ids::{self, Kind};
use crate::util::{Jf, Params, ticks};

/// Largest page; Jellyfin clients often ask without a limit.
const MAX_PAGE: i64 = 2000;
const ALL_KINDS: [&str; 4] = ["movie", "series", "season", "episode"];

pub(crate) fn router() -> Router<JfState> {
    Router::new()
        .route("/userviews", get(views))
        .route("/users/{user}/views", get(views))
        .route("/items", get(list_items))
        .route("/users/{user}/items", get(list_items))
        .route("/items/latest", get(latest))
        .route("/users/{user}/items/latest", get(latest))
        .route("/items/{id}", get(get_item))
        .route("/users/{user}/items/{id}", get(get_user_item))
        .route("/items/{id}/ancestors", get(ancestors))
        .route("/items/{id}/similar", get(similar))
        .route("/movies/{id}/similar", get(similar))
        .route("/shows/{id}/similar", get(similar))
        .route("/items/{id}/intros", get(empty_result))
        .route("/users/{user}/items/{id}/intros", get(empty_result))
        .route("/items/{id}/localtrailers", get(empty_list))
        .route("/users/{user}/items/{id}/localtrailers", get(empty_list))
        .route("/items/{id}/specialfeatures", get(empty_list))
        .route("/users/{user}/items/{id}/specialfeatures", get(empty_list))
        .route("/shows/{id}/seasons", get(seasons))
        .route("/shows/{id}/episodes", get(episodes))
        .route("/genres", get(genres))
        .route("/persons", get(persons))
        .route("/search/hints", get(search_hints))
}

pub(crate) fn page<T>(items: Vec<T>, total: i64, start: i64) -> Jf<QueryResult<T>> {
    Jf(QueryResult {
        items,
        total_record_count: total,
        start_index: start,
    })
}

async fn views(
    jf: JfUser,
    State(state): State<JfState>,
) -> ApiResult<Jf<QueryResult<BaseItemDto>>> {
    let visible = visible_libraries(&state.app, &jf.user, None).await?;
    let mut out = Vec::new();
    for library in libraries::list(&state.app.db).await? {
        if visible.contains(&library.id) {
            out.push(library_dto(&state, &library).await?);
        }
    }
    let total = out.len() as i64;
    Ok(page(out, total, 0))
}

fn sort_from(params: &Params, search: bool, children: bool) -> (Sort, bool) {
    let by = params.list("sortby");
    let sort = match by.first().map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("sortname" | "name" | "random") => Sort::Title,
        Some("datecreated" | "datelastcontentadded") => Sort::DateAdded,
        Some("productionyear") => Sort::Year,
        Some("communityrating" | "criticrating") => Sort::Rating,
        Some("premieredate") => Sort::PremiereDate,
        Some("indexnumber" | "parentindexnumber" | "airedepisodeorder") => Sort::Index,
        _ if search => Sort::Relevance,
        _ if children => Sort::Index,
        _ => Sort::Title,
    };
    let descending = params
        .list("sortorder")
        .first()
        .is_some_and(|o| o.eq_ignore_ascii_case("descending"));
    (sort, descending)
}

fn keep_kinds(kinds: &mut Option<Vec<&'static str>>, allowed: &[&'static str]) {
    let current = kinds.take().unwrap_or_else(|| ALL_KINDS.to_vec());
    *kinds = Some(
        current
            .into_iter()
            .filter(|k| allowed.contains(k))
            .collect(),
    );
}

/// Jellyfin's item query as a Flixnet filter; `None` when nothing can match (e.g.
/// only item types Flixnet does not have).
async fn item_filter(
    state: &JfState,
    user: &AuthUser,
    params: &Params,
) -> ApiResult<Option<ItemFilter>> {
    let db = &state.app.db;
    let visible = visible_libraries(&state.app, user, None).await?;
    // Asking for specific ids means those items, wherever they are.
    let recursive = params.bool("recursive").unwrap_or(false) || params.get("ids").is_some();
    let mut filter = ItemFilter {
        library_ids: visible.clone(),
        max_age: user.max_age_rating,
        user_id: Some(user.user_id),
        limit: params.i64("limit").unwrap_or(MAX_PAGE).clamp(1, MAX_PAGE),
        offset: params.i64("startindex").unwrap_or(0).max(0),
        ..Default::default()
    };

    let include = params.list("includeitemtypes");
    let mut kinds: Option<Vec<&'static str>> = None;
    if !include.is_empty() {
        let mapped: Vec<&'static str> = include.iter().filter_map(|t| flixnet_kind(t)).collect();
        if mapped.is_empty() {
            return Ok(None);
        }
        kinds = Some(mapped);
    }

    let mut children = false;
    match params.get("parentid").map(ids::parse) {
        None => {
            if !recursive && kinds.is_none() {
                kinds = Some(vec!["movie", "series"]);
            }
        }
        Some(Some((Kind::Library, library))) => {
            if !visible.contains(&library) {
                return Ok(None);
            }
            filter.library_ids = vec![library];
            if !recursive && kinds.is_none() {
                kinds = Some(vec!["movie", "series"]);
            }
        }
        Some(Some((Kind::Item, id))) => {
            ensure_visible(&state.app, user, id).await?;
            let item = items::get(db, id)
                .await?
                .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
            children = true;
            if item.kind == "series" && recursive {
                filter.series_id = Some(id);
                filter.exclude_ids.push(id);
            } else {
                filter.parent_id = Some(id);
            }
        }
        Some(Some((Kind::Genre, genre))) => match browse::genre_name(db, genre).await? {
            Some(name) => filter.genres.push(name),
            None => return Ok(None),
        },
        Some(Some((Kind::Person, person))) => filter.person_ids.push(person),
        Some(_) => return Ok(None),
    }

    let exclude: Vec<&'static str> = params
        .list("excludeitemtypes")
        .iter()
        .filter_map(|t| flixnet_kind(t))
        .collect();
    if !exclude.is_empty() {
        let allowed: Vec<&'static str> = ALL_KINDS
            .into_iter()
            .filter(|k| !exclude.contains(k))
            .collect();
        keep_kinds(&mut kinds, &allowed);
    }
    let media_types = params.set("mediatypes");
    if !media_types.is_empty() {
        if !media_types.contains("video") {
            return Ok(None);
        }
        keep_kinds(&mut kinds, &["movie", "episode"]);
    }

    let filters = params.set("filters");
    if filters.contains("isplayed") {
        filter.is_played = Some(true);
    }
    if filters.contains("isunplayed") {
        filter.is_played = Some(false);
    }
    if let Some(played) = params.bool("isplayed") {
        filter.is_played = Some(played);
    }
    if filters.contains("isfavorite") || params.bool("isfavorite") == Some(true) {
        filter.is_favorite = Some(true);
    }
    if filters.contains("isresumable") {
        filter.is_resumable = Some(true);
    }
    if filters.contains("isfolder") || params.bool("isfolder") == Some(true) {
        keep_kinds(&mut kinds, &["series", "season"]);
    }
    if filters.contains("isnotfolder") || params.bool("isfolder") == Some(false) {
        keep_kinds(&mut kinds, &["movie", "episode"]);
    }

    let search = params.get("searchterm");
    if let Some(term) = search {
        match browse::fts_query(term) {
            Some(query) => filter.search = Some(query),
            None => return Ok(None),
        }
    }
    if let Some(letter) = params.get("namestartswith").and_then(|s| s.chars().next())
        && letter.is_ascii_alphabetic()
    {
        filter.letter = Some(letter.to_ascii_lowercase());
    }
    filter.genres.extend(params.split("genres", &['|', ',']));
    for genre in params.list("genreids") {
        if let Some(id) = ids::parse_as(&genre, Kind::Genre)
            && let Some(name) = browse::genre_name(db, id).await?
        {
            filter.genres.push(name);
        }
    }
    filter.years = params
        .list("years")
        .iter()
        .filter_map(|y| y.parse().ok())
        .collect();
    filter.person_ids.extend(
        params
            .list("personids")
            .iter()
            .filter_map(|p| ids::parse_as(p, Kind::Person)),
    );
    if params.get("ids").is_some() {
        filter.ids = item_ids(&params.list("ids"));
        if filter.ids.is_empty() {
            return Ok(None);
        }
    }
    filter
        .exclude_ids
        .extend(item_ids(&params.list("excludeitemids")));

    if let Some(kinds) = kinds {
        if kinds.is_empty() {
            return Ok(None);
        }
        filter.kinds = kinds.into_iter().map(str::to_owned).collect();
    }
    let (sort, descending) = sort_from(params, filter.search.is_some(), children);
    filter.sort = sort;
    filter.descending = descending;
    Ok(Some(filter))
}

pub(crate) async fn query_items(
    state: &JfState,
    user: &AuthUser,
    params: &Params,
) -> ApiResult<Jf<QueryResult<BaseItemDto>>> {
    let start = params.i64("startindex").unwrap_or(0).max(0);
    let Some(filter) = item_filter(state, user, params).await? else {
        return Ok(page(Vec::new(), 0, start));
    };
    let (rows, total) = browse::list(&state.app.db, &filter).await?;
    let dtos = item_dtos(state, user, rows, &Fields::from_params(params)).await?;
    Ok(page(dtos, total, start))
}

async fn list_items(
    jf: JfUser,
    State(state): State<JfState>,
    params: Params,
) -> ApiResult<Jf<QueryResult<BaseItemDto>>> {
    query_items(&state, &jf.user, &params).await
}

/// One item of any kind this layer hands out ids for.
pub(crate) async fn any_item(state: &JfState, user: &AuthUser, id: &str) -> ApiResult<BaseItemDto> {
    let not_found = || ApiError::not_found("Eintrag nicht gefunden.");
    let db = &state.app.db;
    match ids::parse(id).ok_or_else(not_found)? {
        (Kind::Item, id) => {
            ensure_visible(&state.app, user, id).await?;
            item_detail(state, user, id).await
        }
        (Kind::Library, id) => {
            if !visible_libraries(&state.app, user, Some(id))
                .await?
                .contains(&id)
            {
                return Err(not_found());
            }
            let library = libraries::get(db, id).await?.ok_or_else(not_found)?;
            library_dto(state, &library).await
        }
        (Kind::Person, id) => {
            let person = metadata::person(db, id).await?.ok_or_else(not_found)?;
            Ok(person_dto(
                state,
                &PersonRow {
                    id: person.id,
                    name: person.name,
                    image_url: person.image_url,
                },
            ))
        }
        (Kind::Genre, id) => {
            let name = browse::genre_name(db, id).await?.ok_or_else(not_found)?;
            Ok(genre_dto(state, id, &name, None))
        }
        _ => Err(not_found()),
    }
}

async fn get_item(
    jf: JfUser,
    State(state): State<JfState>,
    Path(id): Path<String>,
) -> ApiResult<Jf<BaseItemDto>> {
    any_item(&state, &jf.user, &id).await.map(Jf)
}

async fn get_user_item(
    jf: JfUser,
    State(state): State<JfState>,
    Path((_user, id)): Path<(String, String)>,
) -> ApiResult<Jf<BaseItemDto>> {
    any_item(&state, &jf.user, &id).await.map(Jf)
}

/// Season, series and library of an item, nearest first.
async fn ancestors(
    jf: JfUser,
    State(state): State<JfState>,
    Path(id): Path<String>,
) -> ApiResult<Jf<Vec<BaseItemDto>>> {
    let id = ids::parse_as(&id, Kind::Item)
        .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
    ensure_visible(&state.app, &jf.user, id).await?;
    let db = &state.app.db;
    let item = items::get(db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
    let mut chain = Vec::new();
    let mut parent = item.parent_id;
    while let Some(pid) = parent {
        let Some(p) = items::get(db, pid).await? else {
            break;
        };
        chain.push(p.id);
        parent = p.parent_id;
    }
    let mut rows = browse::summaries(db, &chain, Some(jf.user.user_id)).await?;
    rows.sort_by_key(|r| chain.iter().position(|c| *c == r.id));
    let mut out = item_dtos(&state, &jf.user, rows, &Fields::default()).await?;
    if let Some(library) = libraries::get(db, item.library_id).await? {
        out.push(library_dto(&state, &library).await?);
    }
    Ok(Jf(out))
}

/// Same kind, sharing genres, best rated first.
async fn similar(
    jf: JfUser,
    State(state): State<JfState>,
    Path(id): Path<String>,
    params: Params,
) -> ApiResult<Jf<QueryResult<BaseItemDto>>> {
    let id = ids::parse_as(&id, Kind::Item)
        .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
    ensure_visible(&state.app, &jf.user, id).await?;
    let db = &state.app.db;
    let item = items::get(db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
    let filter = ItemFilter {
        library_ids: visible_libraries(&state.app, &jf.user, None).await?,
        kinds: vec![item.kind.clone()],
        genres: metadata::genres(db, id).await?,
        exclude_ids: vec![id],
        max_age: jf.user.max_age_rating,
        user_id: Some(jf.user.user_id),
        sort: Sort::Rating,
        descending: true,
        limit: params.i64("limit").unwrap_or(12).clamp(1, 100),
        ..Default::default()
    };
    let (rows, _) = browse::list(db, &filter).await?;
    let total = rows.len() as i64;
    let dtos = item_dtos(&state, &jf.user, rows, &Fields::from_params(&params)).await?;
    Ok(page(dtos, total, 0))
}

async fn empty_result(_: JfUser) -> Jf<QueryResult<BaseItemDto>> {
    page(Vec::new(), 0, 0)
}

async fn empty_list(_: JfUser) -> Jf<Vec<BaseItemDto>> {
    Jf(Vec::new())
}

async fn series_id(state: &JfState, user: &AuthUser, id: &str) -> ApiResult<i64> {
    let id = ids::parse_as(id, Kind::Item)
        .ok_or_else(|| ApiError::not_found("Serie nicht gefunden."))?;
    ensure_visible(&state.app, user, id).await?;
    items::get(&state.app.db, id)
        .await?
        .filter(|i| i.kind == "series")
        .map(|i| i.id)
        .ok_or_else(|| ApiError::not_found("Serie nicht gefunden."))
}

fn child_filter(state_libraries: Vec<i64>, user: &AuthUser, kind: &str) -> ItemFilter {
    ItemFilter {
        library_ids: state_libraries,
        kinds: vec![kind.to_owned()],
        max_age: user.max_age_rating,
        user_id: Some(user.user_id),
        sort: Sort::Index,
        limit: MAX_PAGE,
        ..Default::default()
    }
}

async fn seasons(
    jf: JfUser,
    State(state): State<JfState>,
    Path(id): Path<String>,
    params: Params,
) -> ApiResult<Jf<QueryResult<BaseItemDto>>> {
    let series = series_id(&state, &jf.user, &id).await?;
    let mut filter = child_filter(
        visible_libraries(&state.app, &jf.user, None).await?,
        &jf.user,
        "season",
    );
    filter.parent_id = Some(series);
    if params.bool("isspecialseason") == Some(false) {
        filter.exclude_ids = items::children(&state.app.db, series)
            .await?
            .into_iter()
            .filter(|s| s.index_number == Some(0))
            .map(|s| s.id)
            .collect();
    }
    let (rows, total) = browse::list(&state.app.db, &filter).await?;
    let dtos = item_dtos(&state, &jf.user, rows, &Fields::from_params(&params)).await?;
    Ok(page(dtos, total, 0))
}

async fn episodes(
    jf: JfUser,
    State(state): State<JfState>,
    Path(id): Path<String>,
    params: Params,
) -> ApiResult<Jf<QueryResult<BaseItemDto>>> {
    let db = &state.app.db;
    let series = series_id(&state, &jf.user, &id).await?;
    let mut filter = child_filter(
        visible_libraries(&state.app, &jf.user, None).await?,
        &jf.user,
        "episode",
    );
    filter.series_id = Some(series);
    if let Some(season) = params
        .get("seasonid")
        .and_then(|s| ids::parse_as(s, Kind::Item))
    {
        filter.parent_id = Some(season);
    } else if let Some(number) = params.i64("season") {
        let season = items::children(db, series)
            .await?
            .into_iter()
            .find(|s| s.kind == "season" && s.index_number == Some(number));
        match season {
            Some(season) => filter.parent_id = Some(season.id),
            None => return Ok(page(Vec::new(), 0, 0)),
        }
    }
    let (mut rows, _) = browse::list(db, &filter).await?;
    if let Some(start_item) = params
        .get("startitemid")
        .and_then(|s| ids::parse_as(s, Kind::Item))
        && let Some(pos) = rows.iter().position(|r| r.id == start_item)
    {
        rows.drain(..pos);
    }
    let total = rows.len() as i64;
    let start = params.i64("startindex").unwrap_or(0).clamp(0, total);
    let limit = params.i64("limit").unwrap_or(MAX_PAGE).clamp(1, MAX_PAGE);
    let rows: Vec<_> = rows
        .into_iter()
        .skip(start as usize)
        .take(limit as usize)
        .collect();
    let dtos = item_dtos(&state, &jf.user, rows, &Fields::from_params(&params)).await?;
    Ok(page(dtos, total, start))
}

/// Recently added: movies as they are; new episodes grouped by series (a series
/// with several new episodes appears once, as the series).
async fn latest(
    jf: JfUser,
    State(state): State<JfState>,
    params: Params,
) -> ApiResult<Jf<Vec<BaseItemDto>>> {
    let db = &state.app.db;
    let user = &jf.user;
    let limit = params.i64("limit").unwrap_or(16).clamp(1, 100);
    let visible = visible_libraries(&state.app, user, None).await?;
    let library_ids: Vec<i64> = match params.get("parentid").map(ids::parse) {
        None => visible,
        Some(Some((Kind::Library, id))) if visible.contains(&id) => vec![id],
        Some(_) => return Ok(Jf(Vec::new())),
    };
    let include: Vec<&'static str> = params
        .list("includeitemtypes")
        .iter()
        .filter_map(|t| flixnet_kind(t))
        .collect();
    let wants = |kind: &str| include.is_empty() || include.contains(&kind);
    let group = params.bool("groupitems").unwrap_or(true);
    let base = ItemFilter {
        library_ids,
        max_age: user.max_age_rating,
        user_id: Some(user.user_id),
        is_played: params.bool("isplayed"),
        sort: Sort::DateAdded,
        descending: true,
        ..Default::default()
    };

    // (date added, item id) of the candidates.
    let mut picks: Vec<(i64, i64)> = Vec::new();
    if wants("movie") {
        let (rows, _) = browse::list(
            db,
            &ItemFilter {
                kinds: vec!["movie".into()],
                limit,
                ..base.clone()
            },
        )
        .await?;
        picks.extend(rows.iter().map(|r| (r.date_added, r.id)));
    }
    if wants("episode") || wants("series") {
        let (rows, _) = browse::list(
            db,
            &ItemFilter {
                kinds: vec!["episode".into()],
                limit: limit * 5,
                ..base.clone()
            },
        )
        .await?;
        let mut per_series: HashMap<i64, usize> = HashMap::new();
        for row in &rows {
            if let Some(series) = row.series_id {
                *per_series.entry(series).or_default() += 1;
            }
        }
        let mut seen = std::collections::HashSet::new();
        for row in rows {
            match row.series_id {
                Some(series) if (group && per_series[&series] > 1) || !wants("episode") => {
                    if seen.insert(series) {
                        picks.push((row.date_added, series));
                    }
                }
                _ if wants("episode") => picks.push((row.date_added, row.id)),
                _ => {}
            }
        }
    }
    picks.sort_by_key(|p| std::cmp::Reverse(p.0));
    picks.truncate(limit as usize);
    let ids: Vec<i64> = picks.iter().map(|(_, id)| *id).collect();
    let mut rows = browse::summaries(db, &ids, Some(user.user_id)).await?;
    rows.sort_by_key(|r| ids.iter().position(|id| *id == r.id));
    Ok(Jf(item_dtos(
        &state,
        user,
        rows,
        &Fields::from_params(&params),
    )
    .await?))
}

async fn libraries_of(
    state: &JfState,
    user: &AuthUser,
    params: &Params,
) -> ApiResult<Option<Vec<i64>>> {
    let visible = visible_libraries(&state.app, user, None).await?;
    Ok(match params.get("parentid").map(ids::parse) {
        None => Some(visible),
        Some(Some((Kind::Library, id))) if visible.contains(&id) => Some(vec![id]),
        Some(_) => None,
    })
}

async fn genres(
    jf: JfUser,
    State(state): State<JfState>,
    params: Params,
) -> ApiResult<Jf<QueryResult<BaseItemDto>>> {
    let start = params.i64("startindex").unwrap_or(0).max(0);
    let Some(library_ids) = libraries_of(&state, &jf.user, &params).await? else {
        return Ok(page(Vec::new(), 0, start));
    };
    let search = params.get("searchterm").map(str::to_lowercase);
    let prefix = params.get("namestartswith").map(str::to_lowercase);
    let all: Vec<_> = browse::genres_with_ids(&state.app.db, &library_ids)
        .await?
        .into_iter()
        .filter(|(_, name, _)| {
            let lower = name.to_lowercase();
            search.as_deref().is_none_or(|s| lower.contains(s))
                && prefix.as_deref().is_none_or(|p| lower.starts_with(p))
        })
        .collect();
    let total = all.len() as i64;
    let limit = params.i64("limit").unwrap_or(MAX_PAGE).clamp(1, MAX_PAGE);
    let items = all
        .into_iter()
        .skip(start as usize)
        .take(limit as usize)
        .map(|(id, name, count)| genre_dto(&state, id, &name, Some(count)))
        .collect();
    Ok(page(items, total, start))
}

async fn persons(
    jf: JfUser,
    State(state): State<JfState>,
    params: Params,
) -> ApiResult<Jf<QueryResult<BaseItemDto>>> {
    let db = &state.app.db;
    let start = params.i64("startindex").unwrap_or(0).max(0);
    let limit = params.i64("limit").unwrap_or(100).clamp(1, MAX_PAGE);
    if let Some(item) = params.get("appearsinitemid") {
        let id = ids::parse_as(item, Kind::Item)
            .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
        ensure_visible(&state.app, &jf.user, id).await?;
        let people: Vec<BaseItemDto> = metadata::credits(db, id)
            .await?
            .into_iter()
            .map(|c| {
                person_dto(
                    &state,
                    &PersonRow {
                        id: c.person_id,
                        name: c.name,
                        image_url: c.image_url,
                    },
                )
            })
            .collect();
        let total = people.len() as i64;
        return Ok(page(people, total, 0));
    }
    let Some(library_ids) = libraries_of(&state, &jf.user, &params).await? else {
        return Ok(page(Vec::new(), 0, start));
    };
    let (rows, total) =
        browse::people(db, &library_ids, params.get("searchterm"), limit, start).await?;
    let items = rows.iter().map(|p| person_dto(&state, p)).collect();
    Ok(page(items, total, start))
}

async fn search_hints(
    jf: JfUser,
    State(state): State<JfState>,
    params: Params,
) -> ApiResult<Jf<SearchHintResult>> {
    let db = &state.app.db;
    let empty = || {
        Jf(SearchHintResult {
            search_hints: Vec::new(),
            total_record_count: 0,
        })
    };
    let Some(term) = params.get("searchterm") else {
        return Ok(empty());
    };
    let Some(query) = browse::fts_query(term) else {
        return Ok(empty());
    };
    let limit = params.i64("limit").unwrap_or(20).clamp(1, 100);
    let include = params.list("includeitemtypes");
    let mut kinds: Vec<&'static str> = include.iter().filter_map(|t| flixnet_kind(t)).collect();
    let wants_people = params.bool("includepeople").unwrap_or(true)
        && (include.is_empty() || include.iter().any(|t| t.eq_ignore_ascii_case("person")));
    if include.is_empty() {
        kinds = vec!["movie", "series", "episode"];
    }
    let library_ids = visible_libraries(&state.app, &jf.user, None).await?;
    let mut hints = Vec::new();
    let mut total = 0;
    if params.bool("includemedia").unwrap_or(true) && !kinds.is_empty() {
        let filter = ItemFilter {
            library_ids: library_ids.clone(),
            kinds: kinds.iter().map(|k| (*k).to_owned()).collect(),
            search: Some(query),
            max_age: jf.user.max_age_rating,
            user_id: Some(jf.user.user_id),
            sort: Sort::Relevance,
            limit,
            offset: params.i64("startindex").unwrap_or(0).max(0),
            ..Default::default()
        };
        let (rows, count) = browse::list(db, &filter).await?;
        total += count;
        for row in rows {
            let dto = base_dto(&state, &row);
            hints.push(SearchHint {
                item_id: dto.id.clone(),
                id: dto.id,
                name: row.title,
                matched_term: Some(term.to_owned()),
                index_number: row.index_number,
                production_year: row.year,
                parent_index_number: row.parent_index_number,
                primary_image_tag: row.poster_id.map(|i| image_tag(&state, row.id, i)),
                backdrop_image_tag: row.backdrop_id.map(|i| image_tag(&state, row.id, i)),
                backdrop_image_item_id: row.backdrop_id.map(|_| ids::guid(Kind::Item, row.id)),
                kind: dto.kind,
                is_folder: dto.is_folder,
                run_time_ticks: row.runtime_ms.map(ticks),
                media_type: dto.media_type,
                series: row.series_title,
                artists: Vec::new(),
                primary_image_aspect_ratio: dto.primary_image_aspect_ratio,
            });
        }
    }
    if wants_people {
        let (people, count) = browse::people(db, &library_ids, Some(term), limit, 0).await?;
        total += count;
        for person in people {
            let guid = ids::guid(Kind::Person, person.id);
            hints.push(SearchHint {
                item_id: guid.clone(),
                id: guid,
                name: person.name,
                matched_term: Some(term.to_owned()),
                index_number: None,
                production_year: None,
                parent_index_number: None,
                primary_image_tag: person.image_url.map(|_| person_tag(&state, person.id)),
                backdrop_image_tag: None,
                backdrop_image_item_id: None,
                kind: "Person",
                is_folder: false,
                run_time_ticks: None,
                media_type: "Unknown",
                series: None,
                artists: Vec::new(),
                primary_image_aspect_ratio: Some(2.0 / 3.0),
            });
        }
    }
    Ok(Jf(SearchHintResult {
        search_hints: hints,
        total_record_count: total,
    }))
}
