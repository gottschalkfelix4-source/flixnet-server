//! Flixnet rows → Jellyfin DTOs.

use std::collections::{BTreeMap, HashMap, HashSet};

use flixnet_api::auth::AuthUser;
use flixnet_api::browse::visible_libraries;
use flixnet_api::error::{ApiError, ApiResult};
use flixnet_db::browse::{self, ItemSummary, PersonRow};
use flixnet_db::{items, libraries, media, metadata};
use flixnet_media::subtitles::is_text_subtitle;

use crate::JfState;
use crate::dto::{
    BaseItemDto, BaseItemPerson, ChapterInfo, MediaSourceInfo, MediaStream, NameGuidPair,
    UserItemDataDto,
};
use crate::ids::{self, Kind};
use crate::util::{Params, iso, iso_date, ticks};

/// Which optional members to fill (`Fields=…`); detail views get everything.
#[derive(Debug, Clone, Default)]
pub(crate) struct Fields {
    set: HashSet<String>,
    all: bool,
}

impl Fields {
    pub fn from_params(params: &Params) -> Self {
        Self {
            set: params.set("fields"),
            all: false,
        }
    }

    pub fn all() -> Self {
        Self {
            set: HashSet::new(),
            all: true,
        }
    }

    pub fn has(&self, name: &str) -> bool {
        self.all || self.set.contains(name)
    }
}

/// Jellyfin's `BaseItemKind` for a Flixnet item kind.
pub(crate) fn item_type(kind: &str) -> &'static str {
    match kind {
        "movie" => "Movie",
        "series" => "Series",
        "season" => "Season",
        "episode" => "Episode",
        "collection" => "BoxSet",
        _ => "Folder",
    }
}

/// Flixnet's item kind for a Jellyfin `BaseItemKind` (only those Flixnet has).
pub(crate) fn flixnet_kind(jellyfin_type: &str) -> Option<&'static str> {
    match jellyfin_type.to_ascii_lowercase().as_str() {
        "movie" => Some("movie"),
        "series" => Some("series"),
        "season" => Some("season"),
        "episode" => Some("episode"),
        _ => None,
    }
}

pub(crate) fn collection_type(library_kind: &str) -> &'static str {
    match library_kind {
        "movies" => "movies",
        "shows" => "tvshows",
        "music" => "music",
        _ => "homevideos",
    }
}

/// Signed, non-expiring image tag: clients put it into image URLs they load without
/// credentials, so it doubles as the permission to fetch images of this item. The
/// signature covers the item (clients such as Findroid reuse one tag for several
/// image types of a series); the image id is appended so the URL changes with the
/// image and caches stay correct.
pub(crate) fn image_tag(state: &JfState, item_id: i64, image_id: i64) -> String {
    format!("{}{image_id:x}", state.app.signer.tag("jfimg", item_id))
}

/// Does `tag` (from [`image_tag`]) grant access to the item's images?
pub(crate) fn verify_image_tag(state: &JfState, item_id: i64, tag: &str) -> bool {
    tag.get(..32)
        .is_some_and(|sig| state.app.signer.verify_tag("jfimg", item_id, sig))
}

pub(crate) fn person_tag(state: &JfState, person_id: i64) -> String {
    state.app.signer.tag("jfperson", person_id)
}

pub(crate) fn user_data(item_guid: &str, s: &ItemSummary) -> UserItemDataDto {
    let runtime = s.runtime_ms.filter(|r| *r > 0);
    UserItemDataDto {
        played_percentage: match (s.position_ms, runtime) {
            (p, Some(r)) if p > 0 => Some((p as f64 * 100.0 / r as f64).min(100.0)),
            _ => None,
        },
        playback_position_ticks: ticks(s.position_ms),
        play_count: i64::from(s.played),
        is_favorite: s.favorite,
        played: s.played,
        key: s.id.to_string(),
        item_id: item_guid.to_owned(),
        ..Default::default()
    }
}

/// Item ids of Jellyfin GUIDs, ignoring anything that is not an item.
pub(crate) fn item_ids(values: &[String]) -> Vec<i64> {
    values
        .iter()
        .filter_map(|v| ids::parse_as(v, Kind::Item))
        .collect()
}

/// Images of a series needed by its seasons and episodes.
struct SeriesArt {
    poster: Option<i64>,
    backdrop: Option<i64>,
    logo: Option<i64>,
    thumb: Option<i64>,
}

