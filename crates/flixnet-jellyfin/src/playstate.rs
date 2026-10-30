//! Playback reporting and user data, mapped onto Flixnet's playstate: resume points,
//! "watched" near the end, the watch history and the active-sessions dashboard all
//! work as for Flixnet's own player.

use std::collections::HashMap;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use flixnet_api::auth::AuthUser;
use flixnet_api::browse::{ensure_visible, visible_libraries};
use flixnet_api::error::{ApiError, ApiResult};
use flixnet_api::playstate::{PlaybackReport, progress_session, start_session, stop_session};
use flixnet_db::playstate::{self, Visibility};
use flixnet_db::{browse, items, userdata};
use flixnet_media::decision::PlayMethod;
use serde::Deserialize;

use crate::JfState;
use crate::auth::JfUser;
use crate::convert::{Fields, flixnet_kind, item_dtos, user_data};
use crate::dto::{BaseItemDto, QueryResult, UserItemDataDto};
use crate::ids::{self, Kind};
use crate::items::page;
use crate::playback::stop_play_session;
use crate::util::{Jf, JfJson, Params, derived_id, ms};

pub(crate) fn router() -> Router<JfState> {
    Router::new()
        .route("/sessions/playing", post(playing))
        .route("/sessions/playing/progress", post(progress))
        .route("/sessions/playing/stopped", post(stopped))
        .route("/sessions/playing/ping", post(ping))
        .route("/playingitems/{id}", post(legacy_start).delete(legacy_stop))
        .route("/playingitems/{id}/progress", post(legacy_progress))
        .route(
            "/users/{user}/playingitems/{id}",
            post(legacy_start).delete(legacy_stop),
        )
        .route(
            "/users/{user}/playingitems/{id}/progress",
            post(legacy_progress),
        )
        .route(
            "/userplayeditems/{id}",
            post(mark_played).delete(mark_unplayed),
        )
        .route(
            "/users/{user}/playeditems/{id}",
            post(mark_played).delete(mark_unplayed),
        )
        .route(
            "/userfavoriteitems/{id}",
            post(add_favorite).delete(remove_favorite),
        )
        .route(
            "/users/{user}/favoriteitems/{id}",
            post(add_favorite).delete(remove_favorite),
        )
        .route(
            "/useritems/{id}/userdata",
            get(get_user_data).post(update_user_data),
        )
        .route(
            "/users/{user}/items/{id}/userdata",
            get(get_user_data).post(update_user_data),
        )
        .route("/useritems/resume", get(resume))
        .route("/users/{user}/items/resume", get(resume))
        .route("/shows/nextup", get(next_up))
}

/// `PlaybackStartInfo` / `PlaybackProgressInfo` / `PlaybackStopInfo`, the parts used.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Report {
    item_id: Option<String>,
    media_source_id: Option<String>,
    position_ticks: Option<i64>,
    is_paused: Option<bool>,
    play_method: Option<String>,
    play_session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    Start,
    Progress,
    Stop,
}

