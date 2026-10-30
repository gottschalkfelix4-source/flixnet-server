//! Playback: `PlaybackInfo` runs Flixnet's decision engine against the client's
//! DeviceProfile; direct streams serve the file with ranges; everything else plays
//! through a Flixnet transcode session as HLS (fMP4).

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use flixnet_api::auth::AuthUser;
use flixnet_api::browse::ensure_visible;
use flixnet_api::error::{ApiError, ApiResult};
use flixnet_api::playback::{accessible_file, serve_file, source_media, start_transcode};
use flixnet_db::media;
use flixnet_media::decision::{
    AudioCodecSupport, DeviceProfile, PlayMethod, PlaybackChoice, PlaybackPlan, SourceMedia,
    VideoCodecSupport, decide,
};
use flixnet_media::subtitles::is_text_subtitle;
use flixnet_media::transcode::{TranscodeError, is_session_id};
use serde::Deserialize;

use crate::auth::JfUser;
use crate::convert::media_source;
use crate::dto::{MediaSourceInfo, PlaybackInfoResponse};
use crate::ids::{self, Kind};
use crate::util::{Jf, JfJson, Params, random_id};
use crate::{JfState, PlayEntry, TranscodeRef};

pub(crate) fn router() -> Router<JfState> {
    Router::new()
        .route(
            "/items/{id}/playbackinfo",
            get(playback_info_get).post(playback_info_post),
        )
        .route("/items/{id}/download", get(download))
        .route("/videos/activeencodings", delete(stop_encodings))
        .route("/videos/{id}/{file}", get(video_file))
        .route("/videos/{id}/hls/{session}/{file}", get(hls_file))
        .route(
            "/videos/{id}/{source}/subtitles/{index}/{file}",
            get(subtitle),
        )
        .route(
            "/videos/{id}/{source}/subtitles/{index}/{ticks}/{file}",
            get(subtitle_with_ticks),
        )
        .route("/audio/{id}/{file}", get(audio_file))
        .route("/mediasegments/{id}", get(media_segments))
}

// ---- Media segments ----------------------------------------------------------------

/// Jellyfin's `MediaSegmentType` for a Flixnet segment kind.
fn segment_type(kind: &str) -> &'static str {
    match kind {
        "intro" => "Intro",
        "credits" => "Outro",
        "recap" => "Recap",
        _ => "Unknown",
    }
}

/// Intro, credits and recap of the item's first media file (clients use them for
/// "Intro überspringen"); `IncludeSegmentTypes` narrows the list.
async fn media_segments(
    jf: JfUser,
    State(state): State<JfState>,
    Path(id): Path<String>,
    params: Params,
) -> ApiResult<Jf<crate::dto::QueryResult<crate::dto::MediaSegmentDto>>> {
    let item_id = item_id(&id)?;
    ensure_visible(&state.app, &jf.user, item_id).await?;
    let wanted = params.set("includesegmenttypes");
    let rows = flixnet_db::segments::for_item(&state.app.db, item_id).await?;
    let first_file = rows.first().map(|s| s.file_id);
    let item_guid = ids::guid(Kind::Item, item_id);
    let items: Vec<_> = rows
        .into_iter()
        .filter(|s| Some(s.file_id) == first_file)
        .map(|s| crate::dto::MediaSegmentDto {
            id: crate::util::derived_id(&format!("segment:{}:{}", s.file_id, s.kind)),
            item_id: item_guid.clone(),
            kind: segment_type(&s.kind),
            start_ticks: crate::util::ticks(s.start_ms),
            end_ticks: crate::util::ticks(s.end_ms),
        })
        .filter(|s| wanted.is_empty() || wanted.contains(&s.kind.to_ascii_lowercase()))
        .collect();
    let total = items.len() as i64;
    Ok(Jf(crate::dto::QueryResult {
        items,
        total_record_count: total,
        start_index: 0,
    }))
}

// ---- Device profiles ---------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct JfDeviceProfile {
    max_streaming_bitrate: Option<i64>,
    direct_play_profiles: Option<Vec<DirectPlayProfile>>,
    transcoding_profiles: Option<Vec<TranscodingProfile>>,
    codec_profiles: Option<Vec<CodecProfile>>,
}

impl JfDeviceProfile {
    /// Neither direct-play nor transcoding profiles: Findroid's "Direct play all"
    /// profile, sent by a player (ExoPlayer/mpv) that takes the file as it is.
    fn plays_anything(&self) -> bool {
        self.direct_play_profiles.as_ref().is_none_or(Vec::is_empty)
            && self.transcoding_profiles.as_ref().is_none_or(Vec::is_empty)
    }