/// The best image of a kind (images come ordered best first).
fn best(images: &[metadata::Image], kind: &str) -> Option<i64> {
    images.iter().find(|i| i.kind == kind).map(|i| i.id)
}

/// Cards for a page of summaries.
pub(crate) async fn item_dtos(
    state: &JfState,
    user: &AuthUser,
    summaries: Vec<ItemSummary>,
    fields: &Fields,
) -> ApiResult<Vec<BaseItemDto>> {
    let db = &state.app.db;
    let series_ids: Vec<i64> = summaries
        .iter()
        .filter(|s| s.kind == "season" || s.kind == "episode")
        .filter_map(|s| s.series_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let mut series_art = HashMap::new();
    for id in series_ids {
        let images = metadata::images(db, id).await?;
        series_art.insert(
            id,
            SeriesArt {
                poster: best(&images, "poster"),
                backdrop: best(&images, "backdrop"),
                logo: best(&images, "logo"),
                thumb: best(&images, "thumb"),
            },
        );
    }
    let genre_ids: HashMap<String, i64> = if fields.has("genres") {
        let libraries = visible_libraries(&state.app, user, None).await?;
        browse::genres_with_ids(db, &libraries)
            .await?
            .into_iter()
            .map(|(id, name, _)| (name, id))
            .collect()
    } else {
        HashMap::new()
    };

    let mut out = Vec::with_capacity(summaries.len());
    for s in summaries {
        let mut dto = base_dto(state, &s);
        if let Some((series, art)) = s
            .series_id
            .and_then(|id| series_art.get(&id).map(|art| (id, art)))
        {
            let series_guid = s.series_id.map(|id| ids::guid(Kind::Item, id));
            dto.series_primary_image_tag = art.poster.map(|id| image_tag(state, series, id));
            if let Some(backdrop) = art.backdrop {
                dto.parent_backdrop_item_id = series_guid.clone();
                dto.parent_backdrop_image_tags = Some(vec![image_tag(state, series, backdrop)]);
            }
            if let Some(logo) = art.logo {
                dto.parent_logo_item_id = series_guid.clone();
                dto.parent_logo_image_tag = Some(image_tag(state, series, logo));
            }
            if let Some(thumb) = art.thumb {
                dto.parent_thumb_item_id = series_guid;
                dto.parent_thumb_image_tag = Some(image_tag(state, series, thumb));
            }
        }
        dto.provider_ids = Some(provider_ids(db, s.id).await?);
        if fields.has("genres") {
            let names = metadata::genres(db, s.id).await?;
            dto.genre_items = Some(
                names
                    .iter()
                    .map(|name| NameGuidPair {
                        name: name.clone(),
                        id: genre_ids
                            .get(name)
                            .map(|id| ids::guid(Kind::Genre, *id))
                            .unwrap_or_else(|| ids::guid(Kind::Genre, ids::name_id(name))),
                    })
                    .collect(),
            );
            dto.genres = Some(names);
        }
        if fields.has("studios") {
            dto.studios = Some(
                metadata::studios(db, s.id)
                    .await?
                    .into_iter()
                    .map(|name| NameGuidPair {
                        id: ids::guid(Kind::Studio, ids::name_id(&name)),
                        name,
                    })
                    .collect(),
            );
        }
        if fields.has("people") {
            dto.people = Some(people(state, s.id).await?);
        }
        let is_video = s.kind == "movie" || s.kind == "episode";
        if is_video
            && (fields.has("mediasources")
                || fields.has("mediastreams")
                || fields.has("path")
                || fields.has("chapters"))
        {
            let sources = static_media_sources(state, s.id, s.runtime_ms).await?;
            dto.media_source_count = Some(sources.len() as i64);
            if let Some(first) = sources.first() {
                dto.container = first.container.clone();
                dto.path = first.path.clone();
                dto.has_subtitles = Some(first.media_streams.iter().any(|m| m.kind == "Subtitle"));
                if fields.has("mediastreams") {
                    dto.media_streams = Some(first.media_streams.clone());
                }
                let video = first.media_streams.iter().find(|m| m.kind == "Video");
                dto.width = video.and_then(|v| v.width);
                dto.height = video.and_then(|v| v.height);
            }
            if fields.has("chapters") {
                dto.chapters = Some(chapters(db, s.id).await?);
            }
            if fields.has("mediasources") {
                dto.media_sources = Some(sources);
            }
        }
        if (fields.has("sortname") || fields.has("originaltitle") || fields.has("taglines"))
            && let Some(details) = items::details(db, s.id).await?
        {
            dto.sort_name = Some(details.sort_title);
            dto.original_title = details.original_title;
            dto.taglines = details.tagline.map(|t| vec![t]);
        }
        out.push(dto);
    }
    Ok(out)
}

/// Everything about one item (detail pages).
pub(crate) async fn item_detail(
    state: &JfState,
    user: &AuthUser,
    id: i64,
) -> ApiResult<BaseItemDto> {
    let db = &state.app.db;
    let summary = browse::summaries(db, &[id], Some(user.user_id))
        .await?
        .pop()
        .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
    let kind = summary.kind.clone();
    let parent = summary.parent_id;
    let mut dto = item_dtos(state, user, vec![summary], &Fields::all())
        .await?
        .pop()
        .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;

    // Every image the item has, not just the card's.
    let images = metadata::images(db, id).await?;
    for (kind, name) in [("logo", "Logo"), ("thumb", "Thumb"), ("banner", "Banner")] {
        if let Some(image) = best(&images, kind) {
            dto.image_tags
                .entry(name.to_owned())
                .or_insert_with(|| image_tag(state, id, image));
        }
    }
    let backdrops: Vec<String> = images
        .iter()
        .filter(|i| i.kind == "backdrop")
        .map(|i| image_tag(state, id, i.id))
        .collect();
    if !backdrops.is_empty() {
        dto.backdrop_image_tags = backdrops;
    }
    if kind == "episode"
        && let Some(season) = parent
        && let Some(season) = items::get(db, season).await?
    {
        dto.season_name = Some(season.title);
    }
    dto.local_trailer_count = Some(0);
    dto.special_feature_count = Some(0);
    Ok(dto)
}

/// The card part every item has; extra members are added by the caller.
pub(crate) fn base_dto(state: &JfState, s: &ItemSummary) -> BaseItemDto {
    let guid = ids::guid(Kind::Item, s.id);
    let is_video = s.kind == "movie" || s.kind == "episode";
    let is_folder = s.kind == "series" || s.kind == "season";
    let mut image_tags = BTreeMap::new();
    let mut blur_hashes = BTreeMap::new();
    if let Some(poster) = s.poster_id {
        let tag = image_tag(state, s.id, poster);
        if let Some(hash) = &s.poster_blurhash {
            blur_hashes.insert(
                "Primary".to_owned(),
                BTreeMap::from([(tag.clone(), hash.clone())]),
            );
        }
        image_tags.insert("Primary".to_owned(), tag);
    }
    let parent_id = match s.kind.as_str() {
        "episode" | "season" => s.parent_id.map(|id| ids::guid(Kind::Item, id)),
        _ => Some(ids::guid(Kind::Library, s.library_id)),
    };
    BaseItemDto {
        name: s.title.clone(),
        server_id: state.server_id.to_string(),
        id: guid.clone(),
        etag: Some(format!("{:x}", s.date_added)),
        date_created: Some(iso(s.date_added)),
        can_delete: Some(false),
        can_download: Some(is_video),
        premiere_date: s.premiere_date.as_deref().and_then(iso_date),
        external_urls: Some(Vec::new()),
        official_rating: s.official_rating.clone(),
        overview: s.overview.clone(),
        community_rating: s.community_rating,
        run_time_ticks: s.runtime_ms.filter(|r| *r > 0).map(ticks),
        production_year: s.year,
        index_number: s.index_number,
        index_number_end: s.index_end,
        parent_index_number: s.parent_index_number,
        remote_trailers: Some(Vec::new()),
        is_folder,
        parent_id,
        kind: item_type(&s.kind),
        user_data: Some(user_data(&guid, s)),
        child_count: is_folder.then_some(s.child_count),
        series_name: s.series_title.clone(),
        series_id: s
            .series_id
            .filter(|id| *id != s.id)
            .map(|id| ids::guid(Kind::Item, id)),
        season_id: (s.kind == "episode")
            .then(|| s.parent_id.map(|id| ids::guid(Kind::Item, id)))
            .flatten(),
        season_name: (s.kind == "episode")
            .then(|| s.parent_index_number.map(|n| format!("Staffel {n}")))
            .flatten(),
        display_preferences_id: Some(guid),
        primary_image_aspect_ratio: Some(if s.kind == "episode" {
            16.0 / 9.0
        } else {
            2.0 / 3.0
        }),
        video_type: is_video.then_some("VideoFile"),
        image_tags,
        backdrop_image_tags: s
            .backdrop_id
            .map(|image| vec![image_tag(state, s.id, image)])
            .unwrap_or_default(),
        image_blur_hashes: (!blur_hashes.is_empty()).then_some(blur_hashes),
        location_type: Some("FileSystem"),
        media_type: if is_video { "Video" } else { "Unknown" },
        ..Default::default()
    }
}

/// `{"Tmdb": "603", "Imdb": "tt0133093"}` (Jellyfin's provider names).
async fn provider_ids(db: &flixnet_db::Db, item_id: i64) -> ApiResult<BTreeMap<String, String>> {
    Ok(items::provider_ids(db, item_id)
        .await?
        .into_iter()
        .map(|(provider, value)| {
            let name = match provider.as_str() {
                "tmdb" => "Tmdb".to_owned(),
                "imdb" => "Imdb".to_owned(),
                "tvdb" => "Tvdb".to_owned(),
                other => {
                    let mut chars = other.chars();
                    chars
                        .next()
                        .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())
                        .unwrap_or_default()
                }
            };
            (name, value)
        })
        .collect())
}

