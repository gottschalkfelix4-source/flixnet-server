//! `/api/v1/music/…`: album and artist pages, track lists, lyrics, playback sources,
//! converted streams and play counts ("scrobbles").
//!
//! Artist and album *grids* use the general `GET /items?kind=artist|album` (paging,
//! sorting, letters and search for free). Everything here is limited to the libraries
//! the caller may see and to their parental controls; music counts as unrated, which
//! restricted profiles see (see `flixnet_db::browse`), and tracks tagged explicit
//! carry an age rating of 16.

use std::collections::BTreeMap;
use std::path::{Path as FsPath, PathBuf};

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use flixnet_db::browse::{self, ItemFilter, Sort};
use flixnet_db::music::{self, TrackRow};
use flixnet_db::{items, media, metadata, playstate};
use flixnet_library::music::lyrics;
use flixnet_media::audio::{AacJob, AudioMethod, AudioTranscoder, decide_audio};
use flixnet_media::decision::{DeviceProfile, Reason};
use serde::{Deserialize, Serialize};
use tower_http::services::ServeFile;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AuthUser;
use crate::browse::{ItemCardDto, SortOrder, card, ensure_visible, visible_libraries};
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::playback::{StreamQuery, accessible_file, source_media};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_album))
        .routes(routes!(get_artist))
        .routes(routes!(list_tracks))
        .routes(routes!(get_lyrics))
        .routes(routes!(scrobble))
        .routes(routes!(playback_sources))
        .routes(routes!(stream_converted))
}

/// Tracks per request at most (album pages, playback sources).
const MAX_TRACKS: i64 = 1000;
/// A listen counts once it covers half the track or four minutes (Last.fm rule).
const SCROBBLE_MAX_MS: i64 = 4 * 60_000;
/// …and without a known duration, after 30 seconds.
const SCROBBLE_UNKNOWN_MS: i64 = 30_000;

#[derive(Debug, Serialize, ToSchema)]
pub struct TrackDto {
    pub id: i64,
    pub library_id: i64,
    pub title: String,
    /// Track artist(s) as tagged, else the album artist.
    pub artist: String,
    pub album_id: Option<i64>,
    pub album_title: Option<String>,
    pub album_artist_id: Option<i64>,
    pub album_artist: Option<String>,
    pub disc_number: Option<i64>,
    pub track_number: Option<i64>,
    pub year: Option<i64>,
    pub duration_ms: Option<i64>,
    /// Lyrics are available (`GET /music/tracks/{id}/lyrics`).
    pub has_lyrics: bool,
    pub favorite: bool,
    /// Times the signed-in user listened to it.
    pub play_count: i64,
    pub last_played_at: Option<i64>,
    /// Signed album cover URL, 342 px wide.
    pub cover_url: Option<String>,
    pub cover_blurhash: Option<String>,
}

fn track_dto(state: &AppState, row: TrackRow) -> TrackDto {
    TrackDto {
        artist: row
            .artist
            .clone()
            .or_else(|| row.album_artist.clone())
            .unwrap_or_default(),
        cover_url: row.cover_id.map(|id| state.signer.image_url(id, Some(342))),
        cover_blurhash: row.cover_blurhash,
        id: row.id,
        library_id: row.library_id,
        title: row.title,
        album_id: row.album_id,
        album_title: row.album_title,
        album_artist_id: row.album_artist_id,
        album_artist: row.album_artist,
        disc_number: row.disc_number,
        track_number: row.track_number,
        year: row.year,
        duration_ms: row.duration_ms,
        has_lyrics: row.has_lyrics,
        favorite: row.favorite,
        play_count: row.play_count,
        last_played_at: row.last_played_at,
    }
}

/// Visible items of one kind below a parent (or by ids), in the given order.
async fn visible_ids(
    state: &AppState,
    user: &AuthUser,
    mut filter: ItemFilter,
) -> ApiResult<(Vec<i64>, i64)> {
    filter.library_ids = visible_libraries(state, user, None).await?;
    filter.restrict(&user.restriction);
    filter.user_id = Some(user.user_id);
    let (rows, total) = browse::list(&state.db, &filter).await?;
    Ok((rows.into_iter().map(|r| r.id).collect(), total))
}