async fn report(state: &JfState, user: &AuthUser, event: Event, body: Report) -> ApiResult<()> {
    let item_id = body
        .item_id
        .as_deref()
        .and_then(|i| ids::parse_as(i, Kind::Item))
        .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
    let play_session = body.play_session_id.filter(|p| !p.trim().is_empty());
    let wanted_file = body
        .media_source_id
        .as_deref()
        .and_then(|s| ids::parse_as(s, Kind::File));
    let transcode = play_session
        .as_deref()
        .and_then(|p| state.plays.get(p))
        .filter(|e| e.user_id == user.user_id && e.item_id == item_id)
        .and_then(|e| {
            e.transcodes
                .into_iter()
                .find(|t| wanted_file.is_none_or(|f| f == t.file_id))
        })
        .filter(|t| state.app.transcoder.owner(&t.session) == Some(user.user_id));
    let play_method = match (&transcode, body.play_method.as_deref()) {
        (Some(t), _) => t.method,
        (None, Some(m)) if m.eq_ignore_ascii_case("transcode") => PlayMethod::Transcode,
        (None, Some(m)) if m.eq_ignore_ascii_case("directstream") => PlayMethod::Remux,
        _ => PlayMethod::DirectPlay,
    };
    let flixnet_report = PlaybackReport {
        item_id,
        position_ms: ms(body.position_ticks.unwrap_or(0)).max(0),
        duration_ms: None,
        play_method,
        transcode_session_id: transcode.map(|t| t.session),
        is_paused: body.is_paused.unwrap_or(false),
    };
    // One Flixnet session per device, play session and item: start, progress and
    // stop of the same playback land on the same session.
    let id = derived_id(&format!(
        "jellyfin:{}:{}:{item_id}",
        user.token_id,
        play_session.as_deref().unwrap_or_default()
    ));
    match event {
        Event::Start if state.app.playback.get(&id).is_none() => {
            start_session(&state.app, user, Some(id), &flixnet_report).await?;
        }
        Event::Start | Event::Progress => {
            progress_session(&state.app, user, &id, &flixnet_report).await?;
        }
        Event::Stop => {
            stop_session(&state.app, user, &id, &flixnet_report).await?;
            if let Some(play_session) = play_session {
                stop_play_session(state, user, &play_session).await;
            }
        }
    }
    Ok(())
}