fn person_kind(role: &str) -> &'static str {
    match role {
        "actor" => "Actor",
        "director" => "Director",
        "writer" => "Writer",
        "producer" => "Producer",
        "creator" => "Creator",
        "guest_star" => "GuestStar",
        "composer" => "Composer",
        _ => "Unknown",
    }
}

async fn people(state: &JfState, item_id: i64) -> ApiResult<Vec<BaseItemPerson>> {
    Ok(metadata::credits(&state.app.db, item_id)
        .await?
        .into_iter()
        .map(|c| BaseItemPerson {
            id: ids::guid(Kind::Person, c.person_id),
            primary_image_tag: c.image_url.as_ref().map(|_| person_tag(state, c.person_id)),
            role: c
                .character
                .clone()
                .or_else(|| (c.role != "actor").then(|| person_kind(&c.role).to_owned())),
            kind: person_kind(&c.role),
            name: c.name,
        })
        .collect())
}

async fn chapters(db: &flixnet_db::Db, item_id: i64) -> ApiResult<Vec<ChapterInfo>> {
    let Some(file) = media::files_for_item(db, item_id)
        .await?
        .into_iter()
        .find(|f| f.role == "main")
    else {
        return Ok(Vec::new());
    };
    Ok(media::chapters(db, file.id)
        .await?
        .into_iter()
        .map(|c| ChapterInfo {
            start_position_ticks: ticks(c.start_ms),
            name: c.title,
            image_date_modified: iso(0),
        })
        .collect())
}