    /// Whether the client accepts a video transcode at all. A profile without any
    /// video transcoding profile gets none, as from Jellyfin (no ffmpeg is started
    /// for a stream the client would never request).
    fn accepts_transcode(&self) -> bool {
        match &self.transcoding_profiles {
            None => true,
            Some(list) => list.iter().any(|p| {
                p.kind
                    .as_deref()
                    .is_none_or(|k| k.eq_ignore_ascii_case("video"))
            }),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TranscodingProfile {
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DirectPlayProfile {
    container: Option<String>,
    audio_codec: Option<String>,
    video_codec: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CodecProfile {
    #[serde(rename = "type")]
    kind: Option<String>,
    codec: Option<String>,
    conditions: Option<Vec<ProfileCondition>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ProfileCondition {
    condition: Option<String>,
    property: Option<String>,
    value: Option<String>,
}

const VIDEO_CODECS: &[&str] = &[
    "h264",
    "hevc",
    "av1",
    "vp9",
    "vp8",
    "mpeg4",
    "mpeg2video",
    "vc1",
    "msmpeg4v3",
];
const AUDIO_CODECS: &[&str] = &[
    "aac",
    "mp3",
    "ac3",
    "eac3",
    "dts",
    "truehd",
    "flac",
    "opus",
    "vorbis",
    "alac",
    "mp2",
    "pcm_s16le",
    "pcm_s24le",
];
const ALL_HDR: &[&str] = &["hdr10", "hlg", "dovi"];

fn list(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split([',', '|'])
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn video_codec_name(codec: &str) -> String {
    match codec {
        "h265" | "hvc1" | "hev1" => "hevc",
        "avc" | "avc1" => "h264",
        other => other,
    }
    .to_owned()
}

fn audio_codec_name(codec: &str) -> String {
    match codec {
        "dca" => "dts",
        "mp4a" => "aac",
        other => other,
    }
    .to_owned()
}

/// Flixnet's device profile from Jellyfin's: the union of the direct-play profiles
/// (containers and codecs) plus the limits from the codec profiles.
pub(crate) fn device_profile(
    profile: &JfDeviceProfile,
    max_audio_channels: Option<i64>,
) -> DeviceProfile {
    let mut containers = Vec::new();
    let mut video: Vec<String> = Vec::new();
    let mut audio: Vec<String> = Vec::new();
    let mut any_video = false;
    let mut any_audio = false;
    for p in profile.direct_play_profiles.iter().flatten() {
        if p.kind
            .as_deref()
            .is_some_and(|k| !k.eq_ignore_ascii_case("video"))
        {
            continue;
        }
        for c in list(p.container.as_deref()) {
            match c.as_str() {
                "matroska" => containers.push("mkv".to_owned()),
                "mpegts" => containers.push("ts".to_owned()),
                _ => {}
            }
            containers.push(c);
        }
        let v = list(p.video_codec.as_deref());
        any_video |= v.is_empty();
        video.extend(v.iter().map(|c| video_codec_name(c)));
        let a = list(p.audio_codec.as_deref());
        any_audio |= a.is_empty();
        audio.extend(a.iter().map(|c| audio_codec_name(c)));
    }
    if any_video {
        video.extend(VIDEO_CODECS.iter().map(|c| (*c).to_owned()));
    }
    if any_audio {
        audio.extend(AUDIO_CODECS.iter().map(|c| (*c).to_owned()));
    }
    containers.sort();
    containers.dedup();
    video.sort();
    video.dedup();
    audio.sort();
    audio.dedup();

    let mut video_codecs: Vec<VideoCodecSupport> = video
        .into_iter()
        .map(|codec| VideoCodecSupport {
            max_bit_depth: Some(if codec == "h264" { 8 } else { 10 }),
            hdr: ALL_HDR.iter().map(|h| (*h).to_owned()).collect(),
            codec,
            ..Default::default()
        })
        .collect();
    let default_channels = max_audio_channels.unwrap_or(8);
    let mut audio_codecs: Vec<AudioCodecSupport> = audio
        .into_iter()
        .map(|codec| AudioCodecSupport {
            codec,
            max_channels: Some(default_channels),
        })
        .collect();

    for cp in profile.codec_profiles.iter().flatten() {
        let kind = cp.kind.as_deref().unwrap_or_default().to_ascii_lowercase();
        let codecs = list(cp.codec.as_deref());
        for cond in cp.conditions.iter().flatten() {
            let property = cond.property.as_deref().unwrap_or_default();
            let condition = cond.condition.as_deref().unwrap_or_default();
            let value = cond.value.as_deref().unwrap_or_default();
            let number = value.parse::<i64>().ok();
            let at_most = condition.eq_ignore_ascii_case("LessThanEqual");
            if kind == "video" {
                let applies =
                    |c: &VideoCodecSupport| codecs.is_empty() || codecs.contains(&c.codec);
                for support in video_codecs.iter_mut().filter(|c| applies(c)) {
                    match property {
                        "Width" if at_most => support.max_width = number,
                        "Height" if at_most => support.max_height = number,
                        "VideoBitDepth" if at_most => support.max_bit_depth = number,
                        "VideoRangeType" => {
                            let ranges = list(Some(value));
                            support.hdr = ALL_HDR
                                .iter()
                                .filter(|h| {
                                    ranges.iter().any(|r| match **h {
                                        "dovi" => r.starts_with("dovi"),
                                        "hdr10" => r == "hdr10" || r == "hdr10plus",
                                        other => r == other,
                                    })
                                })
                                .map(|h| (*h).to_owned())
                                .collect();
                        }
                        _ => {}
                    }
                }
            } else if (kind == "videoaudio" || kind == "audio")
                && property == "AudioChannels"
                && at_most
            {
                for support in audio_codecs
                    .iter_mut()
                    .filter(|c| codecs.is_empty() || codecs.contains(&c.codec))
                {
                    support.max_channels = number.map(|n| n.min(default_channels));
                }
            }
        }
    }
    DeviceProfile {
        containers,
        video_codecs,
        audio_codecs,
        max_bitrate: profile.max_streaming_bitrate.filter(|b| *b > 0),
        // Native players switch audio tracks themselves.
        multiple_audio_tracks: true,
    }
}

/// A client that sent no profile (e.g. Infuse) plays the file as it is.
fn permissive_profile(source: &SourceMedia) -> DeviceProfile {
    DeviceProfile {
        containers: vec![source.container.clone()],
        video_codecs: source
            .video
            .iter()
            .map(|v| VideoCodecSupport {
                codec: v.codec.clone(),
                max_bit_depth: Some(16),
                hdr: ALL_HDR.iter().map(|h| (*h).to_owned()).collect(),
                ..Default::default()
            })
            .collect(),
        audio_codecs: source
            .audio
            .iter()
            .map(|a| AudioCodecSupport {
                codec: a.codec.clone(),
                max_channels: Some(32),
            })
            .collect(),
        max_bitrate: None,
        multiple_audio_tracks: true,
    }
}

/// The profile a client describes in HLS URL parameters (`VideoCodec=h264,hevc`,
/// `AudioCodec=aac`, `MaxAudioChannels=2`, …) when it builds `master.m3u8` itself.
fn profile_from_params(params: &Params) -> DeviceProfile {
    let channels = params
        .i64("transcodingmaxaudiochannels")
        .or_else(|| params.i64("maxaudiochannels"))
        .or_else(|| params.i64("audiochannels"))
        .unwrap_or(2);
    DeviceProfile {
        // HLS is always a new container.
        containers: Vec::new(),
        video_codecs: list(params.get("videocodec"))
            .iter()
            .map(|c| VideoCodecSupport {
                codec: video_codec_name(c),
                max_width: params.i64("maxwidth"),
                max_height: params.i64("maxheight"),
                max_bit_depth: params.i64("maxvideobitdepth"),
                hdr: Vec::new(),
            })
            .collect(),
        audio_codecs: list(params.get("audiocodec"))
            .iter()
            .map(|c| AudioCodecSupport {
                codec: audio_codec_name(c),
                max_channels: Some(channels),
            })
            .collect(),
        max_bitrate: params
            .i64("maxstreamingbitrate")
            .or_else(|| params.i64("videobitrate"))
            .filter(|b| *b > 0),
        multiple_audio_tracks: false,
    }
}

// ---- PlaybackInfo ------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PlaybackInfoBody {
    max_streaming_bitrate: Option<i64>,
    start_time_ticks: Option<i64>,
    audio_stream_index: Option<i64>,
    subtitle_stream_index: Option<i64>,
    max_audio_channels: Option<i64>,
    media_source_id: Option<String>,
    device_profile: Option<JfDeviceProfile>,
    enable_direct_play: Option<bool>,
    allow_video_stream_copy: Option<bool>,
    allow_audio_stream_copy: Option<bool>,
}

async fn playback_info_get(
    jf: JfUser,
    State(state): State<JfState>,
    Path(id): Path<String>,
    params: Params,
) -> ApiResult<Jf<PlaybackInfoResponse>> {
    playback_info(&state, &jf.user, &id, &params, PlaybackInfoBody::default()).await
}

async fn playback_info_post(
    jf: JfUser,
    State(state): State<JfState>,
    Path(id): Path<String>,
    params: Params,
    JfJson(body): JfJson<PlaybackInfoBody>,
) -> ApiResult<Jf<PlaybackInfoResponse>> {
    playback_info(&state, &jf.user, &id, &params, body).await
}

fn item_id(id: &str) -> ApiResult<i64> {
    ids::parse_as(id, Kind::Item).ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))
}

/// Main files of an item (one per version or part), optionally only the one asked for.
async fn main_files(
    state: &JfState,
    item_id: i64,
    source: Option<&str>,
) -> ApiResult<Vec<media::MediaFile>> {
    let wanted = source.and_then(|s| ids::parse_as(s, Kind::File));
    let files: Vec<_> = media::files_for_item(&state.app.db, item_id)
        .await?
        .into_iter()
        .filter(|f| f.role == "main" && f.missing_since.is_none())
        .collect();
    let picked: Vec<_> = match wanted {
        Some(file) => files.iter().filter(|f| f.id == file).cloned().collect(),
        None => Vec::new(),
    };
    Ok(if picked.is_empty() { files } else { picked })
}

async fn playback_info(
    state: &JfState,
    user: &AuthUser,
    id: &str,
    params: &Params,
    body: PlaybackInfoBody,
) -> ApiResult<Jf<PlaybackInfoResponse>> {
    let item_id = item_id(id)?;
    ensure_visible(&state.app, user, item_id).await?;
    let media_source_id = body
        .media_source_id
        .clone()
        .or_else(|| params.get("mediasourceid").map(str::to_owned));
    let files = main_files(state, item_id, media_source_id.as_deref()).await?;
    if files.is_empty() {
        return Err(ApiError::not_found("Keine abspielbare Datei vorhanden."));
    }
    let non_negative = |v: Option<i64>| v.filter(|i| *i >= 0);
    let choice = PlaybackChoice {
        audio_index: non_negative(
            body.audio_stream_index
                .or_else(|| params.i64("audiostreamindex")),
        ),
        subtitle_index: non_negative(
            body.subtitle_stream_index
                .or_else(|| params.i64("subtitlestreamindex")),
        ),
        max_bitrate: body
            .max_streaming_bitrate
            .or_else(|| params.i64("maxstreamingbitrate"))
            .filter(|b| *b > 0),
    };
    let max_audio_channels = body
        .max_audio_channels
        .or_else(|| params.i64("maxaudiochannels"))
        .filter(|c| *c > 0);
    let enable_direct_play = body
        .enable_direct_play
        .or_else(|| params.bool("enabledirectplay"))
        .unwrap_or(true);
    let allow_video_copy = body
        .allow_video_stream_copy
        .or_else(|| params.bool("allowvideostreamcopy"))
        .unwrap_or(true);
    let allow_audio_copy = body
        .allow_audio_stream_copy
        .or_else(|| params.bool("allowaudiostreamcopy"))
        .unwrap_or(true);
    let start_ms = body
        .start_time_ticks
        .or_else(|| params.i64("starttimeticks"))
        .map(crate::util::ms)
        .filter(|ms| *ms > 0);

    let play_session_id = random_id();
    let mut transcodes = Vec::new();
    let mut sources = Vec::new();
    for file in files {
        let streams = media::streams(&state.app.db, file.id).await?;
        let source = source_media(&file, &streams);
        let mut profile = match &body.device_profile {
            Some(p) if !p.plays_anything() => device_profile(p, max_audio_channels),
            _ => permissive_profile(&source),
        };
        let accepts_transcode = body
            .device_profile
            .as_ref()
            .is_none_or(JfDeviceProfile::accepts_transcode);
        if !enable_direct_play {
            profile.containers.clear();
        }
        if !allow_video_copy {
            // Nothing can be copied: the video is re-encoded.
            profile.video_codecs.clear();
        }
        if !allow_audio_copy {
            profile.audio_codecs.retain(|c| c.codec == "aac");
        }
        let plan = decide(&source, &profile, choice);
        let mut info = media_source(&file, &streams, file.duration_ms);
        let item_guid = ids::guid(Kind::Item, item_id);
        info.supports_transcoding = accepts_transcode;
        if plan.method != PlayMethod::DirectPlay && accepts_transcode {
            let session = start_transcode(&state.app, user, item_id, &file, &source, &plan).await?;
            if let Some(ms) = start_ms {
                state.app.transcoder.hint_start(&session, ms);
            }
            info.transcoding_url = Some(format!(
                "/Videos/{item_guid}/hls/{session}/master.m3u8?MediaSourceId={}&PlaySessionId={play_session_id}",
                info.id
            ));
            info.transcoding_sub_protocol = "hls";
            info.transcoding_container = Some("mp4");
            transcodes.push(transcode_ref(&file, session, &plan));
        }
        set_delivery(state, user, &mut info, &file, &streams, &plan, &item_guid);
        info.supports_direct_play = plan.method == PlayMethod::DirectPlay;
        info.supports_direct_stream = plan.method == PlayMethod::DirectPlay;
        info.default_audio_stream_index = plan.audio.as_ref().map(|a| a.stream_index);
        info.default_subtitle_stream_index = Some(plan.subtitle.map_or(-1, |s| s.stream_index));
        sources.push(info);
    }
    state.plays.insert(
        play_session_id.clone(),
        PlayEntry {
            user_id: user.user_id,
            item_id,
            transcodes,
            created_at: flixnet_db::now_ms(),
        },
    );
    Ok(Jf(PlaybackInfoResponse {
        media_sources: sources,
        play_session_id,
    }))
}

fn transcode_ref(file: &media::MediaFile, session: String, plan: &PlaybackPlan) -> TranscodeRef {
    let video = plan.video.as_ref();
    TranscodeRef {
        file_id: file.id,
        session,
        method: plan.method,
        bandwidth: video
            .and_then(|v| v.bitrate)
            .or(file.bitrate)
            .unwrap_or(8_000_000)
            .max(64_000),
        width: video.map(|v| v.width).filter(|w| *w > 0),
        height: video.map(|v| v.height).filter(|h| *h > 0),
    }
}

/// How each subtitle reaches the player: embedded ones stay in the file for Direct
/// Play, text subtitles are offered as WebVTT, picture subtitles are burned in.
fn set_delivery(
    state: &JfState,
    user: &AuthUser,
    info: &mut MediaSourceInfo,
    file: &media::MediaFile,
    streams: &[media::Stream],
    plan: &PlaybackPlan,
    item_guid: &str,
) {
    let direct = plan.method == PlayMethod::DirectPlay;
    for stream in info
        .media_streams
        .iter_mut()
        .filter(|s| s.kind == "Subtitle")
    {
        let Some(source) = streams
            .iter()
            .find(|s| s.kind == "subtitle" && s.idx == stream.index)
        else {
            continue;
        };
        let text = is_text_subtitle(
            source.codec.as_deref().unwrap_or_default(),
            source.is_external,
        );
        if direct && !source.is_external {
            stream.delivery_method = Some("Embed");
        } else if text {
            stream.delivery_method = Some("External");
            stream.delivery_url = Some(subtitle_url(state, user, item_guid, file.id, stream.index));
        } else {
            stream.delivery_method = Some("Encode");
        }
    }
}

/// Signed subtitle URL (players load it without headers).
fn subtitle_url(
    state: &JfState,
    user: &AuthUser,
    item_guid: &str,
    file_id: i64,
    index: i64,
) -> String {
    let signed = state.app.signer.subtitle_url(file_id, index, user.user_id);
    let query = signed.split_once('?').map(|(_, q)| q).unwrap_or_default();
    format!(
        "/Videos/{item_guid}/{}/Subtitles/{index}/0/Stream.vtt?{query}",
        ids::guid(Kind::File, file_id)
    )
}

// ---- Direct streams ----------------------------------------------------------------

/// The user a valid `u`/`exp`/`sig` signature was issued to.
fn signed_user(params: &Params, verify: impl Fn(i64, i64, &str) -> bool) -> Option<i64> {
    match (params.i64("u"), params.i64("exp"), params.get("sig")) {
        (Some(u), Some(exp), Some(sig)) if verify(u, exp, sig) => Some(u),
        _ => None,
    }
}

/// The file behind `MediaSourceId` (or the item's first main file) after Flixnet's
/// access check.
async fn requested_file(
    state: &JfState,
    user: Option<&JfUser>,
    item: &str,
    params: &Params,
    signature: impl Fn(&JfState, i64, i64, i64, &str) -> bool,
    source: Option<&str>,
) -> ApiResult<media::MediaFile> {
    let not_found = || ApiError::not_found("Datei nicht gefunden.");
    let item_id = item_id(item)?;
    let source = source.or_else(|| params.get("mediasourceid"));
    let file_id = match source.and_then(|s| ids::parse_as(s, Kind::File)) {
        Some(id) => id,
        None => main_files(state, item_id, None)
            .await?
            .first()
            .map(|f| f.id)
            .ok_or_else(not_found)?,
    };
    let signed = signed_user(params, |u, exp, sig| signature(state, file_id, u, exp, sig));
    let file = accessible_file(&state.app, user.map(|j| &j.user), signed, file_id).await?;
    if file.item_id != Some(item_id) {
        return Err(not_found());
    }
    Ok(file)
}

fn stream_signature(state: &JfState, file_id: i64, user: i64, exp: i64, sig: &str) -> bool {
    state.app.signer.verify_stream(file_id, user, exp, sig)
}

fn subtitle_signature(state: &JfState, file_id: i64, user: i64, exp: i64, sig: &str) -> bool {
    state.app.signer.verify_subtitle(file_id, user, exp, sig)
}

async fn direct_stream(
    state: &JfState,
    user: Option<JfUser>,
    item: &str,
    params: &Params,
    request: Request,
) -> ApiResult<Response> {
    let file = requested_file(state, user.as_ref(), item, params, stream_signature, None).await?;
    serve_file(&file, request).await
}

/// `stream`, `stream.{ext}` (Static=true: the original file), `master.m3u8` and
/// `main.m3u8` (HLS for clients that build the URL themselves).
async fn video_file(
    user: Option<JfUser>,
    State(state): State<JfState>,
    Path((id, file)): Path<(String, String)>,
    params: Params,
    request: Request,
) -> ApiResult<Response> {
    match file.as_str() {
        "stream" => direct_stream(&state, user, &id, &params, request).await,
        f if f.starts_with("stream.") => direct_stream(&state, user, &id, &params, request).await,
        "master.m3u8" | "main.m3u8" => {
            let user = user.ok_or_else(ApiError::unauthorized)?;
            let session = session_for(&state, &user.user, &id, &params).await?;
            if file == "master.m3u8" {
                Ok(master_playlist(
                    &state,
                    &session,
                    &format!("hls/{session}/main.m3u8"),
                ))
            } else {
                let playlist = state.app.transcoder.playlist(&session).map_err(map_error)?;
                Ok(playlist_response(prefix_playlist(
                    &playlist,
                    &format!("hls/{session}/"),
                )))
            }
        }
        _ => Err(ApiError::not_found("Nicht gefunden.")),
    }
}

async fn audio_file(
    user: Option<JfUser>,
    State(state): State<JfState>,
    Path((id, file)): Path<(String, String)>,
    params: Params,
    request: Request,
) -> ApiResult<Response> {
    if file == "stream" || file == "universal" || file.starts_with("stream.") {
        direct_stream(&state, user, &id, &params, request).await
    } else {
        Err(ApiError::not_found("Nicht gefunden."))
    }
}

async fn download(
    user: Option<JfUser>,
    State(state): State<JfState>,
    Path(id): Path<String>,
    params: Params,
    request: Request,
) -> ApiResult<Response> {
    let file = requested_file(&state, user.as_ref(), &id, &params, stream_signature, None).await?;
    let name = crate::convert::file_name(&file).replace(['"', '\\'], "_");
    let mut response = serve_file(&file, request).await?;
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
    Ok(response)
}

// ---- HLS ---------------------------------------------------------------------------

fn map_error(err: TranscodeError) -> ApiError {
    match err {
        TranscodeError::UnknownSession => ApiError::not_found("Wiedergabe-Sitzung nicht gefunden."),
        TranscodeError::OutOfRange(_) => ApiError::not_found("Segment nicht vorhanden."),
        TranscodeError::Timeout(_) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "transcode_timeout",
            "Die Umwandlung ist zu langsam.",
        ),
        other => ApiError::internal(other),
    }
}

/// The transcode session for a client-built HLS URL: the one `PlaybackInfo` started
/// for this play session, or a new one from the URL's codec parameters.
async fn session_for(
    state: &JfState,
    user: &AuthUser,
    item: &str,
    params: &Params,
) -> ApiResult<String> {
    let item_id = item_id(item)?;
    ensure_visible(&state.app, user, item_id).await?;
    let wanted_file = params
        .get("mediasourceid")
        .and_then(|s| ids::parse_as(s, Kind::File));
    if let Some(entry) = params.get("playsessionid").and_then(|p| state.plays.get(p))
        && entry.user_id == user.user_id
        && entry.item_id == item_id
        && let Some(t) = entry
            .transcodes
            .iter()
            .find(|t| wanted_file.is_none_or(|f| f == t.file_id))
        && state.app.transcoder.owner(&t.session) == Some(user.user_id)
    {
        return Ok(t.session.clone());
    }

    let file = main_files(state, item_id, params.get("mediasourceid"))
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::not_found("Keine abspielbare Datei vorhanden."))?;
    let streams = media::streams(&state.app.db, file.id).await?;
    let source = source_media(&file, &streams);
    let choice = PlaybackChoice {
        audio_index: params.i64("audiostreamindex").filter(|i| *i >= 0),
        subtitle_index: params.i64("subtitlestreamindex").filter(|i| *i >= 0),
        max_bitrate: None,
    };
    let plan = decide(&source, &profile_from_params(params), choice);
    let session = start_transcode(&state.app, user, item_id, &file, &source, &plan).await?;
    if let Some(ms) = params
        .i64("starttimeticks")
        .map(crate::util::ms)
        .filter(|m| *m > 0)
    {
        state.app.transcoder.hint_start(&session, ms);
    }
    let play_session = params
        .get("playsessionid")
        .map(str::to_owned)
        .unwrap_or_else(random_id);
    let mut entry = state.plays.get(&play_session).unwrap_or(PlayEntry {
        user_id: user.user_id,
        item_id,
        transcodes: Vec::new(),
        created_at: flixnet_db::now_ms(),
    });
    if entry.user_id == user.user_id {
        entry
            .transcodes
            .push(transcode_ref(&file, session.clone(), &plan));
        state.plays.insert(play_session, entry);
    }
    Ok(session)
}

fn playlist_response(body: String) -> Response {
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/vnd.apple.mpegurl"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        body,
    )
        .into_response()
}

fn master_playlist(state: &JfState, session: &str, uri: &str) -> Response {
    let known = state.plays.find_transcode(session);
    let bandwidth = known.as_ref().map_or(8_000_000, |t| t.bandwidth);
    let resolution = known
        .as_ref()
        .and_then(|t| Some(format!(",RESOLUTION={}x{}", t.width?, t.height?)))
        .unwrap_or_default();
    playlist_response(format!(
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH={bandwidth},AVERAGE-BANDWIDTH={bandwidth}{resolution}\n{uri}\n"
    ))
}

/// Rewrites the relative URIs of a media playlist (`init.mp4`, `{n}.m4s`) to live
/// under `prefix`.
fn prefix_playlist(playlist: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(playlist.len() + 64);
    for line in playlist.lines() {
        if line.is_empty() {
            out.push('\n');
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MAP:URI=\"") {
            out.push_str(&format!("#EXT-X-MAP:URI=\"{prefix}{rest}\n"));
        } else if line.starts_with('#') {
            out.push_str(line);
            out.push('\n');
        } else {
            out.push_str(prefix);
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

async fn file_response(
    path: std::path::PathBuf,
    content_type: &'static str,
) -> ApiResult<Response> {
    let bytes = tokio::fs::read(&path).await.map_err(ApiError::internal)?;
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=3600"),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// Playlists and segments of a session; the 128-bit session id is the capability
/// (players cannot add headers to segment requests).
async fn hls_file(
    State(state): State<JfState>,
    Path((_id, session, file)): Path<(String, String, String)>,
    params: Params,
) -> ApiResult<Response> {
    if !is_session_id(&session) {
        return Err(ApiError::not_found("Wiedergabe-Sitzung nicht gefunden."));
    }
    let transcoder = &state.app.transcoder;
    match file.as_str() {
        "master.m3u8" => {
            transcoder.playlist(&session).map_err(map_error)?;
            Ok(master_playlist(&state, &session, "main.m3u8"))
        }
        "main.m3u8" => {
            let playlist = transcoder.playlist(&session).map_err(map_error)?;
            if let Some(ms) = params
                .i64("starttimeticks")
                .map(crate::util::ms)
                .filter(|m| *m > 0)
            {
                transcoder.hint_start(&session, ms);
            }
            Ok(playlist_response(playlist))
        }
        "init.mp4" => {
            let path = transcoder.init(&session).await.map_err(map_error)?;
            file_response(path, "video/mp4").await
        }
        segment => {
            let index: u32 = segment
                .strip_suffix(".m4s")
                .and_then(|n| n.parse().ok())
                .ok_or_else(|| ApiError::not_found("Segment nicht vorhanden."))?;
            let path = transcoder
                .segment(&session, index)
                .await
                .map_err(map_error)?;
            file_response(path, "video/iso.segment").await
        }
    }
}

/// The player closed: stop the play session's transcodes.
async fn stop_encodings(jf: JfUser, State(state): State<JfState>, params: Params) -> StatusCode {
    if let Some(play_session) = params.get("playsessionid") {
        stop_play_session(&state, &jf.user, play_session).await;
    }
    StatusCode::NO_CONTENT
}

pub(crate) async fn stop_play_session(state: &JfState, user: &AuthUser, play_session: &str) {
    let Some(entry) = state.plays.get(play_session) else {
        return;
    };
    if entry.user_id != user.user_id {
        return;
    }
    state.plays.remove(play_session);
    for t in entry.transcodes {
        if state.app.transcoder.owner(&t.session) == Some(user.user_id) {
            state.app.transcoder.stop(&t.session).await;
        }
    }
}

// ---- Subtitles ---------------------------------------------------------------------

async fn subtitle(
    user: Option<JfUser>,
    State(state): State<JfState>,
    Path((id, source, index, file)): Path<(String, String, i64, String)>,
    params: Params,
) -> ApiResult<Response> {
    subtitle_response(&state, user, &id, &source, index, &file, &params).await
}

async fn subtitle_with_ticks(
    user: Option<JfUser>,
    State(state): State<JfState>,
    Path((id, source, index, _ticks, file)): Path<(String, String, i64, i64, String)>,
    params: Params,
) -> ApiResult<Response> {
    subtitle_response(&state, user, &id, &source, index, &file, &params).await
}

async fn subtitle_response(
    state: &JfState,
    user: Option<JfUser>,
    item: &str,
    source: &str,
    index: i64,
    file_name: &str,
    params: &Params,
) -> ApiResult<Response> {
    let format = file_name
        .strip_prefix("stream.")
        .ok_or_else(|| ApiError::not_found("Untertitelspur nicht gefunden."))?;
    let file = requested_file(
        state,
        user.as_ref(),
        item,
        params,
        subtitle_signature,
        Some(source),
    )
    .await?;
    let path = flixnet_api::tracks::webvtt(&state.app, file, index).await?;
    let vtt = tokio::fs::read_to_string(&path)
        .await
        .map_err(ApiError::internal)?;
    let (body, content_type) = match format {
        "srt" | "subrip" => (vtt_to_srt(&vtt), "application/x-subrip; charset=utf-8"),
        _ => (vtt, "text/vtt; charset=utf-8"),
    };
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=86400"),
            ),
        ],
        body,
    )
        .into_response())
}

/// WebVTT → SubRip: numbered cues, `,` as decimal separator, no cue settings.
fn vtt_to_srt(vtt: &str) -> String {
    let normalized = vtt.replace("\r\n", "\n");
    let mut out = String::new();
    let mut n = 0;
    for block in normalized.split("\n\n") {
        let lines: Vec<&str> = block.lines().filter(|l| !l.trim().is_empty()).collect();
        let Some(timing) = lines.iter().position(|l| l.contains("-->")) else {
            continue;
        };
        let mut parts = lines[timing].split("-->");
        let (Some(start), Some(end)) = (parts.next(), parts.next()) else {
            continue;
        };
        let end = end.split_whitespace().next().unwrap_or_default();
        n += 1;
        out.push_str(&format!(
            "{n}\n{} --> {}\n",
            srt_time(start.trim()),
            srt_time(end)
        ));
        for line in &lines[timing + 1..] {
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

fn srt_time(vtt: &str) -> String {
    let time = vtt.replace('.', ",");
    if time.matches(':').count() == 1 {
        format!("00:{time}")
    } else {
        time
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webvtt_becomes_subrip() {
        let vtt = "WEBVTT\n\nNOTE x\n\n1\n00:00:01.000 --> 00:00:02.500 align:start\nHallo\nWelt\n\n01:02.000 --> 01:03.000\nZwei\n";
        assert_eq!(
            vtt_to_srt(vtt),
            "1\n00:00:01,000 --> 00:00:02,500\nHallo\nWelt\n\n2\n00:01:02,000 --> 00:01:03,000\nZwei\n\n"
        );
    }

    #[test]
    fn playlists_are_rebased() {
        let playlist =
            "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:6.000,\n0.m4s\n#EXT-X-ENDLIST\n";
        assert_eq!(
            prefix_playlist(playlist, "hls/abc/"),
            "#EXTM3U\n#EXT-X-MAP:URI=\"hls/abc/init.mp4\"\n#EXTINF:6.000,\nhls/abc/0.m4s\n#EXT-X-ENDLIST\n"
        );
    }

    fn profile(json: serde_json::Value) -> DeviceProfile {
        let jf: JfDeviceProfile = serde_json::from_value(crate::util::snake_keys(json)).unwrap();
        device_profile(&jf, None)
    }

    #[test]
    fn jellyfin_profiles_map_to_flixnet_profiles() {
        let p = profile(serde_json::json!({
            "MaxStreamingBitrate": 20000000,
            "DirectPlayProfiles": [
                { "Container": "mp4,m4v", "Type": "Video", "VideoCodec": "h264,h265", "AudioCodec": "aac,ac3" },
                { "Container": "mkv", "Type": "Video", "VideoCodec": "h264", "AudioCodec": "aac" },
                { "Container": "mp3", "Type": "Audio" }
            ],
            "CodecProfiles": [
                { "Type": "Video", "Codec": "hevc", "Conditions": [
                    { "Condition": "LessThanEqual", "Property": "Width", "Value": "1920" },
                    { "Condition": "EqualsAny", "Property": "VideoRangeType", "Value": "SDR|HDR10" }
                ]},
                { "Type": "VideoAudio", "Codec": "ac3", "Conditions": [
                    { "Condition": "LessThanEqual", "Property": "AudioChannels", "Value": "6" }
                ]}
            ]
        }));
        assert_eq!(p.containers, ["m4v", "mkv", "mp4"]);
        assert_eq!(p.max_bitrate, Some(20_000_000));
        let hevc = p.video_codecs.iter().find(|c| c.codec == "hevc").unwrap();
        assert_eq!(hevc.max_width, Some(1920));
        assert_eq!(hevc.max_bit_depth, Some(10));
        assert_eq!(hevc.hdr, ["hdr10"]);
        let h264 = p.video_codecs.iter().find(|c| c.codec == "h264").unwrap();
        assert_eq!(h264.max_bit_depth, Some(8));
        assert_eq!(h264.hdr, ["hdr10", "hlg", "dovi"]);
        let ac3 = p.audio_codecs.iter().find(|c| c.codec == "ac3").unwrap();
        assert_eq!(ac3.max_channels, Some(6));
        let aac = p.audio_codecs.iter().find(|c| c.codec == "aac").unwrap();
        assert_eq!(aac.max_channels, Some(8));
        assert!(p.multiple_audio_tracks);
    }

    #[test]
    fn profiles_without_codecs_accept_any() {
        let p = profile(serde_json::json!({
            "DirectPlayProfiles": [ { "Container": "mkv,mp4", "Type": "Video" } ]
        }));
        assert!(p.video_codecs.iter().any(|c| c.codec == "av1"));
        assert!(p.audio_codecs.iter().any(|c| c.codec == "truehd"));
    }
}