async fn playing(
    jf: JfUser,
    State(state): State<JfState>,
    JfJson(body): JfJson<Report>,
) -> ApiResult<StatusCode> {
    report(&state, &jf.user, Event::Start, body).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn progress(
    jf: JfUser,
    State(state): State<JfState>,
    JfJson(body): JfJson<Report>,
) -> ApiResult<StatusCode> {
    report(&state, &jf.user, Event::Progress, body).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn stopped(
    jf: JfUser,
    State(state): State<JfState>,
    JfJson(body): JfJson<Report>,
) -> ApiResult<StatusCode> {
    report(&state, &jf.user, Event::Stop, body).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn ping(_: JfUser) -> StatusCode {
    StatusCode::NO_CONTENT
}

/// The legacy endpoints carry the report in the path and query.
fn legacy_report(path: &HashMap<String, String>, params: &Params) -> Report {
    Report {
        item_id: path.get("id").cloned(),
        media_source_id: params.get("mediasourceid").map(str::to_owned),
        position_ticks: params.i64("positionticks"),
        is_paused: params.bool("ispaused"),
        play_method: params.get("playmethod").map(str::to_owned),
        play_session_id: params.get("playsessionid").map(str::to_owned),
    }
}

async fn legacy_start(
    jf: JfUser,
    State(state): State<JfState>,
    Path(path): Path<HashMap<String, String>>,
    params: Params,
) -> ApiResult<StatusCode> {
    report(
        &state,
        &jf.user,
        Event::Start,
        legacy_report(&path, &params),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn legacy_progress(
    jf: JfUser,
    State(state): State<JfState>,
    Path(path): Path<HashMap<String, String>>,
    params: Params,
) -> ApiResult<StatusCode> {
    report(
        &state,
        &jf.user,
        Event::Progress,
        legacy_report(&path, &params),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn legacy_stop(
    jf: JfUser,
    State(state): State<JfState>,
    Path(path): Path<HashMap<String, String>>,
    params: Params,
) -> ApiResult<StatusCode> {
    report(&state, &jf.user, Event::Stop, legacy_report(&path, &params)).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- User data ---------------------------------------------------------------------

async fn visible_item(
    state: &JfState,
    user: &AuthUser,
    path: &HashMap<String, String>,
) -> ApiResult<i64> {
    let id = path
        .get("id")
        .and_then(|i| ids::parse_as(i, Kind::Item))
        .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
    ensure_visible(&state.app, user, id).await?;
    Ok(id)
}

async fn user_data_of(state: &JfState, user: &AuthUser, id: i64) -> ApiResult<Jf<UserItemDataDto>> {
    let summary = browse::summaries(&state.app.db, &[id], Some(user.user_id))
        .await?
        .pop()
        .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
    let mut data = user_data(&ids::guid(Kind::Item, id), &summary);
    let stored = userdata::get(&state.app.db, user.user_id, id).await?;
    data.play_count = stored.play_count;
    data.last_played_date = stored.last_played_at.map(crate::util::iso);
    Ok(Jf(data))
}

async fn mark_played(
    jf: JfUser,
    State(state): State<JfState>,
    Path(path): Path<HashMap<String, String>>,
) -> ApiResult<Jf<UserItemDataDto>> {
    let id = visible_item(&state, &jf.user, &path).await?;
    userdata::set_played(&state.app.db, jf.user.user_id, id, true).await?;
    user_data_of(&state, &jf.user, id).await
}

async fn mark_unplayed(
    jf: JfUser,
    State(state): State<JfState>,
    Path(path): Path<HashMap<String, String>>,
) -> ApiResult<Jf<UserItemDataDto>> {
    let id = visible_item(&state, &jf.user, &path).await?;
    userdata::set_played(&state.app.db, jf.user.user_id, id, false).await?;
    user_data_of(&state, &jf.user, id).await
}

async fn add_favorite(
    jf: JfUser,
    State(state): State<JfState>,
    Path(path): Path<HashMap<String, String>>,
) -> ApiResult<Jf<UserItemDataDto>> {
    let id = visible_item(&state, &jf.user, &path).await?;
    userdata::set_favorite(&state.app.db, jf.user.user_id, id, true).await?;
    user_data_of(&state, &jf.user, id).await
}

async fn remove_favorite(
    jf: JfUser,
    State(state): State<JfState>,
    Path(path): Path<HashMap<String, String>>,
) -> ApiResult<Jf<UserItemDataDto>> {
    let id = visible_item(&state, &jf.user, &path).await?;
    userdata::set_favorite(&state.app.db, jf.user.user_id, id, false).await?;
    user_data_of(&state, &jf.user, id).await
}

async fn get_user_data(
    jf: JfUser,
    State(state): State<JfState>,
    Path(path): Path<HashMap<String, String>>,
) -> ApiResult<Jf<UserItemDataDto>> {
    let id = visible_item(&state, &jf.user, &path).await?;
    user_data_of(&state, &jf.user, id).await
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UserDataUpdate {
    played: Option<bool>,
    is_favorite: Option<bool>,
}

/// Watched and favorite state (other members are ignored).
async fn update_user_data(
    jf: JfUser,
    State(state): State<JfState>,
    Path(path): Path<HashMap<String, String>>,
    JfJson(body): JfJson<UserDataUpdate>,
) -> ApiResult<Jf<UserItemDataDto>> {
    let id = visible_item(&state, &jf.user, &path).await?;
    let current = userdata::get(&state.app.db, jf.user.user_id, id).await?;
    if let Some(played) = body.played.filter(|p| *p != current.played) {
        userdata::set_played(&state.app.db, jf.user.user_id, id, played).await?;
    }
    if let Some(favorite) = body.is_favorite {
        userdata::set_favorite(&state.app.db, jf.user.user_id, id, favorite).await?;
    }
    user_data_of(&state, &jf.user, id).await
}

// ---- Resume and Next Up ------------------------------------------------------------

/// The user's visibility, optionally narrowed to one library (`ParentId`).
async fn visibility(
    state: &JfState,
    user: &AuthUser,
    params: &Params,
) -> ApiResult<Option<Visibility>> {
    let visible = visible_libraries(&state.app, user, None).await?;
    let library_ids = match params.get("parentid").map(ids::parse) {
        None => visible,
        Some(Some((Kind::Library, id))) if visible.contains(&id) => vec![id],
        Some(_) => return Ok(None),
    };
    Ok(Some(Visibility {
        library_ids,
        restriction: user.restriction.clone(),
    }))
}

async fn dtos_for(
    state: &JfState,
    user: &AuthUser,
    ids: &[i64],
    params: &Params,
) -> ApiResult<Vec<BaseItemDto>> {
    let mut rows = browse::summaries(&state.app.db, ids, Some(user.user_id)).await?;
    rows.sort_by_key(|r| ids.iter().position(|id| *id == r.id));
    item_dtos(state, user, rows, &Fields::from_params(params)).await
}

/// "Weiterschauen": movies and episodes with a resume point, most recent first.
async fn resume(
    jf: JfUser,
    State(state): State<JfState>,
    params: Params,
) -> ApiResult<Jf<QueryResult<BaseItemDto>>> {
    let start = params.i64("startindex").unwrap_or(0).max(0);
    let Some(visibility) = visibility(&state, &jf.user, &params).await? else {
        return Ok(page(Vec::new(), 0, start));
    };
    let media_types = params.set("mediatypes");
    if !media_types.is_empty() && !media_types.contains("video") {
        return Ok(page(Vec::new(), 0, start));
    }
    let include: Vec<&str> = params
        .list("includeitemtypes")
        .iter()
        .filter_map(|t| flixnet_kind(t))
        .collect();
    let exclude: Vec<&str> = params
        .list("excludeitemtypes")
        .iter()
        .filter_map(|t| flixnet_kind(t))
        .collect();
    let limit = params.i64("limit").unwrap_or(50).clamp(1, 200);
    let candidates =
        playstate::resume_items(&state.app.db, jf.user.user_id, &visibility, 200).await?;
    let mut ids = Vec::new();
    for id in candidates {
        let kind = items::get(&state.app.db, id).await?.map(|i| i.kind);
        let kind = kind.as_deref().unwrap_or_default();
        if (include.is_empty() || include.contains(&kind)) && !exclude.contains(&kind) {
            ids.push(id);
        }
    }
    let total = ids.len() as i64;
    let ids: Vec<i64> = ids
        .into_iter()
        .skip(start as usize)
        .take(limit as usize)
        .collect();
    let dtos = dtos_for(&state, &jf.user, &ids, &params).await?;
    Ok(page(dtos, total, start))
}

/// "Nächste Folge": with `SeriesId` the episode to continue that series with,
/// otherwise the next episodes of recently watched series.
async fn next_up(
    jf: JfUser,
    State(state): State<JfState>,
    params: Params,
) -> ApiResult<Jf<QueryResult<BaseItemDto>>> {
    let start = params.i64("startindex").unwrap_or(0).max(0);
    let db = &state.app.db;
    let limit = params.i64("limit").unwrap_or(20).clamp(1, 100);
    let ids: Vec<i64> = if let Some(series) = params.get("seriesid") {
        let series = ids::parse_as(series, Kind::Item)
            .ok_or_else(|| ApiError::not_found("Serie nicht gefunden."))?;
        ensure_visible(&state.app, &jf.user, series).await?;
        match playstate::series_next(db, jf.user.user_id, series).await? {
            Some(next)
                if browse::is_visible(
                    db,
                    next.episode_id,
                    &visible_libraries(&state.app, &jf.user, None).await?,
                    &jf.user.restriction,
                )
                .await? =>
            {
                vec![next.episode_id]
            }
            _ => Vec::new(),
        }
    } else {
        let Some(visibility) = visibility(&state, &jf.user, &params).await? else {
            return Ok(page(Vec::new(), 0, start));
        };
        let since = flixnet_db::now_ms() - playstate::NEXT_UP_MAX_AGE_MS;
        playstate::next_up(db, jf.user.user_id, &visibility, since, limit + start)
            .await?
            .into_iter()
            .skip(start as usize)
            .collect()
    };
    let total = ids.len() as i64 + start;
    let dtos = dtos_for(&state, &jf.user, &ids, &params).await?;
    Ok(page(dtos, total, start))
}