/// A library as a Jellyfin view.
pub(crate) async fn library_dto(
    state: &JfState,
    library: &libraries::Library,
) -> ApiResult<BaseItemDto> {
    let counts = libraries::counts(&state.app.db, library.id).await?;
    let guid = ids::guid(Kind::Library, library.id);
    let collection = collection_type(&library.kind);
    Ok(BaseItemDto {
        name: library.name.clone(),
        server_id: state.server_id.to_string(),
        id: guid.clone(),
        etag: Some(format!("{:x}", library.created_at)),
        date_created: Some(iso(library.created_at)),
        can_delete: Some(false),
        can_download: Some(false),
        sort_name: Some(library.name.to_lowercase()),
        external_urls: Some(Vec::new()),
        is_folder: true,
        kind: "CollectionFolder",
        child_count: Some(if collection == "tvshows" {
            counts.series
        } else {
            counts.movies
        }),
        movie_count: Some(counts.movies),
        series_count: Some(counts.series),
        episode_count: Some(counts.episodes),
        display_preferences_id: Some(guid),
        primary_image_aspect_ratio: Some(16.0 / 9.0),
        collection_type: Some(collection),
        location_type: Some("FileSystem"),
        media_type: "Unknown",
        ..Default::default()
    })
}

pub(crate) fn person_dto(state: &JfState, person: &PersonRow) -> BaseItemDto {
    let mut image_tags = BTreeMap::new();
    if person.image_url.is_some() {
        image_tags.insert("Primary".to_owned(), person_tag(state, person.id));
    }
    BaseItemDto {
        name: person.name.clone(),
        server_id: state.server_id.to_string(),
        id: ids::guid(Kind::Person, person.id),
        kind: "Person",
        is_folder: false,
        image_tags,
        location_type: Some("FileSystem"),
        media_type: "Unknown",
        primary_image_aspect_ratio: Some(2.0 / 3.0),
        ..Default::default()
    }
}