async fn tracks_of(state: &AppState, user: &AuthUser, ids: &[i64]) -> ApiResult<Vec<TrackDto>> {
    Ok(music::track_rows(&state.db, ids, Some(user.user_id))
        .await?
        .into_iter()
        .map(|r| track_dto(state, r))
        .collect())
}

async fn cards(state: &AppState, user: &AuthUser, ids: &[i64]) -> ApiResult<Vec<ItemCardDto>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut rows = browse::summaries(&state.db, ids, Some(user.user_id)).await?;
    let position: std::collections::HashMap<i64, usize> =
        ids.iter().enumerate().map(|(p, id)| (*id, p)).collect();
    rows.sort_by_key(|r| position.get(&r.id).copied().unwrap_or(usize::MAX));
    Ok(rows.into_iter().map(|r| card(state, r)).collect())
}

/// A visible item of the given kind, or 404.
async fn visible_item(
    state: &AppState,
    user: &AuthUser,
    id: i64,
    kind: &str,
) -> ApiResult<items::ItemDetails> {
    ensure_visible(state, user, id).await?;
    items::details(&state.db, id)
        .await?
        .filter(|i| i.kind == kind)
        .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DiscDto {
    /// Disc number (1 for single-disc albums).
    pub number: i64,
    pub tracks: Vec<TrackDto>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AlbumDetailDto {
    #[serde(flatten)]
    pub card: ItemCardDto,
    /// Album artist (name in `series_title`).
    pub artist_id: Option<i64>,
    pub genres: Vec<String>,
    pub release_date: Option<String>,
    /// Signed cover URL, 780 px wide.
    pub cover_url: Option<String>,
    pub discs: Vec<DiscDto>,
    pub track_count: i64,
    pub duration_ms: i64,
    /// e.g. `{"musicbrainz_album": "…"}`.
    pub provider_ids: BTreeMap<String, String>,
}

/// An album with its tracks, grouped by disc.
#[utoipa::path(
    get,
    path = "/music/albums/{id}",
    operation_id = "getMusicAlbum",
    tag = "music",
    params(("id" = i64, Path, description = "Album item id")),
    responses(
        (status = OK, description = "The album", body = AlbumDetailDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) album", body = ErrorBody),
    )
)]
async fn get_album(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<AlbumDetailDto>> {
    let details = visible_item(&state, &user, id, "album").await?;
    let (ids, _) = visible_ids(
        &state,
        &user,
        ItemFilter {
            kinds: vec!["track".to_owned()],
            parent_id: Some(id),
            sort: Sort::Index,
            limit: MAX_TRACKS,
            ..Default::default()
        },
    )
    .await?;
    let tracks = tracks_of(&state, &user, &ids).await?;
    let mut discs: Vec<DiscDto> = Vec::new();
    let mut duration_ms = 0;
    let track_count = tracks.len() as i64;
    for track in tracks {
        duration_ms += track.duration_ms.unwrap_or(0);
        let number = track.disc_number.unwrap_or(1);
        match discs.last_mut() {
            Some(disc) if disc.number == number => disc.tracks.push(track),
            _ => discs.push(DiscDto {
                number,
                tracks: vec![track],
            }),
        }
    }
    let card = cards(&state, &user, &[id])
        .await?
        .pop()
        .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
    let cover = metadata::images(&state.db, id)
        .await?
        .into_iter()
        .find(|i| i.kind == "poster");
    Ok(Json(AlbumDetailDto {
        artist_id: details.parent_id,
        genres: metadata::genres(&state.db, id).await?,
        release_date: details.premiere_date,
        cover_url: cover.map(|c| state.signer.image_url(c.id, Some(780))),
        discs,
        track_count,
        duration_ms,
        provider_ids: items::provider_ids(&state.db, id)
            .await?
            .into_iter()
            .collect(),
        card,
    }))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ArtistDetailDto {
    #[serde(flatten)]
    pub card: ItemCardDto,
    /// Signed backdrop URL (fanart.tv or a local `fanart.jpg`), 1920 px wide.
    pub backdrop_url: Option<String>,
    pub logo_url: Option<String>,
    /// The artist's own albums, newest first.
    pub albums: Vec<ItemCardDto>,
    /// Other artists' albums (compilations) with tracks by this artist.
    pub appears_on: Vec<ItemCardDto>,
    /// Most played tracks (all profiles of the household), then album order.
    pub popular: Vec<TrackDto>,
    pub provider_ids: BTreeMap<String, String>,
}

/// An artist with albums, appearances and popular tracks.
#[utoipa::path(
    get,
    path = "/music/artists/{id}",
    operation_id = "getMusicArtist",
    tag = "music",
    params(("id" = i64, Path, description = "Artist item id")),
    responses(
        (status = OK, description = "The artist", body = ArtistDetailDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) artist", body = ErrorBody),
    )
)]
async fn get_artist(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<ArtistDetailDto>> {
    let details = visible_item(&state, &user, id, "artist").await?;
    let library_ids = visible_libraries(&state, &user, None).await?;
    let (album_ids, _) = visible_ids(
        &state,
        &user,
        ItemFilter {
            kinds: vec!["album".to_owned()],
            parent_id: Some(id),
            sort: Sort::Year,
            descending: true,
            limit: 200,
            ..Default::default()
        },
    )
    .await?;
    let appears = music::appears_on(
        &state.db,
        id,
        &details.title,
        &library_ids,
        &user.restriction,
    )
    .await?;
    let popular = music::popular_tracks(
        &state.db,
        id,
        &details.title,
        &library_ids,
        &user.restriction,
        10,
    )
    .await?;
    let images = metadata::images(&state.db, id).await?;
    let best = |kind: &str| images.iter().find(|i| i.kind == kind);
    let card = cards(&state, &user, &[id])
        .await?
        .pop()
        .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
    Ok(Json(ArtistDetailDto {
        backdrop_url: best("backdrop").map(|i| state.signer.image_url(i.id, Some(1920))),
        logo_url: best("logo").map(|i| state.signer.image_url(i.id, Some(780))),
        albums: cards(&state, &user, &album_ids).await?,
        appears_on: cards(&state, &user, &appears).await?,
        popular: tracks_of(&state, &user, &popular).await?,
        provider_ids: items::provider_ids(&state.db, id)
            .await?
            .into_iter()
            .collect(),
        card,
    }))
}

#[derive(Debug, Clone, Copy, Default, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrackSort {
    /// Album order (disc, track); the default for `album_id`.
    Index,
    #[default]
    Title,
    DateAdded,
    Year,
    Relevance,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TracksQuery {
    pub library_id: Option<i64>,
    pub album_id: Option<i64>,
    /// Tracks on the albums of this album artist.
    pub artist_id: Option<i64>,
    /// Free-text search over track titles.
    pub search: Option<String>,
    pub genre: Option<String>,
    /// Only favorites.
    pub favorite: Option<bool>,
    #[param(inline)]
    pub sort: Option<TrackSort>,
    #[param(inline)]
    pub order: Option<SortOrder>,
    /// 1–500, default 100.
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TrackPage {
    pub items: Vec<TrackDto>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

/// Tracks: of an album or artist, favorites, a genre or a search.
#[utoipa::path(
    get,
    path = "/music/tracks",
    operation_id = "listMusicTracks",
    tag = "music",
    params(TracksQuery),
    responses(
        (status = OK, description = "One page of tracks", body = TrackPage),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn list_tracks(
    user: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<TracksQuery>,
) -> ApiResult<Json<TrackPage>> {
    let search = query
        .search
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| browse::fts_query(s).unwrap_or_else(|| "\"\"".to_owned()));
    let default_sort = if search.is_some() {
        TrackSort::Relevance
    } else if query.album_id.is_some() || query.artist_id.is_some() {
        TrackSort::Index
    } else {
        TrackSort::Title
    };
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let offset = query.offset.unwrap_or(0).max(0);
    let mut filter = ItemFilter {
        kinds: vec!["track".to_owned()],
        parent_id: query.album_id,
        series_id: query.artist_id,
        search,
        genre: query.genre.clone().filter(|g| !g.is_empty()),
        is_favorite: query.favorite,
        sort: match query.sort.unwrap_or(default_sort) {
            TrackSort::Index => Sort::Index,
            TrackSort::Title => Sort::Title,
            TrackSort::DateAdded => Sort::DateAdded,
            TrackSort::Year => Sort::Year,
            TrackSort::Relevance => Sort::Relevance,
        },
        descending: matches!(query.order, Some(SortOrder::Desc)),
        limit,
        offset,
        ..Default::default()
    };
    filter.library_ids = visible_libraries(&state, &user, query.library_id).await?;
    filter.restrict(&user.restriction);
    filter.user_id = Some(user.user_id);
    let (rows, total) = browse::list(&state.db, &filter).await?;
    let ids: Vec<i64> = rows.into_iter().map(|r| r.id).collect();
    Ok(Json(TrackPage {
        items: tracks_of(&state, &user, &ids).await?,
        total,
        offset,
        limit,
    }))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LyricLineDto {
    /// Start of the line in milliseconds (0 for plain lyrics).
    pub time_ms: i64,
    pub text: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LyricsDto {
    /// Lines carry timestamps (LRC) and can follow playback.
    pub synced: bool,
    pub lines: Vec<LyricLineDto>,
    /// `sidecar` (`.lrc` next to the file) or `embedded` (in the tags).
    pub source: String,
}

/// Lyrics of a track: a sidecar `.lrc` file (synced) or lyrics embedded in the tags.
#[utoipa::path(
    get,
    path = "/music/tracks/{id}/lyrics",
    operation_id = "getTrackLyrics",
    tag = "music",
    params(("id" = i64, Path, description = "Track item id")),
    responses(
        (status = OK, description = "The lyrics", body = LyricsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) track, or no lyrics", body = ErrorBody),
    )
)]
async fn get_lyrics(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<LyricsDto>> {
    visible_item(&state, &user, id, "track").await?;
    let no_lyrics = || ApiError::not_found("Für diesen Titel gibt es keinen Liedtext.");
    let file_id = music::track_rows(&state.db, &[id], None)
        .await?
        .pop()
        .and_then(|r| r.file_id)
        .ok_or_else(no_lyrics)?;
    let file = media::get_file(&state.db, file_id)
        .await?
        .ok_or_else(no_lyrics)?;
    let path = PathBuf::from(&file.path);
    let found = tokio::task::spawn_blocking(move || lyrics::find(&path))
        .await?
        .ok_or_else(no_lyrics)?;
    Ok(Json(LyricsDto {
        synced: found.synced,
        lines: found
            .lines
            .into_iter()
            .map(|l| LyricLineDto {
                time_ms: l.time_ms,
                text: l.text,
            })
            .collect(),
        source: found.source.to_owned(),
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ScrobbleRequest {
    /// How long the track was actually listened to (milliseconds, pauses excluded).
    pub listened_ms: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ScrobbleDto {
    /// The listen was long enough to count as a play.
    pub counted: bool,
    pub play_count: i64,
}

/// Reports a finished (or skipped) listen. It counts as a play — play count, last
/// played, listening history — once it covers half the track or four minutes.
#[utoipa::path(
    post,
    path = "/music/tracks/{id}/scrobble",
    operation_id = "scrobbleTrack",
    tag = "music",
    params(("id" = i64, Path, description = "Track item id")),
    request_body = ScrobbleRequest,
    responses(
        (status = OK, description = "Recorded", body = ScrobbleDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) track", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Negative duration", body = ErrorBody),
    )
)]
async fn scrobble(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<ScrobbleRequest>,
) -> ApiResult<Json<ScrobbleDto>> {
    visible_item(&state, &user, id, "track").await?;
    if body.listened_ms < 0 {
        return Err(ApiError::validation(
            "Die Hördauer darf nicht negativ sein.",
        ));
    }
    let duration = playstate::item_duration(&state.db, id).await?;
    let needed = match duration {
        Some(d) => (d / 2).min(SCROBBLE_MAX_MS),
        None => SCROBBLE_UNKNOWN_MS,
    };
    let counted = body.listened_ms >= needed;
    if counted {
        music::record_play(&state.db, user.user_id, id).await?;
        playstate::record_event(
            &state.db,
            &playstate::NewWatchEvent {
                user_id: user.user_id,
                item_id: id,
                event: "stop",
                position_ms: body.listened_ms,
                duration_ms: duration,
                play_method: Some("music"),
                completed: true,
                device_id: Some(user.device_id),
                ..Default::default()
            },
        )
        .await?;
    }
    let play_count = music::track_rows(&state.db, &[id], Some(user.user_id))
        .await?
        .pop()
        .map_or(0, |r| r.play_count);
    Ok(Json(ScrobbleDto {
        counted,
        play_count,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct MusicPlaybackRequest {
    /// Tracks to play (at most 1000), e.g. the current and the next queue entry.
    pub track_ids: Vec<i64>,
    /// What the client plays (from `canPlayType`).
    #[serde(default)]
    pub profile: DeviceProfile,
    /// Bitrate limit (bits/s); larger files are converted.
    pub max_bitrate: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ReplayGainDto {
    pub track_gain_db: Option<f64>,
    pub track_peak: Option<f64>,
    pub album_gain_db: Option<f64>,
    pub album_peak: Option<f64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TrackSourceDto {
    pub track_id: i64,
    pub file_id: i64,
    /// Signed URL to play (original file or AAC conversion).
    pub url: String,
    pub method: AudioMethod,
    /// Why the file is converted (empty for Direct Play).
    pub reasons: Vec<Reason>,
    /// Container (file extension) and codec of the original.
    pub container: String,
    pub codec: Option<String>,
    pub sample_rate: Option<i64>,
    pub bitrate: Option<i64>,
    pub duration_ms: Option<i64>,
    pub replaygain: ReplayGainDto,
}

/// How to play tracks on this client: Direct Play or AAC conversion, with ReplayGain.
/// Tracks that are not visible (or have no file) are left out.
#[utoipa::path(
    post,
    path = "/music/playback",
    operation_id = "getMusicPlayback",
    tag = "music",
    request_body = MusicPlaybackRequest,
    responses(
        (status = OK, description = "One source per playable track, in request order", body = Vec<TrackSourceDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Too many tracks", body = ErrorBody),
    )
)]
async fn playback_sources(
    user: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<MusicPlaybackRequest>,
) -> ApiResult<Json<Vec<TrackSourceDto>>> {
    if body.track_ids.len() as i64 > MAX_TRACKS {
        return Err(ApiError::validation("Höchstens 1000 Titel auf einmal."));
    }
    if body.track_ids.is_empty() {
        return Ok(Json(Vec::new()));
    }
    let (visible, _) = visible_ids(
        &state,
        &user,
        ItemFilter {
            kinds: vec!["track".to_owned()],
            ids: body.track_ids.clone(),
            limit: MAX_TRACKS,
            ..Default::default()
        },
    )
    .await?;
    let ordered: Vec<i64> = body
        .track_ids
        .iter()
        .copied()
        .filter(|id| visible.contains(id))
        .collect();
    let mut out = Vec::new();
    for row in music::track_rows(&state.db, &ordered, None).await? {
        let Some(file) = (match row.file_id {
            Some(id) => media::get_file(&state.db, id).await?,
            None => None,
        }) else {
            continue;
        };
        let streams = media::streams(&state.db, file.id).await?;
        let source = source_media(&file, &streams);
        let decision = decide_audio(&source, &body.profile, body.max_bitrate);
        let audio = streams.iter().find(|s| s.kind == "audio");
        out.push(TrackSourceDto {
            track_id: row.id,
            file_id: file.id,
            url: match decision.method {
                AudioMethod::DirectPlay => state.signer.stream_url(file.id, user.user_id),
                AudioMethod::Transcode => state.signer.audio_url(file.id, user.user_id),
            },
            method: decision.method,
            reasons: decision.reasons,
            container: source.container,
            codec: audio.and_then(|a| a.codec.clone()),
            sample_rate: audio.and_then(|a| a.sample_rate),
            bitrate: file.bitrate,
            duration_ms: file.duration_ms.or(row.duration_ms),
            replaygain: ReplayGainDto {
                track_gain_db: row.track_gain_db,
                track_peak: row.track_peak,
                album_gain_db: row.album_gain_db,
                album_peak: row.album_peak,
            },
        });
    }
    Ok(Json(out))
}

/// A track converted to AAC (`audio/mp4`), with HTTP range support. Converted once,
/// then served from the cache.
///
/// Use the signed `url` from `POST /music/playback`; alternatively authenticate normally.
#[utoipa::path(
    get,
    path = "/music/stream/{file_id}",
    operation_id = "streamMusicConverted",
    tag = "music",
    security(()),
    params(("file_id" = i64, Path, description = "Media file id of a track"), StreamQuery),
    responses(
        (status = OK, description = "The converted track", content_type = "audio/mp4"),
        (status = PARTIAL_CONTENT, description = "The requested byte range", content_type = "audio/mp4"),
        (status = UNAUTHORIZED, description = "Neither a valid signature nor a valid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such track file, or no access", body = ErrorBody),
    )
)]
async fn stream_converted(
    user: Option<AuthUser>,
    State(state): State<AppState>,
    Path(file_id): Path<i64>,
    Query(query): Query<StreamQuery>,
    request: Request,
) -> ApiResult<Response> {
    let signed_user = match (query.u, query.exp, query.sig.as_deref()) {
        (Some(u), Some(exp), Some(sig)) if state.signer.verify_audio(file_id, u, exp, sig) => {
            Some(u)
        }
        _ => None,
    };
    let file = accessible_file(&state, user.as_ref(), signed_user, file_id).await?;
    let not_found = || ApiError::not_found("Datei nicht gefunden.");
    // Only tracks: this endpoint converts audio, not films.
    let item_id = file.item_id.ok_or_else(not_found)?;
    if items::get(&state.db, item_id)
        .await?
        .is_none_or(|i| i.kind != "track")
    {
        return Err(not_found());
    }
    let streams = media::streams(&state.db, file.id).await?;
    let audio = streams
        .iter()
        .find(|s| s.kind == "audio")
        .ok_or_else(not_found)?;
    let transcoder = AudioTranscoder::new(
        &state.config.media.ffmpeg,
        state.config.paths.config_dir.join("cache").join("audio"),
    );
    let input = FsPath::new(&file.path);
    let converted = transcoder
        .aac(
            &format!("{}-{}-{}", file.id, file.size, file.mtime),
            &AacJob {
                input,
                stream_index: Some(audio.idx),
                channels: audio.channels.unwrap_or(2),
                sample_rate: audio.sample_rate,
            },
        )
        .await
        .map_err(|err| {
            tracing::warn!(file = file.id, %err, "audio conversion failed");
            ApiError::not_found("Der Titel konnte nicht umgewandelt werden.")
        })?;
    let response = ServeFile::new_with_mime(&converted, &"audio/mp4".parse().expect("valid mime"))
        .try_call(request)
        .await
        .map_err(ApiError::internal)?;
    if response.status() == StatusCode::NOT_FOUND {
        return Err(not_found());
    }
    let mut response = response.map(Body::new);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-transform"),
    );
    Ok(response)
}
