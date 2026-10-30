//! Watch history import from Plex and Jellyfin.
//!
//! The other server is read (watched state, resume points, favorites, ratings, last
//! played), every entry is matched to a Flixnet item (provider ids first, then title +
//! year or series + season + episode) and the result is merged into the user data of
//! the mapped Flixnet users. A preview (`importPreview`) shows the match statistics
//! without writing anything; `importApply` writes.

use std::collections::HashMap;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use flixnet_db::history::{self, ImportedState};
use flixnet_db::users;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::matching::{ExternalIds, LibraryIndex, MatchMethod, strip_year_suffix};
use super::time::parse_rfc3339;
use crate::AppState;
use crate::auth::AdminUser;
use crate::error::{ApiError, ApiResult, ErrorBody};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(import_preview))
        .routes(routes!(import_apply))
}

/// Entries per page when reading the other server.
const PAGE: usize = 500;
/// Unmatched titles listed in the preview.
const SAMPLES: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportSource {
    Plex,
    Jellyfin,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UserMapping {
    /// User id on the other server.
    pub source_user_id: String,
    /// Flixnet user that receives this history; `0` skips this user.
    pub user_id: i64,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ImportRequest {
    pub source: ImportSource,
    /// Base URL of the other server, e.g. `http://192.168.1.10:32400`.
    pub server_url: String,
    /// Plex: `X-Plex-Token` (not needed when Plex lets this network in without
    /// sign-in). Jellyfin: an API key (Dashboard → API-Schlüssel).
    pub token: Option<String>,
    /// Jellyfin without API key: sign in as this user (imports only that user).
    pub username: Option<String>,
    pub password: Option<String>,
    /// Plex (one account per token) and Jellyfin sign-in: the Flixnet user to import
    /// into.
    pub user_id: Option<i64>,
    /// Jellyfin with API key: which Jellyfin user goes to which Flixnet user. Users
    /// left out are mapped by equal name, if there is one.
    #[serde(default)]
    pub user_map: Vec<UserMapping>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, ToSchema)]
pub struct MatchStats {
    /// Entries with any state (watched, in progress, favorite, rated).
    pub entries: usize,
    pub matched: usize,
    pub unmatched: usize,
    pub by_provider_id: usize,
    pub by_title_year: usize,
    pub by_episode: usize,
    pub played: usize,
    pub in_progress: usize,
    pub favorites: usize,
    pub rated: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ImportUserDto {
    pub source_user_id: String,
    pub source_user_name: String,
    /// Flixnet user the history goes to (`None`: skipped, no mapping).
    pub user_id: Option<i64>,
    pub user_name: Option<String>,
    pub stats: MatchStats,
    /// Some titles that were not found in the library.
    pub unmatched_samples: Vec<String>,
    /// After applying: items written.
    pub written_items: Option<usize>,
    /// After applying: history entries added.
    pub written_history: Option<usize>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ImportResultDto {
    pub source: ImportSource,
    /// Whether anything was written (`false` for a preview).
    pub applied: bool,
    pub users: Vec<ImportUserDto>,
}

/// What the other server reported for one movie or episode (or a series, which
/// carries only a favorite or a rating).
#[derive(Debug, Clone, Default)]
struct SourceItem {
    episode: bool,
    series: bool,
    title: String,
    year: Option<i64>,
    ids: ExternalIds,
    series_title: String,
    series_year: Option<i64>,
    series_ids: ExternalIds,
    season: Option<i64>,
    number: Option<i64>,
    played: bool,
    play_count: i64,
    position_ms: i64,
    last_played_at: Option<i64>,
    favorite: bool,
    rating: Option<f64>,
}

impl SourceItem {
    fn has_state(&self) -> bool {
        self.played || self.position_ms > 0 || self.favorite || self.rating.is_some()
    }

    fn label(&self) -> String {
        if self.episode {
            match (self.season, self.number) {
                (Some(s), Some(e)) => format!("{} – S{s:02}E{e:02}", self.series_title),
                _ => format!("{} – {}", self.series_title, self.title),
            }
        } else {
            let title = strip_year_suffix(&self.title, self.year);
            match self.year {
                Some(y) => format!("{title} ({y})"),
                None => title.to_owned(),
            }
        }
    }
}

struct SourceUser {
    id: String,
    name: String,
    items: Vec<SourceItem>,
}

fn upstream(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, "import_failed", message)
}

fn base(url: &str) -> ApiResult<String> {
    let url = url.trim().trim_end_matches('/');
    match reqwest::Url::parse(url) {
        Ok(u) if matches!(u.scheme(), "http" | "https") && u.host().is_some() => Ok(url.to_owned()),
        _ => Err(ApiError::validation(
            "Bitte die Server-Adresse mit http:// oder https:// angeben.",
        )),
    }
}

async fn get_json(request: reqwest::RequestBuilder, what: &str) -> ApiResult<Value> {
    let response = request
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(|e| upstream(format!("{what} ist nicht erreichbar: {}", e.without_url())))?;
    match response.status().as_u16() {
        200..=299 => response
            .json()
            .await
            .map_err(|_| upstream(format!("{what} hat keine gültige Antwort geliefert."))),
        401 | 403 => Err(ApiError::validation(format!(
            "{what} hat die Anmeldung abgelehnt (Token/Schlüssel prüfen)."
        ))),
        status => Err(upstream(format!("{what} antwortet mit HTTP {status}."))),
    }
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn int_of(v: &Value, key: &str) -> Option<i64> {
    match v.get(key)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

// ---- Plex ---------------------------------------------------------------------------

/// A Plex request. Without a token Plex answers only addresses listed under
/// "Netzwerke ohne Authentifizierung" (`allowedNetworks`); any other request gets 401.
fn plex(
    http: &reqwest::Client,
    base: &str,
    path: &str,
    token: Option<&str>,
) -> reqwest::RequestBuilder {
    let request = http
        .get(format!("{base}{path}"))
        .header("Accept", "application/json")
        .header("X-Plex-Product", "Flixnet")
        .header("X-Plex-Client-Identifier", "flixnet-history-import");
    match token {
        Some(token) => request.header("X-Plex-Token", token),
        None => request,
    }
}

fn float_of(v: &Value, key: &str) -> Option<f64> {
    match v.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
    .filter(|f: &f64| f.is_finite())
}

/// Provider ids from Plex `Guid` entries (`imdb://tt…`, `tmdb://…`, `tvdb://…`) and
/// legacy agent guids (`com.plexapp.agents.imdb://tt…?lang=en`).
fn plex_ids(v: &Value) -> ExternalIds {
    let mut guids: Vec<String> = v
        .get("Guid")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|g| str_of(g, "id"))
        .collect();
    if let Some(guid) = str_of(v, "guid") {
        guids.push(guid);
    }
    let mut ids = ExternalIds::default();
    for guid in guids {
        let Some((scheme, rest)) = guid.split_once("://") else {
            continue;
        };
        let value = rest.split(['?', '/']).next().unwrap_or_default().to_owned();
        let provider = scheme.rsplit('.').next().unwrap_or(scheme);
        match provider {
            "imdb" => ids.imdb = ids.imdb.or(Some(value)),
            "tmdb" | "themoviedb" => ids.tmdb = ids.tmdb.or(Some(value)),
            "tvdb" | "thetvdb" => ids.tvdb = ids.tvdb.or(Some(value)),
            _ => {}
        }
    }
    ids
}

async fn plex_all(
    http: &reqwest::Client,
    base: &str,
    token: Option<&str>,
    section: &str,
    kind: u8,
) -> ApiResult<Vec<Value>> {
    let mut out = Vec::new();
    loop {
        let path = format!(
            "/library/sections/{section}/all?type={kind}&includeGuids=1&X-Plex-Container-Start={}&X-Plex-Container-Size={PAGE}",
            out.len()
        );
        let page = get_json(plex(http, base, &path, token), "Plex").await?;
        let container = &page["MediaContainer"];
        let entries = container["Metadata"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let got = entries.len();
        out.extend(entries);
        // Paged answers carry `totalSize`; a page may hold fewer entries than asked for.
        let done = match int_of(container, "totalSize") {
            Some(total) => out.len() >= usize::try_from(total).unwrap_or(0),
            None => got < PAGE,
        };
        if done || got == 0 {
            return Ok(out);
        }
    }
}

fn plex_state(v: &Value, item: &mut SourceItem) {
    item.play_count = int_of(v, "viewCount").unwrap_or(0);
    item.played = item.play_count > 0;
    // Watched and started again: Plex keeps the new resume point next to the count.
    item.position_ms = int_of(v, "viewOffset").unwrap_or(0);
    item.last_played_at = int_of(v, "lastViewedAt").map(|s| s * 1000);
    item.rating = float_of(v, "userRating");
}

async fn fetch_plex(
    http: &reqwest::Client,
    base: &str,
    token: Option<&str>,
) -> ApiResult<Vec<SourceItem>> {
    let sections = get_json(plex(http, base, "/library/sections", token), "Plex").await?;
    let mut items = Vec::new();
    for section in sections["MediaContainer"]["Directory"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let Some(key) = str_of(&section, "key") else {
            continue;
        };
        match section["type"].as_str() {
            Some("movie") => {
                for v in plex_all(http, base, token, &key, 1).await? {
                    let mut item = SourceItem {
                        title: str_of(&v, "title").unwrap_or_default(),
                        year: int_of(&v, "year"),
                        ids: plex_ids(&v),
                        ..Default::default()
                    };
                    plex_state(&v, &mut item);
                    items.push(item);
                }
            }
            Some("show") => {
                let shows = plex_all(http, base, token, &key, 2).await?;
                // Shows carry ratings only; their `viewCount` counts watched episodes.
                items.extend(shows.iter().map(|s| SourceItem {
                    series: true,
                    title: str_of(s, "title").unwrap_or_default(),
                    year: int_of(s, "year"),
                    ids: plex_ids(s),
                    rating: float_of(s, "userRating"),
                    ..Default::default()
                }));
                let shows: HashMap<String, Value> = shows
                    .into_iter()
                    .filter_map(|s| Some((str_of(&s, "ratingKey")?, s)))
                    .collect();
                for v in plex_all(http, base, token, &key, 4).await? {
                    let show = str_of(&v, "grandparentRatingKey").and_then(|k| shows.get(&k));
                    let mut item = SourceItem {
                        episode: true,
                        title: str_of(&v, "title").unwrap_or_default(),
                        ids: plex_ids(&v),
                        series_title: str_of(&v, "grandparentTitle")
                            .or_else(|| show.and_then(|s| str_of(s, "title")))
                            .unwrap_or_default(),
                        series_year: show.and_then(|s| int_of(s, "year")),
                        series_ids: show.map(plex_ids).unwrap_or_default(),
                        season: int_of(&v, "parentIndex"),
                        number: int_of(&v, "index"),
                        ..Default::default()
                    };
                    plex_state(&v, &mut item);
                    items.push(item);
                }
            }
            _ => {}
        }
    }
    Ok(items)
}

// ---- Jellyfin -----------------------------------------------------------------------

fn jellyfin_auth(token: Option<&str>) -> String {
    let mut header = format!(
        "MediaBrowser Client=\"Flixnet\", Device=\"Flixnet Import\", DeviceId=\"flixnet-history-import\", Version=\"{}\"",
        flixnet_core::VERSION
    );
    if let Some(token) = token {
        header.push_str(&format!(", Token=\"{token}\""));
    }
    header
}

fn jellyfin(
    http: &reqwest::Client,
    base: &str,
    path: &str,
    token: &str,
) -> reqwest::RequestBuilder {
    http.get(format!("{base}{path}"))
        .header("Accept", "application/json")
        .header("Authorization", jellyfin_auth(Some(token)))
        .header("X-Emby-Token", token)
}

fn jellyfin_ids(v: &Value) -> ExternalIds {
    let Some(map) = v.get("ProviderIds").and_then(Value::as_object) else {
        return ExternalIds::default();
    };
    ExternalIds::from_pairs(
        map.iter()
            .filter_map(|(k, v)| v.as_str().map(|v| (k.as_str(), v))),
    )
}

/// All items of the given types with the user's data. `/Items?userId=` rather than the
/// legacy `/Users/{id}/Items`, which Jellyfin 10.9+ no longer documents.
async fn jellyfin_items(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    user: &str,
    types: &str,
) -> ApiResult<Vec<Value>> {
    let mut out = Vec::new();
    loop {
        let path = format!(
            "/Items?userId={user}&Recursive=true&IncludeItemTypes={types}&Fields=ProviderIds,ProductionYear&EnableUserData=true&StartIndex={}&Limit={PAGE}",
            out.len()
        );
        let page = get_json(jellyfin(http, base, &path, token), "Jellyfin").await?;
        let entries = page["Items"].as_array().cloned().unwrap_or_default();
        let got = entries.len();
        out.extend(entries);
        let total = int_of(&page, "TotalRecordCount").map_or(usize::MAX, |t| t as usize);
        if got < PAGE || out.len() >= total {
            return Ok(out);
        }
    }
}

async fn fetch_jellyfin_user(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    user: &str,
) -> ApiResult<Vec<SourceItem>> {
    let series_list = jellyfin_items(http, base, token, user, "Series").await?;
    // Series carry favorites and ratings only; their `Played` / `PlayedPercentage`
    // summarise the episodes, which are imported one by one.
    let mut items: Vec<SourceItem> = series_list
        .iter()
        .map(|v| SourceItem {
            series: true,
            title: str_of(v, "Name").unwrap_or_default(),
            year: int_of(v, "ProductionYear"),
            ids: jellyfin_ids(v),
            favorite: v["UserData"]["IsFavorite"].as_bool().unwrap_or(false),
            rating: v["UserData"].get("Rating").and_then(Value::as_f64),
            ..Default::default()
        })
        .collect();
    let series: HashMap<String, Value> = series_list
        .into_iter()
        .filter_map(|s| Some((str_of(&s, "Id")?, s)))
        .collect();
    for v in jellyfin_items(http, base, token, user, "Movie,Episode").await? {
        let data = &v["UserData"];
        let episode = v["Type"].as_str() == Some("Episode");
        let show = str_of(&v, "SeriesId").and_then(|id| series.get(&id));
        let runtime_ms = int_of(&v, "RunTimeTicks").map(|t| t / 10_000);
        let mut position_ms = int_of(data, "PlaybackPositionTicks").map_or(0, |t| t / 10_000);
        if position_ms == 0
            && let (Some(percent), Some(runtime)) = (
                data.get("PlayedPercentage").and_then(Value::as_f64),
                runtime_ms,
            )
        {
            position_ms = (runtime as f64 * percent / 100.0) as i64;
        }
        let played = data["Played"].as_bool().unwrap_or(false);
        items.push(SourceItem {
            episode,
            title: str_of(&v, "Name").unwrap_or_default(),
            year: int_of(&v, "ProductionYear"),
            ids: jellyfin_ids(&v),
            series_title: str_of(&v, "SeriesName").unwrap_or_default(),
            series_year: show.and_then(|s| int_of(s, "ProductionYear")),
            series_ids: show.map(jellyfin_ids).unwrap_or_default(),
            season: int_of(&v, "ParentIndexNumber"),
            number: int_of(&v, "IndexNumber"),
            played,
            play_count: int_of(data, "PlayCount").unwrap_or(i64::from(played)),
            position_ms: if played { 0 } else { position_ms },
            last_played_at: str_of(data, "LastPlayedDate").and_then(|d| parse_rfc3339(&d)),
            favorite: data["IsFavorite"].as_bool().unwrap_or(false),
            rating: data.get("Rating").and_then(Value::as_f64),
            series: false,
        });
    }
    Ok(items)
}

async fn fetch_jellyfin(http: &reqwest::Client, req: &ImportRequest) -> ApiResult<Vec<SourceUser>> {
    let base = base(&req.server_url)?;
    match (&req.token, &req.username) {
        (Some(token), _) if !token.trim().is_empty() => {
            let token = token.trim();
            let users = get_json(jellyfin(http, &base, "/Users", token), "Jellyfin").await?;
            let mut out = Vec::new();
            for user in users.as_array().cloned().unwrap_or_default() {
                let (Some(id), Some(name)) = (str_of(&user, "Id"), str_of(&user, "Name")) else {
                    continue;
                };
                let items = fetch_jellyfin_user(http, &base, token, &id).await?;
                out.push(SourceUser { id, name, items });
            }
            Ok(out)
        }
        (_, Some(username)) => {
            let response = http
                .post(format!("{base}/Users/AuthenticateByName"))
                .header("Authorization", jellyfin_auth(None))
                .json(&serde_json::json!({
                    "Username": username,
                    "Pw": req.password.clone().unwrap_or_default(),
                }));
            let session = get_json(response, "Jellyfin").await?;
            let token = str_of(&session, "AccessToken")
                .ok_or_else(|| upstream("Jellyfin hat kein Zugriffstoken geliefert."))?;
            let id = str_of(&session["User"], "Id")
                .ok_or_else(|| upstream("Jellyfin hat keinen Nutzer geliefert."))?;
            let name = str_of(&session["User"], "Name").unwrap_or_else(|| username.clone());
            let items = fetch_jellyfin_user(http, &base, &token, &id).await?;
            Ok(vec![SourceUser { id, name, items }])
        }
        _ => Err(ApiError::validation(
            "Bitte einen Jellyfin-API-Schlüssel oder Name und Passwort angeben.",
        )),
    }
}

// ---- Matching and writing -----------------------------------------------------------

struct Planned {
    stats: MatchStats,
    samples: Vec<String>,
    states: Vec<ImportedState>,
}

fn plan(index: &LibraryIndex, items: &[SourceItem]) -> Planned {
    let mut stats = MatchStats::default();
    let mut samples = Vec::new();
    let mut by_identity: HashMap<String, ImportedState> = HashMap::new();
    for item in items.iter().filter(|i| i.has_state()) {
        stats.entries += 1;
        let found = if item.series {
            index.series(&item.ids, &item.title, item.year)
        } else if item.episode {
            index.episode(
                &item.ids,
                &item.series_ids,
                &item.series_title,
                item.series_year,
                item.season,
                item.number,
            )
        } else {
            index.movie(&item.ids, &item.title, item.year)
        };
        let Some((id, method)) = found else {
            stats.unmatched += 1;
            if samples.len() < SAMPLES {
                samples.push(item.label());
            }
            continue;
        };
        stats.matched += 1;
        match method {
            MatchMethod::ProviderId => stats.by_provider_id += 1,
            MatchMethod::TitleYear => stats.by_title_year += 1,
            MatchMethod::Episode => stats.by_episode += 1,
        }
        if item.played {
            stats.played += 1;
        } else if item.position_ms > 0 {
            stats.in_progress += 1;
        }
        if item.favorite {
            stats.favorites += 1;
        }
        if item.rating.is_some() {
            stats.rated += 1;
        }
        let identity = index.item(id).expect("indexed").identity.clone();
        // Two source entries for one identity (e.g. two versions): combine them.
        let entry = by_identity
            .entry(identity.clone())
            .or_insert_with(|| ImportedState {
                item_id: id,
                identity,
                ..Default::default()
            });
        entry.played |= item.played;
        entry.play_count = entry.play_count.max(item.play_count);
        if item.last_played_at > entry.last_played_at {
            entry.last_played_at = item.last_played_at;
            entry.position_ms = item.position_ms;
        } else if entry.position_ms == 0 {
            entry.position_ms = item.position_ms;
        }
        entry.favorite |= item.favorite;
        entry.rating = entry.rating.or(item.rating);
    }
    Planned {
        stats,
        samples,
        states: by_identity.into_values().collect(),
    }
}

async fn run(state: &AppState, req: ImportRequest, apply: bool) -> ApiResult<ImportResultDto> {
    let http = &state.ops.http;
    let flixnet_users = users::list(&state.db).await?;
    let user_name = |id: i64| {
        flixnet_users
            .iter()
            .find(|u| u.id == id)
            .map(|u| u.name.clone())
    };
    if let Some(id) = req.user_id
        && user_name(id).is_none()
    {
        return Err(ApiError::validation("Der Flixnet-Nutzer existiert nicht."));
    }

    // 1. Read the other server (no database transaction open meanwhile).
    let sources: Vec<SourceUser> = match req.source {
        ImportSource::Plex => {
            // Optional: a Plex server that allows this network without sign-in needs none.
            let token = req
                .token
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty());
            if req.user_id.is_none() {
                return Err(ApiError::validation(
                    "Bitte den Flixnet-Nutzer wählen, der den Verlauf bekommt.",
                ));
            }
            let items = fetch_plex(http, &base(&req.server_url)?, token).await?;
            vec![SourceUser {
                id: "plex".to_owned(),
                name: "Plex-Konto".to_owned(),
                items,
            }]
        }
        ImportSource::Jellyfin => fetch_jellyfin(http, &req).await?,
    };
    let signed_in = req.source == ImportSource::Plex
        || req.token.as_deref().is_none_or(|t| t.trim().is_empty());

    // 2. Match against the library.
    let index = LibraryIndex::load(&state.db).await?;
    let mut out = Vec::new();
    for source in sources {
        let target = if signed_in {
            req.user_id
        } else {
            req.user_map
                .iter()
                .find(|m| m.source_user_id == source.id)
                .map(|m| m.user_id)
                .or_else(|| {
                    flixnet_users
                        .iter()
                        .find(|u| u.name.to_lowercase() == source.name.to_lowercase())
                        .map(|u| u.id)
                })
        };
        let target = target.filter(|id| user_name(*id).is_some());
        let planned = plan(&index, &source.items);

        // 3. Write in batches.
        let written = match (apply, target) {
            (true, Some(user_id)) => Some(
                history::apply_import(
                    &state.db,
                    user_id,
                    match req.source {
                        ImportSource::Plex => "plex",
                        ImportSource::Jellyfin => "jellyfin",
                    },
                    match req.source {
                        ImportSource::Plex => "Plex-Import",
                        ImportSource::Jellyfin => "Jellyfin-Import",
                    },
                    &planned.states,
                )
                .await?,
            ),
            _ => None,
        };
        out.push(ImportUserDto {
            source_user_id: source.id,
            source_user_name: source.name,
            user_id: target,
            user_name: target.and_then(user_name),
            stats: planned.stats,
            unmatched_samples: planned.samples,
            written_items: written.map(|w| w.items),
            written_history: written.map(|w| w.history),
        });
    }
    if apply {
        tracing::info!(source = ?req.source, users = out.len(), "watch history imported");
    }
    Ok(ImportResultDto {
        source: req.source,
        applied: apply,
        users: out,
    })
}

/// Preview an import: what would be matched, without writing (administrators).
#[utoipa::path(
    post,
    path = "/import/preview",
    operation_id = "importPreview",
    tag = "import",
    request_body = ImportRequest,
    responses(
        (status = OK, description = "Match statistics per user", body = ImportResultDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Missing or rejected credentials", body = ErrorBody),
        (status = BAD_GATEWAY, description = "The other server failed", body = ErrorBody),
    )
)]
async fn import_preview(
    _: AdminUser,
    State(state): State<AppState>,
    Json(req): Json<ImportRequest>,
) -> ApiResult<Json<ImportResultDto>> {
    run(&state, req, false).await.map(Json)
}

/// Import watch history from Plex or Jellyfin (administrators). Repeating an import
/// adds nothing twice.
#[utoipa::path(
    post,
    path = "/import/apply",
    operation_id = "importApply",
    tag = "import",
    request_body = ImportRequest,
    responses(
        (status = OK, description = "What was written per user", body = ImportResultDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Missing or rejected credentials", body = ErrorBody),
        (status = BAD_GATEWAY, description = "The other server failed", body = ErrorBody),
    )
)]
async fn import_apply(
    _: AdminUser,
    State(state): State<AppState>,
    Json(req): Json<ImportRequest>,
) -> ApiResult<Json<ImportResultDto>> {
    run(&state, req, true).await.map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plex_guids_new_and_legacy() {
        let v = serde_json::json!({
            "guid": "plex://movie/5d776825880197001ec967c6",
            "Guid": [{ "id": "imdb://tt0133093" }, { "id": "tmdb://603" }, { "id": "tvdb://169" }]
        });
        let ids = plex_ids(&v);
        assert_eq!(ids.imdb.as_deref(), Some("tt0133093"));
        assert_eq!(ids.tmdb.as_deref(), Some("603"));
        assert_eq!(ids.tvdb.as_deref(), Some("169"));

        let legacy =
            serde_json::json!({ "guid": "com.plexapp.agents.thetvdb://121361/1/2?lang=de" });
        assert_eq!(plex_ids(&legacy).tvdb.as_deref(), Some("121361"));
        let legacy = serde_json::json!({ "guid": "com.plexapp.agents.imdb://tt0816692?lang=en" });
        assert_eq!(plex_ids(&legacy).imdb.as_deref(), Some("tt0816692"));

        // Unmatched items (PMS 1.43): no provider ids at all.
        for guid in [
            "local://2",
            "tv.plex.agents.none://35",
            "com.plexapp.agents.none://a1b2c3?lang=xn",
        ] {
            assert!(
                plex_ids(&serde_json::json!({ "guid": guid })).is_empty(),
                "{guid}"
            );
        }
    }

    #[test]
    fn plex_numbers_as_numbers_or_strings() {
        let v = serde_json::json!({ "a": 8.0, "b": "6", "c": "x", "d": null });
        assert_eq!(float_of(&v, "a"), Some(8.0));
        assert_eq!(float_of(&v, "b"), Some(6.0));
        assert_eq!(float_of(&v, "c"), None);
        assert_eq!(float_of(&v, "d"), None);
        assert_eq!(float_of(&v, "e"), None);
    }
}