pub(crate) fn genre_dto(state: &JfState, id: i64, name: &str, count: Option<i64>) -> BaseItemDto {
    BaseItemDto {
        name: name.to_owned(),
        server_id: state.server_id.to_string(),
        id: ids::guid(Kind::Genre, id),
        kind: "Genre",
        is_folder: true,
        child_count: count,
        location_type: Some("FileSystem"),
        media_type: "Unknown",
        ..Default::default()
    }
}

/// German display names of common languages (ISO 639-2), else the code.
fn language_name(code: &str) -> String {
    match code.to_ascii_lowercase().as_str() {
        "deu" | "ger" | "de" => "Deutsch".to_owned(),
        "eng" | "en" => "Englisch".to_owned(),
        "fra" | "fre" | "fr" => "Französisch".to_owned(),
        "spa" | "es" => "Spanisch".to_owned(),
        "ita" | "it" => "Italienisch".to_owned(),
        "jpn" | "ja" => "Japanisch".to_owned(),
        "kor" | "ko" => "Koreanisch".to_owned(),
        "nld" | "dut" | "nl" => "Niederländisch".to_owned(),
        "por" | "pt" => "Portugiesisch".to_owned(),
        "rus" | "ru" => "Russisch".to_owned(),
        "tur" | "tr" => "Türkisch".to_owned(),
        "pol" | "pl" => "Polnisch".to_owned(),
        "und" => "Unbekannt".to_owned(),
        other => other.to_uppercase(),
    }
}

/// Jellyfin's codec names for subtitle formats ffprobe names differently.
fn codec_name(stream: &media::Stream) -> Option<String> {
    let codec = stream.codec.as_deref()?;
    Some(
        match codec {
            "hdmv_pgs_subtitle" => "PGSSUB",
            "dvd_subtitle" => "DVDSUB",
            "dvb_subtitle" => "DVBSUB",
            "srt" => "subrip",
            "vtt" => "webvtt",
            "ssa" if stream.is_external => "ssa",
            other => other,
        }
        .to_owned(),
    )
}

const COVER_CODECS: &[&str] = &["mjpeg", "png", "bmp", "gif", "webp"];

/// A probed stream as Jellyfin's `MediaStream` (delivery is set by the caller).
pub(crate) fn media_stream(stream: &media::Stream) -> MediaStream {
    let codec = stream.codec.clone().unwrap_or_default();
    let kind = match stream.kind.as_str() {
        "video" if COVER_CODECS.contains(&codec.as_str()) => "EmbeddedImage",
        "video" => "Video",
        "audio" => "Audio",
        "subtitle" => "Subtitle",
        _ => "Data",
    };
    let (range, range_type) = match (kind, stream.hdr.as_deref()) {
        ("Video", Some("hdr10")) => ("HDR", "HDR10"),
        ("Video", Some("hlg")) => ("HDR", "HLG"),
        ("Video", Some("dovi")) => ("HDR", "DOVI"),
        ("Video", _) => ("SDR", "SDR"),
        _ => ("Unknown", "Unknown"),
    };
    let jf_codec = codec_name(stream);
    let codec_label = jf_codec.clone().unwrap_or_default().to_uppercase();
    let language = stream.language.as_deref().map(language_name);
    let display_title = match kind {
        "Video" => {
            let height = stream.height.map(|h| format!("{h}p ")).unwrap_or_default();
            Some(
                format!("{height}{codec_label} {range_type}")
                    .trim()
                    .to_owned(),
            )
        }
        "Audio" => {
            let mut parts = vec![language.clone().unwrap_or_else(|| "Unbekannt".to_owned())];
            parts.push(codec_label.clone());
            if let Some(layout) = &stream.channel_layout {
                parts.push(layout.clone());
            }
            if stream.is_default {
                parts.push("Standard".to_owned());
            }
            Some(parts.join(" - "))
        }
        "Subtitle" => {
            let mut parts = vec![language.clone().unwrap_or_else(|| "Unbekannt".to_owned())];
            if let Some(title) = &stream.title {
                parts.push(title.clone());
            }
            parts.push(codec_label.clone());
            if stream.is_forced {
                parts.push("Erzwungen".to_owned());
            }
            if stream.is_sdh {
                parts.push("SDH".to_owned());
            }
            if stream.is_external {
                parts.push("Extern".to_owned());
            }
            Some(parts.join(" - "))
        }
        _ => None,
    };
    let text = kind == "Subtitle" && is_text_subtitle(&codec, stream.is_external);
    MediaStream {
        codec: jf_codec,
        language: stream.language.clone(),
        color_transfer: stream.color_transfer.clone(),
        color_primaries: stream.color_primaries.clone(),
        title: stream.title.clone(),
        video_range: range,
        video_range_type: range_type,
        audio_spatial_format: "None",
        display_title,
        is_interlaced: false,
        channel_layout: stream.channel_layout.clone(),
        bit_rate: stream.bitrate,
        bit_depth: stream.bit_depth,
        channels: stream.channels,
        sample_rate: stream.sample_rate,
        is_default: stream.is_default,
        is_forced: stream.is_forced,
        is_hearing_impaired: stream.is_sdh,
        is_original: false,
        height: stream.height,
        width: stream.width,
        average_frame_rate: stream.fps,
        real_frame_rate: stream.fps,
        profile: stream.profile.clone(),
        kind,
        aspect_ratio: None,
        index: stream.idx,
        is_external: stream.is_external,
        delivery_method: None,
        delivery_url: None,
        is_external_url: stream.is_external.then_some(false),
        is_text_subtitle_stream: text,
        supports_external_stream: text,
        pixel_format: stream.pix_fmt.clone(),
        level: stream.level.map(|l| l as f64),
    }
}

/// Container name as Jellyfin uses it (the file extension, `mkv`, `mp4`, …).
pub(crate) fn container_of(file: &media::MediaFile) -> Option<String> {
    std::path::Path::new(&file.path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
}

pub(crate) fn file_name(file: &media::MediaFile) -> String {
    std::path::Path::new(&file.path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A media source without a playback decision (detail views): the file as it is.
pub(crate) fn media_source(
    file: &media::MediaFile,
    streams: &[media::Stream],
    runtime_ms: Option<i64>,
) -> MediaSourceInfo {
    let media_streams: Vec<MediaStream> = streams.iter().map(media_stream).collect();
    let default_audio = streams
        .iter()
        .filter(|s| s.kind == "audio")
        .find(|s| s.is_default)
        .or_else(|| streams.iter().find(|s| s.kind == "audio"))
        .map(|s| s.idx);
    let name = file_name(file);
    MediaSourceInfo {
        protocol: "File",
        id: ids::guid(Kind::File, file.id),
        path: Some(name.clone()),
        kind: "Default",
        container: container_of(file),
        size: Some(file.size),
        name: Some(
            std::path::Path::new(&name)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or(name),
        ),
        is_remote: false,
        etag: Some(format!("{:x}", file.mtime)),
        run_time_ticks: file.duration_ms.or(runtime_ms).map(ticks),
        read_at_native_framerate: false,
        ignore_dts: false,
        ignore_index: false,
        gen_pts_input: false,
        supports_transcoding: true,
        supports_direct_stream: true,
        supports_direct_play: true,
        is_infinite_stream: false,
        use_most_compatible_transcoding_profile: false,
        requires_opening: false,
        requires_closing: false,
        requires_looping: false,
        supports_probing: true,
        video_type: Some("VideoFile"),
        media_streams,
        media_attachments: Vec::new(),
        formats: Vec::new(),
        bitrate: file.bitrate,
        required_http_headers: BTreeMap::new(),
        transcoding_url: None,
        transcoding_sub_protocol: "http",
        transcoding_container: None,
        default_audio_stream_index: default_audio,
        default_subtitle_stream_index: None,
        has_segments: false,
    }
}

async fn static_media_sources(
    state: &JfState,
    item_id: i64,
    runtime_ms: Option<i64>,
) -> ApiResult<Vec<MediaSourceInfo>> {
    let db = &state.app.db;
    let mut out = Vec::new();
    for file in media::files_for_item(db, item_id).await? {
        if file.role != "main" || file.missing_since.is_some() {
            continue;
        }
        let streams = media::streams(db, file.id).await?;
        out.push(media_source(&file, &streams, runtime_ms));
    }
    Ok(out)
}
