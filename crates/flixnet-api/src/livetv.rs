//! `/api/v1/livetv/*`: tuners and guide sources (administrators), channels, guide,
//! program search and live playback (everyone signed in).
//!
//! Watching a channel creates a viewer session whose 128-bit id is the capability
//! for the HLS URLs (players cannot add headers), like transcode sessions.

use std::collections::HashMap;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use flixnet_core::jobs::priority;
use flixnet_db::livetv::{self, Channel, EpgMatch, HostUpdate, NewTunerHost, Program};
use flixnet_livetv::config::{self, LiveTvConfig};
use flixnet_livetv::schedules_direct::{self, SdCredentials, SdError};
use flixnet_livetv::stream::{WatchRequest, is_session_id};
use flixnet_livetv::tuner::{is_stream_url, stream_source};
use flixnet_livetv::{LiveError, TunerKind, hdhomerun, http, jobs, matching};
use flixnet_media::decision::{
    self, AudioCodecSupport, DeviceProfile, PlayMethod, PlaybackChoice, PlaybackPlan, SourceAudio,
    SourceMedia, SourceVideo, VideoCodecSupport,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::{AdminUser, AuthUser};
use crate::dvr::{live_viewer, program_allowed};
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::validate;

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_tuners, create_tuner))
        .routes(routes!(update_tuner, delete_tuner))
        .routes(routes!(import_tuner))
        .routes(routes!(discover_tuners))
        .routes(routes!(list_epg_sources, create_epg_source))
        .routes(routes!(update_epg_source, delete_epg_source))
        .routes(routes!(refresh_epg_source))
        .routes(routes!(list_epg_channels))
        .routes(routes!(list_channels))
        .routes(routes!(update_channel))
        .routes(routes!(get_channel_logo))
        .routes(routes!(set_channel_epg))
        .routes(routes!(rematch_channels))
        .routes(routes!(get_guide))
        .routes(routes!(search_programs))
        .routes(routes!(get_program))
        .routes(routes!(watch_channel))
        .routes(routes!(stop_live_session))
        .routes(routes!(get_live_playlist))
        .routes(routes!(get_live_segment))
        .routes(routes!(list_live_streams))
        .routes(routes!(get_livetv_settings, update_livetv_settings))
        .routes(routes!(get_schedules_direct, update_schedules_direct))
        .routes(routes!(list_schedules_direct_lineups))
}

// ---- Tuners ------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TunerKindDto {
    /// M3U playlist (URL or file on the server).
    M3u,
    /// Xtream Codes account.
    Xtream,
    /// HDHomeRun network tuner.
    Hdhomerun,
    /// tvheadend server (HTTP API; its EPG becomes a guide source).
    Tvheadend,
}

impl TunerKindDto {
    fn kind(self) -> TunerKind {
        match self {
            TunerKindDto::M3u => TunerKind::M3u,
            TunerKindDto::Xtream => TunerKind::Xtream,
            TunerKindDto::Hdhomerun => TunerKind::HdHomeRun,
            TunerKindDto::Tvheadend => TunerKind::Tvheadend,
        }
    }

    fn from_kind(kind: &str) -> Self {
        match TunerKind::parse(kind) {
            Some(TunerKind::Xtream) => TunerKindDto::Xtream,
            Some(TunerKind::HdHomeRun) => TunerKindDto::Hdhomerun,
            Some(TunerKind::Tvheadend) => TunerKindDto::Tvheadend,
            _ => TunerKindDto::M3u,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TunerDto {
    pub id: i64,
    pub kind: TunerKindDto,
    pub name: String,
    /// Playlist URL/path, server URL or device address.
    pub url: String,
    pub username: Option<String>,
    /// Whether a password is stored (it is never returned).
    pub has_password: bool,
    pub user_agent: Option<String>,
    pub device_id: Option<String>,
    /// Streams that may run at once (0 = unlimited).
    pub tuner_count: i64,
    /// The count was set by an administrator (else it comes from the source).
    pub tuner_count_manual: bool,
    /// Streams running right now.
    pub tuners_in_use: i64,
    pub enabled: bool,
    pub channel_count: i64,
    pub last_import_at: Option<i64>,
    pub last_error: Option<String>,
}

fn tuner_dto(state: &AppState, host: livetv::TunerHost) -> TunerDto {
    TunerDto {
        tuners_in_use: state.livetv.tuners_in_use(host.id) as i64,
        id: host.id,
        kind: TunerKindDto::from_kind(&host.kind),
        name: host.name,
        url: host.url,
        username: host.username,
        has_password: host.password.is_some_and(|p| !p.is_empty()),
        user_agent: host.user_agent,
        device_id: host.device_id,
        tuner_count: host.tuner_count,
        tuner_count_manual: host.tuner_count_manual,
        enabled: host.enabled,
        channel_count: host.channel_count,
        last_import_at: host.last_import_at,
        last_error: host.last_error,
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateTunerRequest {
    pub kind: TunerKindDto,
    pub name: String,
    /// M3U: playlist URL or absolute file path; Xtream: server URL; HDHomeRun: IP
    /// address or URL of the device; tvheadend: `http://host:9981`.
    pub url: String,
    /// Xtream Codes and tvheadend.
    pub username: Option<String>,
    /// Xtream Codes and tvheadend.
    pub password: Option<String>,
    /// Sent when fetching the playlist and streams (some providers require one).
    pub user_agent: Option<String>,
    /// Streams that may run at once; omit to use what the source reports.
    pub tuner_count: Option<i64>,
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct UpdateTunerRequest {
    pub name: Option<String>,
    pub url: Option<String>,
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub username: Option<Option<String>>,
    /// A new password; `null` removes it, omitted keeps it.
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub password: Option<Option<String>>,
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub user_agent: Option<Option<String>>,
    /// `null` hands the tuner count back to the source.
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<i64>, nullable)]
    pub tuner_count: Option<Option<i64>>,
    pub enabled: Option<bool>,
}

fn is_absolute_path(location: &str) -> bool {
    let path = std::path::Path::new(location);
    path.is_absolute() || location.starts_with('/')
}

fn validate_location(kind: TunerKind, url: &str) -> ApiResult<String> {
    let url = url.trim();
    if url.is_empty() || url.len() > 2048 || url.chars().any(char::is_control) {
        return Err(ApiError::validation("Bitte eine gültige Adresse angeben."));
    }
    let ok = match kind {
        TunerKind::M3u => http::is_http(url) || is_absolute_path(url),
        TunerKind::Xtream | TunerKind::Tvheadend => http::is_http(url),
        TunerKind::HdHomeRun => !url.contains(char::is_whitespace),
    };
    if !ok {
        return Err(ApiError::validation(match kind {
            TunerKind::M3u => {
                "Die Playlist braucht eine http(s)-Adresse oder einen absoluten Dateipfad."
            }
            TunerKind::Xtream | TunerKind::Tvheadend => {
                "Die Server-Adresse muss mit http:// oder https:// beginnen."
            }
            TunerKind::HdHomeRun => "Bitte die IP-Adresse des HDHomeRun angeben.",
        }));
    }
    Ok(url.to_owned())
}

fn optional_text(value: Option<&str>, max: usize) -> ApiResult<Option<String>> {
    match value.map(str::trim) {
        None | Some("") => Ok(None),
        Some(v) if v.len() > max || v.chars().any(char::is_control) => Err(ApiError::validation(
            "Ein Feld enthält ungültige Zeichen oder ist zu lang.",
        )),
        Some(v) => Ok(Some(v.to_owned())),
    }
}

fn validate_tuner_count(count: Option<i64>) -> ApiResult<()> {
    match count {
        Some(n) if !(0..=64).contains(&n) => Err(ApiError::validation(
            "Die Anzahl der Tuner muss zwischen 0 (unbegrenzt) und 64 liegen.",
        )),
        _ => Ok(()),
    }
}

/// Configured tuners (administrators).
#[utoipa::path(
    get,
    path = "/livetv/tuners",
    operation_id = "listTuners",
    tag = "livetv",
    responses(
        (status = OK, description = "Tuners", body = [TunerDto]),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn list_tuners(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<TunerDto>>> {
    let hosts = livetv::list_hosts(&state.db).await?;
    Ok(Json(
        hosts.into_iter().map(|h| tuner_dto(&state, h)).collect(),
    ))
}

/// Adds a tuner and imports its channels in the background.
#[utoipa::path(
    post,
    path = "/livetv/tuners",
    operation_id = "createTuner",
    tag = "livetv",
    request_body = CreateTunerRequest,
    responses(
        (status = CREATED, description = "Tuner added; channel import queued", body = TunerDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid input", body = ErrorBody),
    )
)]
async fn create_tuner(
    _: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<CreateTunerRequest>,
) -> ApiResult<(StatusCode, Json<TunerDto>)> {
    let kind = body.kind.kind();
    let name = validate::name(&body.name)?;
    let url = validate_location(kind, &body.url)?;
    let username = optional_text(body.username.as_deref(), 256)?;
    let password = optional_text(body.password.as_deref(), 256)?;
    let user_agent = optional_text(body.user_agent.as_deref(), 256)?;
    validate_tuner_count(body.tuner_count)?;
    if kind == TunerKind::Xtream && (username.is_none() || password.is_none()) {
        return Err(ApiError::validation(
            "Für Xtream Codes werden Benutzername und Passwort benötigt.",
        ));
    }
    let id = livetv::create_host(
        &state.db,
        &NewTunerHost {
            kind: kind.as_str(),
            name: &name,
            url: &url,
            username: username.as_deref(),
            password: password.as_deref(),
            user_agent: user_agent.as_deref(),
            device_id: None,
            tuner_count: body.tuner_count,
        },
    )
    .await?;
    jobs::queue_import(state.jobs.queue(), Some(id), priority::INTERACTIVE).await?;
    if kind == TunerKind::Tvheadend {
        // Its EPG comes along as a guide source.
        let source = livetv::create_source_of_kind(
            &state.db,
            &format!("{name} (EPG)"),
            &url,
            "tvheadend",
            Some(id),
        )
        .await?;
        jobs::queue_refresh(state.jobs.queue(), Some(source), priority::INTERACTIVE).await?;
    }
    let host = livetv::get_host(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Tuner nicht gefunden."))?;
    Ok((StatusCode::CREATED, Json(tuner_dto(&state, host))))
}

/// Changes a tuner; a changed address or account re-imports its channels.
#[utoipa::path(
    patch,
    path = "/livetv/tuners/{id}",
    operation_id = "updateTuner",
    tag = "livetv",
    params(("id" = i64, Path, description = "Tuner id")),
    request_body = UpdateTunerRequest,
    responses(
        (status = OK, description = "Updated tuner", body = TunerDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown tuner", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid input", body = ErrorBody),
    )
)]
async fn update_tuner(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<UpdateTunerRequest>,
) -> ApiResult<Json<TunerDto>> {
    let host = livetv::get_host(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Tuner nicht gefunden."))?;
    let kind = TunerKind::parse(&host.kind).unwrap_or(TunerKind::M3u);
    let name = body.name.as_deref().map(validate::name).transpose()?;
    let url = body
        .url
        .as_deref()
        .map(|u| validate_location(kind, u))
        .transpose()?;
    let text = |value: &Option<Option<String>>| -> ApiResult<Option<Option<String>>> {
        value
            .as_ref()
            .map(|v| optional_text(v.as_deref(), 256))
            .transpose()
    };
    let username = text(&body.username)?;
    let password = text(&body.password)?;
    let user_agent = text(&body.user_agent)?;
    validate_tuner_count(body.tuner_count.flatten())?;
    let reimport = url.is_some() || username.is_some() || password.is_some();
    livetv::update_host(
        &state.db,
        id,
        &HostUpdate {
            name: name.as_deref(),
            url: url.as_deref(),
            username: username.as_ref().map(|u| u.as_deref()),
            password: password.as_ref().map(|p| p.as_deref()),
            user_agent: user_agent.as_ref().map(|a| a.as_deref()),
            tuner_count: body.tuner_count,
            enabled: body.enabled,
        },
    )
    .await?;
    if reimport {
        jobs::queue_import(state.jobs.queue(), Some(id), priority::INTERACTIVE).await?;
    }
    let host = livetv::get_host(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Tuner nicht gefunden."))?;
    Ok(Json(tuner_dto(&state, host)))
}

/// Removes a tuner with its channels.
#[utoipa::path(
    delete,
    path = "/livetv/tuners/{id}",
    operation_id = "deleteTuner",
    tag = "livetv",
    params(("id" = i64, Path, description = "Tuner id")),
    responses(
        (status = NO_CONTENT, description = "Removed"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown tuner", body = ErrorBody),
    )
)]
async fn delete_tuner(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    // Its guide (tvheadend EPG) goes too, programs in batches.
    for source in livetv::list_sources(&state.db).await? {
        if source.tuner_host_id == Some(id) {
            livetv::delete_source(&state.db, source.id).await?;
        }
    }
    if livetv::delete_host(&state.db, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("Tuner nicht gefunden."))
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct JobQueuedDto {
    /// Id of the queued job (`null` when the same job is already waiting).
    pub job_id: Option<i64>,
}

/// Re-imports a tuner's channels (background job).
#[utoipa::path(
    post,
    path = "/livetv/tuners/{id}/import",
    operation_id = "importTunerChannels",
    tag = "livetv",
    params(("id" = i64, Path, description = "Tuner id")),
    responses(
        (status = ACCEPTED, description = "Import queued", body = JobQueuedDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown tuner", body = ErrorBody),
    )
)]
async fn import_tuner(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<(StatusCode, Json<JobQueuedDto>)> {
    livetv::get_host(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Tuner nicht gefunden."))?;
    let job_id = jobs::queue_import(state.jobs.queue(), Some(id), priority::INTERACTIVE).await?;
    Ok((StatusCode::ACCEPTED, Json(JobQueuedDto { job_id })))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct DiscoverRequest {
    /// Ask one device directly (IP address or URL) instead of broadcasting.
    pub address: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DiscoveredTunerDto {
    pub device_id: String,
    /// Address to add the device with.
    pub url: String,
    pub tuner_count: Option<i64>,
    pub name: Option<String>,
    pub model: Option<String>,
    /// A tuner with this device id is already set up.
    pub configured: bool,
}

/// Finds HDHomeRun devices: UDP broadcast in the local network, or one address.
#[utoipa::path(
    post,
    path = "/livetv/tuners/discover",
    operation_id = "discoverTuners",
    tag = "livetv",
    request_body = DiscoverRequest,
    responses(
        (status = OK, description = "Devices found (possibly none)", body = [DiscoveredTunerDto]),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = BAD_GATEWAY, description = "The address answered no HDHomeRun request", body = ErrorBody),
    )
)]
async fn discover_tuners(
    _: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<DiscoverRequest>,
) -> ApiResult<Json<Vec<DiscoveredTunerDto>>> {
    let client = http::client();
    let configured: Vec<String> = livetv::list_hosts(&state.db)
        .await?
        .into_iter()
        .filter_map(|h| h.device_id)
        .collect();
    let mut found = Vec::new();
    match body
        .address
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        Some(address) => {
            validate_location(TunerKind::HdHomeRun, address)?;
            let info = hdhomerun::device_info(&client, address)
                .await
                .map_err(|_| {
                    ApiError::new(
                        StatusCode::BAD_GATEWAY,
                        "tuner_unreachable",
                        "Unter dieser Adresse antwortet kein HDHomeRun.",
                    )
                })?;
            found.push(DiscoveredTunerDto {
                device_id: info.device_id.clone().unwrap_or_default(),
                url: hdhomerun::base_url(address),
                tuner_count: info.tuner_count,
                name: info.friendly_name,
                model: info.model_number,
                configured: false,
            });
        }
        None => {
            let devices = hdhomerun::discover(
                &[hdhomerun::broadcast_target()],
                Duration::from_millis(1500),
            )
            .await
            .map_err(ApiError::internal)?;
            for device in devices {
                let info = hdhomerun::device_info(&client, &device.base_url).await.ok();
                found.push(DiscoveredTunerDto {
                    device_id: device.device_id,
                    url: device.base_url,
                    tuner_count: device.tuner_count,
                    name: info.as_ref().and_then(|i| i.friendly_name.clone()),
                    model: info.and_then(|i| i.model_number),
                    configured: false,
                });
            }
        }
    }
    for device in &mut found {
        device.configured = configured.contains(&device.device_id);
    }
    Ok(Json(found))
}

// ---- Guide sources -----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EpgSourceKind {
    /// XMLTV file or URL.
    Xmltv,
    /// A Schedules Direct lineup (credentials in `updateSchedulesDirect`).
    SchedulesDirect,
    /// The EPG of a tvheadend tuner (created with it).
    Tvheadend,
}

impl EpgSourceKind {
    fn parse(kind: &str) -> Self {
        match kind {
            "schedules_direct" => EpgSourceKind::SchedulesDirect,
            "tvheadend" => EpgSourceKind::Tvheadend,
            _ => EpgSourceKind::Xmltv,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EpgSourceDto {
    pub id: i64,
    pub kind: EpgSourceKind,
    /// The tvheadend tuner this guide belongs to.
    pub tuner_id: Option<i64>,
    pub name: String,
    /// XMLTV URL or file path (gzip allowed); Schedules Direct: the lineup id;
    /// tvheadend: the server.
    pub url: String,
    pub enabled: bool,
    pub channel_count: i64,
    pub program_count: i64,
    pub last_refresh_at: Option<i64>,
    pub last_error: Option<String>,
}

fn source_dto(s: livetv::EpgSource) -> EpgSourceDto {
    EpgSourceDto {
        kind: EpgSourceKind::parse(&s.kind),
        tuner_id: s.tuner_host_id,
        id: s.id,
        name: s.name,
        url: s.url,
        enabled: s.enabled,
        channel_count: s.channel_count,
        program_count: s.program_count,
        last_refresh_at: s.last_refresh_at,
        last_error: s.last_error,
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateEpgSourceRequest {
    pub name: String,
    /// XMLTV: URL (http/https) or absolute file path (`.gz` files are unpacked);
    /// Schedules Direct: the lineup id, e.g. `DEU-1000097-DEFAULT`.
    pub url: String,
    /// Default `xmltv`; tvheadend guides come with their tuner.
    pub kind: Option<EpgSourceKind>,
}

/// `DEU-1000097-DEFAULT`
fn validate_lineup(lineup: &str) -> ApiResult<String> {
    let lineup = lineup.trim();
    if lineup.is_empty()
        || lineup.len() > 64
        || !lineup
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ApiError::validation(
            "Bitte ein Schedules-Direct-Lineup angeben, z. B. „DEU-1000097-DEFAULT“.",
        ));
    }
    Ok(lineup.to_owned())
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct UpdateEpgSourceRequest {
    pub name: Option<String>,
    pub url: Option<String>,
    pub enabled: Option<bool>,
}

fn validate_guide_location(url: &str) -> ApiResult<String> {
    let url = url.trim();
    if url.is_empty()
        || url.len() > 2048
        || url.chars().any(char::is_control)
        || !(http::is_http(url) || is_absolute_path(url))
    {
        return Err(ApiError::validation(
            "Die Programmquelle braucht eine http(s)-Adresse oder einen absoluten Dateipfad.",
        ));
    }
    Ok(url.to_owned())
}

/// Guide sources (administrators).
#[utoipa::path(
    get,
    path = "/livetv/epg-sources",
    operation_id = "listEpgSources",
    tag = "livetv",
    responses(
        (status = OK, description = "Guide sources", body = [EpgSourceDto]),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn list_epg_sources(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<EpgSourceDto>>> {
    let sources = livetv::list_sources(&state.db).await?;
    Ok(Json(sources.into_iter().map(source_dto).collect()))
}

/// Adds an XMLTV guide source and loads it in the background.
#[utoipa::path(
    post,
    path = "/livetv/epg-sources",
    operation_id = "createEpgSource",
    tag = "livetv",
    request_body = CreateEpgSourceRequest,
    responses(
        (status = CREATED, description = "Source added; refresh queued", body = EpgSourceDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid input", body = ErrorBody),
    )
)]
async fn create_epg_source(
    _: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<CreateEpgSourceRequest>,
) -> ApiResult<(StatusCode, Json<EpgSourceDto>)> {
    let name = validate::name(&body.name)?;
    let id = match body.kind.unwrap_or(EpgSourceKind::Xmltv) {
        EpgSourceKind::Xmltv => {
            let url = validate_guide_location(&body.url)?;
            livetv::create_source(&state.db, &name, &url).await?
        }
        EpgSourceKind::SchedulesDirect => {
            let lineup = validate_lineup(&body.url)?;
            livetv::create_source_of_kind(&state.db, &name, &lineup, "schedules_direct", None)
                .await?
        }
        EpgSourceKind::Tvheadend => {
            return Err(ApiError::validation(
                "tvheadend-Programmdaten kommen mit dem tvheadend-Tuner.",
            ));
        }
    };
    jobs::queue_refresh(state.jobs.queue(), Some(id), priority::INTERACTIVE).await?;
    let source = livetv::get_source(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Programmquelle nicht gefunden."))?;
    Ok((StatusCode::CREATED, Json(source_dto(source))))
}

/// Changes a guide source; a new address reloads it.
#[utoipa::path(
    patch,
    path = "/livetv/epg-sources/{id}",
    operation_id = "updateEpgSource",
    tag = "livetv",
    params(("id" = i64, Path, description = "Guide source id")),
    request_body = UpdateEpgSourceRequest,
    responses(
        (status = OK, description = "Updated source", body = EpgSourceDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown source", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid input", body = ErrorBody),
    )
)]
async fn update_epg_source(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<UpdateEpgSourceRequest>,
) -> ApiResult<Json<EpgSourceDto>> {
    let name = body.name.as_deref().map(validate::name).transpose()?;
    let current = livetv::get_source(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Programmquelle nicht gefunden."))?;
    let url = match current.kind.as_str() {
        "schedules_direct" => body.url.as_deref().map(validate_lineup).transpose()?,
        // The tvheadend guide follows its tuner's address.
        "tvheadend" => None,
        _ => body
            .url
            .as_deref()
            .map(validate_guide_location)
            .transpose()?,
    };
    if !livetv::update_source(&state.db, id, name.as_deref(), url.as_deref(), body.enabled).await? {
        return Err(ApiError::not_found("Programmquelle nicht gefunden."));
    }
    if url.is_some() || body.enabled == Some(true) {
        jobs::queue_refresh(state.jobs.queue(), Some(id), priority::INTERACTIVE).await?;
    }
    if body.enabled.is_some() {
        matching::auto_match(&state.db).await?;
    }
    let source = livetv::get_source(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Programmquelle nicht gefunden."))?;
    Ok(Json(source_dto(source)))
}

/// Removes a guide source with its programs.
#[utoipa::path(
    delete,
    path = "/livetv/epg-sources/{id}",
    operation_id = "deleteEpgSource",
    tag = "livetv",
    params(("id" = i64, Path, description = "Guide source id")),
    responses(
        (status = NO_CONTENT, description = "Removed"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown source", body = ErrorBody),
    )
)]
async fn delete_epg_source(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    if !livetv::delete_source(&state.db, id).await? {
        return Err(ApiError::not_found("Programmquelle nicht gefunden."));
    }
    matching::auto_match(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Reloads a guide source now (background job).
#[utoipa::path(
    post,
    path = "/livetv/epg-sources/{id}/refresh",
    operation_id = "refreshEpgSource",
    tag = "livetv",
    params(("id" = i64, Path, description = "Guide source id")),
    responses(
        (status = ACCEPTED, description = "Refresh queued", body = JobQueuedDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown source", body = ErrorBody),
    )
)]
async fn refresh_epg_source(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<(StatusCode, Json<JobQueuedDto>)> {
    livetv::get_source(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Programmquelle nicht gefunden."))?;
    let job_id = jobs::queue_refresh(state.jobs.queue(), Some(id), priority::INTERACTIVE).await?;
    Ok((StatusCode::ACCEPTED, Json(JobQueuedDto { job_id })))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EpgChannelDto {
    pub epg_source_id: i64,
    /// XMLTV channel id.
    pub channel_id: String,
    pub names: Vec<String>,
    pub icon_url: Option<String>,
}

/// Guide channels of the enabled sources, for assigning them by hand (administrators).
#[utoipa::path(
    get,
    path = "/livetv/epg-channels",
    operation_id = "listEpgChannels",
    tag = "livetv",
    responses(
        (status = OK, description = "Guide channels", body = [EpgChannelDto]),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn list_epg_channels(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<EpgChannelDto>>> {
    let channels = livetv::list_epg_channels(&state.db, None).await?;
    let mut out: Vec<EpgChannelDto> = channels
        .into_iter()
        .map(|c| EpgChannelDto {
            names: c.names(),
            epg_source_id: c.epg_source_id,
            channel_id: c.channel_id,
            icon_url: c.icon_url,
        })
        .collect();
    out.sort_by_key(|c| c.names.first().map(|n| n.to_lowercase()));
    Ok(Json(out))
}

// ---- Channels ----------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EpgMatchKind {
    /// Found by the matcher.
    Auto,
    /// Chosen by an administrator.
    Manual,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ChannelEpgDto {
    pub epg_source_id: i64,
    pub epg_channel_id: String,
    pub kind: EpgMatchKind,
    /// Matcher confidence (0–1) for automatic matches.
    pub score: Option<f64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProgramDto {
    pub id: i64,
    pub channel_id: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub title: String,
    pub subtitle: Option<String>,
    pub description: Option<String>,
    pub categories: Vec<String>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    /// `S01E05` or as the guide writes it.
    pub episode_label: Option<String>,
    pub year: Option<i64>,
    pub icon_url: Option<String>,
    pub rating: Option<String>,
    pub is_new: bool,
    /// The parental controls of this profile hide the program (title and texts are
    /// replaced).
    pub restricted: bool,
    /// Scheduled, running or done recording of this broadcast.
    pub recording: Option<ProgramRecordingDto>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProgramRecordingDto {
    pub id: i64,
    pub status: crate::dvr::RecordingStatus,
    /// Recorded by a series rule.
    pub series: bool,
    pub conflict: bool,
}

/// Recordings by broadcast slot `(channel, start)`.
type Marks = HashMap<(i64, i64), ProgramRecordingDto>;

async fn marks(state: &AppState, from_ms: i64, to_ms: i64) -> ApiResult<Marks> {
    Ok(flixnet_db::dvr::between(&state.db, from_ms, to_ms)
        .await?
        .into_iter()
        .filter_map(|r| {
            let status = match r.status.as_str() {
                "recording" => crate::dvr::RecordingStatus::Recording,
                "completed" => crate::dvr::RecordingStatus::Completed,
                "failed" => crate::dvr::RecordingStatus::Failed,
                _ => crate::dvr::RecordingStatus::Scheduled,
            };
            Some((
                (r.channel_id?, r.start_ms),
                ProgramRecordingDto {
                    id: r.id,
                    status,
                    series: r.rule_kind.as_deref() == Some("series"),
                    conflict: r.conflict,
                },
            ))
        })
        .collect())
}

/// The program as the profile may see it: restricted ones keep only their slot and
/// rating.
fn program_dto(p: Program, user: &AuthUser, marks: &Marks) -> ProgramDto {
    let recording = marks.get(&(p.channel, p.start_ms)).cloned();
    if !program_allowed(&user.restriction, p.rating.as_deref(), &p.category_list()) {
        return ProgramDto {
            id: p.id,
            channel_id: p.channel,
            start_ms: p.start_ms,
            end_ms: p.end_ms,
            title: "Gesperrte Sendung".to_owned(),
            subtitle: None,
            description: None,
            categories: Vec::new(),
            season: None,
            episode: None,
            episode_label: None,
            year: None,
            icon_url: None,
            rating: p.rating,
            is_new: false,
            restricted: true,
            recording: None,
        };
    }
    ProgramDto {
        restricted: false,
        recording,
        categories: p.category_list(),
        id: p.id,
        channel_id: p.channel,
        start_ms: p.start_ms,
        end_ms: p.end_ms,
        title: p.title,
        subtitle: p.subtitle,
        description: p.description,
        season: p.season,
        episode: p.episode,
        episode_label: p.episode_label,
        year: p.year,
        icon_url: p.icon_url,
        rating: p.rating,
        is_new: p.is_new,
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ChannelDto {
    pub id: i64,
    pub tuner_id: i64,
    pub number: Option<String>,
    pub name: String,
    pub logo_url: Option<String>,
    pub group: Option<String>,
    pub is_hd: bool,
    pub enabled: bool,
    /// The guide channel this channel shows (`null`: no guide).
    pub epg: Option<ChannelEpgDto>,
    /// An administrator decided this channel has no guide.
    pub epg_disabled: bool,
    /// `tvg-id` from the playlist (administrators).
    pub tvg_id: Option<String>,
    /// What is on now and next.
    pub now: Option<ProgramDto>,
    pub next: Option<ProgramDto>,
}

/// Channel logos go through the image cache: a signed URL of the server instead of
/// the provider's (which may be slow, tracking, or plain HTTP on an HTTPS page).
fn logo_url(state: &AppState, c: &Channel) -> Option<String> {
    c.logo_url.as_deref().filter(|u| http::is_http(u))?;
    let (expires, sig) = state.signer.sign("logo", c.id);
    Some(format!(
        "/api/v1/livetv/channels/{}/logo?exp={expires}&sig={sig}",
        c.id
    ))
}

fn channel_dto(
    state: &AppState,
    c: Channel,
    admin: bool,
    now: Option<ProgramDto>,
    next: Option<ProgramDto>,
) -> ChannelDto {
    let logo = logo_url(state, &c);
    let manual = c.epg_match.as_deref() == Some("manual");
    let epg = match (c.epg_source_id, c.epg_channel_id) {
        (Some(source), Some(channel)) => Some(ChannelEpgDto {
            epg_source_id: source,
            epg_channel_id: channel,
            kind: if manual {
                EpgMatchKind::Manual
            } else {
                EpgMatchKind::Auto
            },
            score: c.epg_match_score,
        }),
        _ => None,
    };
    ChannelDto {
        epg_disabled: manual && epg.is_none(),
        id: c.id,
        tuner_id: c.tuner_host_id,
        number: c.number,
        name: c.name,
        logo_url: logo,
        group: c.group_title,
        is_hd: c.is_hd,
        enabled: c.enabled,
        epg,
        tvg_id: if admin { c.tvg_id } else { None },
        now,
        next,
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ChannelsQuery {
    /// Also list disabled channels (administrators).
    #[serde(default)]
    pub include_disabled: bool,
}

/// Channels in guide order, with what is on now and next.
#[utoipa::path(
    get,
    path = "/livetv/channels",
    operation_id = "listChannels",
    tag = "livetv",
    params(ChannelsQuery),
    responses(
        (status = OK, description = "Channels", body = [ChannelDto]),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn list_channels(
    user: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<ChannelsQuery>,
) -> ApiResult<Json<Vec<ChannelDto>>> {
    live_viewer(&state, &user).await?;
    let all = query.include_disabled && user.is_admin;
    let channels = livetv::list_channels(&state.db, !all).await?;
    let now = flixnet_db::now_ms();
    let marks = marks(&state, now, now + 6 * 3_600_000).await?;
    let ids: Vec<i64> = channels.iter().map(|c| c.id).collect();
    // Now and next: whatever overlaps the coming hours, first two per channel.
    let mut by_channel: HashMap<i64, Vec<Program>> = HashMap::new();
    for program in livetv::programs_between(&state.db, &ids, now, now + 6 * 3_600_000).await? {
        let list = by_channel.entry(program.channel).or_default();
        if list.len() < 2 {
            list.push(program);
        }
    }
    let out = channels
        .into_iter()
        .map(|c| {
            let mut programs = by_channel.remove(&c.id).unwrap_or_default().into_iter();
            let first = programs.next();
            let (current, next) = match first {
                Some(p) if p.start_ms <= now => (Some(p), programs.next()),
                other => (None, other),
            };
            channel_dto(
                &state,
                c,
                user.is_admin,
                current.map(|p| program_dto(p, &user, &marks)),
                next.map(|p| program_dto(p, &user, &marks)),
            )
        })
        .collect();
    Ok(Json(out))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateChannelRequest {
    /// Hidden channels appear neither in the channel list nor in the guide.
    pub enabled: Option<bool>,
}

/// Shows or hides a channel (administrators).
#[utoipa::path(
    patch,
    path = "/livetv/channels/{id}",
    operation_id = "updateChannel",
    tag = "livetv",
    params(("id" = i64, Path, description = "Channel id")),
    request_body = UpdateChannelRequest,
    responses(
        (status = NO_CONTENT, description = "Updated"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown channel", body = ErrorBody),
    )
)]
async fn update_channel(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<UpdateChannelRequest>,
) -> ApiResult<StatusCode> {
    livetv::get_channel(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Sender nicht gefunden."))?;
    if let Some(enabled) = body.enabled {
        livetv::set_channel_enabled(&state.db, id, enabled).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LogoQuery {
    /// Width in pixels (snapped to a few sizes).
    pub w: Option<u32>,
    /// Expiry of a signed URL (unix seconds).
    pub exp: Option<i64>,
    /// Signature of a signed URL.
    pub sig: Option<String>,
}

/// A channel's logo from the image cache (fetched from the provider once).
///
/// Use the signed URL from `listChannels`; alternatively authenticate normally.
#[utoipa::path(
    get,
    path = "/livetv/channels/{id}/logo",
    operation_id = "getChannelLogo",
    tag = "livetv",
    security(()),
    params(("id" = i64, Path, description = "Channel id"), LogoQuery),
    responses(
        (status = OK, description = "The logo (PNG)", content_type = "image/png"),
        (status = UNAUTHORIZED, description = "Neither a valid signature nor a valid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No logo", body = ErrorBody),
    )
)]
async fn get_channel_logo(
    user: Option<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(query): Query<LogoQuery>,
) -> ApiResult<Response> {
    let signed = match (query.exp, query.sig.as_deref()) {
        (Some(exp), Some(sig)) => state.signer.verify("logo", id, exp, sig),
        _ => false,
    };
    if !signed && user.is_none() {
        return Err(ApiError::unauthorized());
    }
    let missing = || ApiError::not_found("Kein Logo.");
    let channel = livetv::get_channel(&state.db, id)
        .await?
        .ok_or_else(missing)?;
    let url = channel
        .logo_url
        .filter(|u| http::is_http(u))
        .ok_or_else(missing)?;
    let image = flixnet_db::metadata::Image {
        id: 0,
        item_id: 0,
        kind: "logo".to_owned(),
        source: "livetv".to_owned(),
        path: None,
        url: Some(url),
        language: None,
        width: None,
        height: None,
        // Not stored: logos need no placeholder.
        blurhash: Some(String::new()),
        score: 0.0,
    };
    let variant = state.images.variant(&image, query.w).await.map_err(|err| {
        tracing::debug!(channel = id, %err, "channel logo unavailable");
        missing()
    })?;
    let bytes = tokio::fs::read(&variant.path)
        .await
        .map_err(ApiError::internal)?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(variant.content_type),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=86400"),
            ),
        ],
        bytes,
    )
        .into_response())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EpgAssignmentMode {
    /// Let the matcher decide (again).
    Auto,
    /// Use the given guide channel.
    Manual,
    /// Show no guide for this channel.
    None,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetChannelEpgRequest {
    pub mode: EpgAssignmentMode,
    /// For `manual`.
    pub epg_source_id: Option<i64>,
    /// For `manual`: the XMLTV channel id.
    pub epg_channel_id: Option<String>,
}

/// Assigns a guide channel by hand, or hands the channel back to the matcher.
#[utoipa::path(
    put,
    path = "/livetv/channels/{id}/epg",
    operation_id = "setChannelEpg",
    tag = "livetv",
    params(("id" = i64, Path, description = "Channel id")),
    request_body = SetChannelEpgRequest,
    responses(
        (status = OK, description = "The channel with its guide assignment", body = ChannelDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown channel", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Unknown guide channel", body = ErrorBody),
    )
)]
async fn set_channel_epg(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<SetChannelEpgRequest>,
) -> ApiResult<Json<ChannelDto>> {
    livetv::get_channel(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Sender nicht gefunden."))?;
    match body.mode {
        EpgAssignmentMode::Auto => {
            livetv::clear_manual_match(&state.db, id).await?;
            matching::auto_match(&state.db).await?;
        }
        EpgAssignmentMode::None => {
            livetv::set_epg_matches(
                &state.db,
                &[EpgMatch {
                    channel_id: id,
                    epg_source_id: None,
                    epg_channel_id: None,
                    kind: Some("manual"),
                    score: None,
                }],
            )
            .await?;
        }
        EpgAssignmentMode::Manual => {
            let (Some(source), Some(channel)) = (body.epg_source_id, body.epg_channel_id) else {
                return Err(ApiError::validation(
                    "Bitte einen Programm-Sender auswählen.",
                ));
            };
            let known = livetv::list_epg_channels(&state.db, Some(source))
                .await?
                .iter()
                .any(|c| c.channel_id == channel);
            if !known {
                return Err(ApiError::validation(
                    "Diesen Programm-Sender gibt es nicht.",
                ));
            }
            livetv::set_epg_matches(
                &state.db,
                &[EpgMatch {
                    channel_id: id,
                    epg_source_id: Some(source),
                    epg_channel_id: Some(channel),
                    kind: Some("manual"),
                    score: None,
                }],
            )
            .await?;
        }
    }
    let channel = livetv::get_channel(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Sender nicht gefunden."))?;
    Ok(Json(channel_dto(&state, channel, true, None, None)))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RematchDto {
    /// Channels that have a guide now.
    pub matched: i64,
}

/// Runs the automatic matcher over all channels without a manual assignment.
#[utoipa::path(
    post,
    path = "/livetv/channels/match",
    operation_id = "rematchChannels",
    tag = "livetv",
    responses(
        (status = OK, description = "Matching done", body = RematchDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn rematch_channels(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<RematchDto>> {
    let matched = matching::auto_match(&state.db).await?;
    Ok(Json(RematchDto {
        matched: matched as i64,
    }))
}

// ---- Guide and programs ------------------------------------------------------------

/// The longest guide window one request may ask for.
const MAX_GUIDE_WINDOW_MS: i64 = 48 * 3_600_000;

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct GuideQuery {
    /// Window start (unix ms).
    pub start_ms: i64,
    /// Window end (unix ms); at most 48 h after the start.
    pub end_ms: i64,
    /// First channel (in guide order).
    #[serde(default)]
    pub offset: i64,
    /// Channels per page (1–200, default 50).
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GuideRowDto {
    pub channel_id: i64,
    pub programs: Vec<ProgramDto>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GuideDto {
    pub start_ms: i64,
    pub end_ms: i64,
    /// Channels in the guide (all pages).
    pub total: i64,
    pub offset: i64,
    /// One row per channel of this page, in guide order (same as `listChannels`).
    pub rows: Vec<GuideRowDto>,
}

/// Programs of a page of channels within a time window (for the guide grid).
#[utoipa::path(
    get,
    path = "/livetv/guide",
    operation_id = "getGuide",
    tag = "livetv",
    params(GuideQuery),
    responses(
        (status = OK, description = "Guide rows", body = GuideDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid window", body = ErrorBody),
    )
)]
async fn get_guide(
    user: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<GuideQuery>,
) -> ApiResult<Json<GuideDto>> {
    live_viewer(&state, &user).await?;
    if query.end_ms <= query.start_ms || query.end_ms - query.start_ms > MAX_GUIDE_WINDOW_MS {
        return Err(ApiError::validation(
            "Das Zeitfenster muss zwischen 0 und 48 Stunden lang sein.",
        ));
    }
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let offset = query.offset.max(0);
    let channels = livetv::list_channels(&state.db, true).await?;
    let total = channels.len() as i64;
    let page: Vec<i64> = channels
        .iter()
        .skip(offset as usize)
        .take(limit as usize)
        .map(|c| c.id)
        .collect();
    let marks = marks(&state, query.start_ms, query.end_ms).await?;
    let mut by_channel: HashMap<i64, Vec<ProgramDto>> = HashMap::new();
    for program in livetv::programs_between(&state.db, &page, query.start_ms, query.end_ms).await? {
        by_channel
            .entry(program.channel)
            .or_default()
            .push(program_dto(program, &user, &marks));
    }
    Ok(Json(GuideDto {
        start_ms: query.start_ms,
        end_ms: query.end_ms,
        total,
        offset,
        rows: page
            .into_iter()
            .map(|id| GuideRowDto {
                channel_id: id,
                programs: by_channel.remove(&id).unwrap_or_default(),
            })
            .collect(),
    }))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ProgramSearchQuery {
    /// Words in title, episode title or description.
    pub q: String,
    /// Most results (1–100, default 30).
    pub limit: Option<i64>,
}

/// Searches programs that have not ended yet, soonest first.
#[utoipa::path(
    get,
    path = "/livetv/programs",
    operation_id = "searchPrograms",
    tag = "livetv",
    params(ProgramSearchQuery),
    responses(
        (status = OK, description = "Matching programs", body = [ProgramDto]),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn search_programs(
    user: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<ProgramSearchQuery>,
) -> ApiResult<Json<Vec<ProgramDto>>> {
    live_viewer(&state, &user).await?;
    let Some(fts) = flixnet_db::browse::fts_query(&query.q) else {
        return Ok(Json(Vec::new()));
    };
    let limit = query.limit.unwrap_or(30).clamp(1, 100);
    let found = livetv::search_programs(&state.db, &fts, flixnet_db::now_ms(), limit).await?;
    let now = flixnet_db::now_ms();
    let marks = marks(&state, now, now + flixnet_livetv::dvr::plan::HORIZON_MS).await?;
    Ok(Json(
        found
            .into_iter()
            // Restricted programs are not found at all.
            .filter(|p| program_allowed(&user.restriction, p.rating.as_deref(), &p.category_list()))
            .map(|p| program_dto(p, &user, &marks))
            .collect(),
    ))
}

/// One program.
#[utoipa::path(
    get,
    path = "/livetv/programs/{id}",
    operation_id = "getProgram",
    tag = "livetv",
    params(("id" = i64, Path, description = "Program id")),
    responses(
        (status = OK, description = "The program", body = ProgramDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown program", body = ErrorBody),
    )
)]
async fn get_program(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<ProgramDto>> {
    live_viewer(&state, &user).await?;
    let program = livetv::get_program(&state.db, id)
        .await?
        .filter(|p| program_allowed(&user.restriction, p.rating.as_deref(), &p.category_list()))
        .ok_or_else(|| ApiError::not_found("Sendung nicht gefunden."))?;
    let marks = marks(&state, program.start_ms, program.end_ms).await?;
    Ok(Json(program_dto(program, &user, &marks)))
}

// ---- Playback ----------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct WatchChannelRequest {
    /// What the client plays; copied streams need its support for the codecs.
    pub device_profile: DeviceProfile,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LiveSessionDto {
    /// Viewer session; end it with `stopLiveSession`.
    pub session_id: String,
    /// Live HLS playlist (the session id is the capability).
    pub playlist_url: String,
    pub channel_id: i64,
    /// Copy or transcode, with the reasons.
    pub plan: PlaybackPlan,
    /// Joined a stream someone else started (no extra tuner used).
    pub shared: bool,
    /// Timeshift: seconds the server keeps for pausing and rewinding (the playlist
    /// lists all of them; the player starts at the live edge).
    pub buffer_secs: i64,
}

/// Codecs the live HLS output (MPEG-TS segments) can carry to a browser.
const TS_VIDEO: &[&str] = &["h264"];
const TS_AUDIO: &[&str] = &["aac", "mp3", "ac3", "eac3"];

/// The client's profile narrowed to what MPEG-TS segments can deliver.
fn live_profile(profile: &DeviceProfile) -> DeviceProfile {
    DeviceProfile {
        containers: vec!["ts".to_owned()],
        video_codecs: profile
            .video_codecs
            .iter()
            .filter(|c| TS_VIDEO.contains(&c.codec.as_str()))
            .cloned()
            .collect::<Vec<VideoCodecSupport>>(),
        audio_codecs: profile
            .audio_codecs
            .iter()
            .filter(|c| TS_AUDIO.contains(&c.codec.as_str()))
            .cloned()
            .collect::<Vec<AudioCodecSupport>>(),
        max_bitrate: profile.max_bitrate,
        multiple_audio_tracks: false,
    }
}

/// What the decision engine needs to know about a channel's stream.
fn live_source(channel: &Channel) -> SourceMedia {
    let (width, height) = match (channel.width, channel.height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
        _ if channel.is_hd => (1920, 1080),
        _ => (720, 576),
    };
    SourceMedia {
        container: "ts".to_owned(),
        bitrate: None,
        video: channel.video_codec.as_ref().map(|codec| SourceVideo {
            index: 0,
            codec: codec.clone(),
            width,
            height,
            bit_depth: 8,
            hdr: "sdr".to_owned(),
        }),
        audio: channel
            .audio_codec
            .iter()
            .map(|codec| SourceAudio {
                index: 1,
                codec: codec.clone(),
                channels: channel.audio_channels.unwrap_or(2),
                is_default: true,
            })
            .collect(),
        subtitles: Vec::new(),
    }
}

fn live_error(err: LiveError) -> ApiError {
    match err {
        LiveError::TunersBusy => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "tuners_busy",
            "Alle Tuner dieser Quelle sind gerade belegt.",
        ),
        LiveError::UnknownSession => ApiError::not_found("Live-Sitzung nicht gefunden."),
        LiveError::Ended => ApiError::new(
            StatusCode::GONE,
            "stream_ended",
            "Der Live-Stream wurde beendet.",
        ),
        LiveError::Failed(reason) | LiveError::Probe(reason) => {
            tracing::warn!(%reason, "live stream failed");
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "stream_failed",
                "Der Sender lässt sich gerade nicht empfangen.",
            )
        }
        LiveError::Timeout => ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "stream_timeout",
            "Der Sender startet nicht.",
        ),
        other => ApiError::internal(other),
    }
}

/// Starts watching a channel: joins its running stream or starts one (if a tuner is
/// free) and answers once the first segments exist.
#[utoipa::path(
    post,
    path = "/livetv/channels/{id}/watch",
    operation_id = "watchChannel",
    tag = "livetv",
    params(("id" = i64, Path, description = "Channel id")),
    request_body = WatchChannelRequest,
    responses(
        (status = OK, description = "Live session", body = LiveSessionDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown or hidden channel", body = ErrorBody),
        (status = BAD_GATEWAY, description = "The channel cannot be received", body = ErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "All tuners are in use", body = ErrorBody),
        (status = GATEWAY_TIMEOUT, description = "The stream did not start in time", body = ErrorBody),
    )
)]
async fn watch_channel(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<WatchChannelRequest>,
) -> ApiResult<Json<LiveSessionDto>> {
    live_viewer(&state, &user).await?;
    let not_found = || ApiError::not_found("Sender nicht gefunden.");
    let mut channel = livetv::get_channel(&state.db, id)
        .await?
        .ok_or_else(not_found)?;
    let host = livetv::get_host(&state.db, channel.tuner_host_id)
        .await?
        .ok_or_else(not_found)?;
    if !channel.enabled || !host.enabled {
        return Err(not_found());
    }
    if !is_stream_url(&channel.stream_url) {
        return Err(live_error(LiveError::Failed(
            "unsupported stream URL".into(),
        )));
    }
    // What runs now must be allowed for the profile.
    let now = flixnet_db::now_ms();
    let running = livetv::programs_between(&state.db, &[id], now, now + 1).await?;
    if running
        .iter()
        .any(|p| !program_allowed(&user.restriction, p.rating.as_deref(), &p.category_list()))
    {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "program_restricted",
            "Diese Sendung ist für dein Profil gesperrt.",
        ));
    }
    let source = stream_source(&host, &channel.stream_url);
    // No free tuner: say so before a probe would need one.
    if host.tuner_count > 0
        && state.livetv.tuners_in_use(host.id) as i64 >= host.tuner_count
        && !state.livetv.is_streaming(id)
    {
        return Err(live_error(LiveError::TunersBusy));
    }

    // Codecs: from the lineup, or probed once and remembered.
    if channel.video_codec.is_none() && channel.probed_at.is_none() {
        let info = state.livetv.probe(&source).await.map_err(live_error)?;
        let video = info
            .streams
            .iter()
            .find(|s| s.kind == flixnet_media::StreamKind::Video);
        let audio = info
            .streams
            .iter()
            .find(|s| s.kind == flixnet_media::StreamKind::Audio);
        let props = livetv::StreamProps {
            video_codec: video.and_then(|v| v.codec.clone()),
            audio_codec: audio.and_then(|a| a.codec.clone()),
            width: video.and_then(|v| v.width),
            height: video.and_then(|v| v.height),
            audio_channels: audio.and_then(|a| a.channels),
        };
        livetv::set_stream_props(&state.db, id, &props).await?;
        channel.video_codec = props.video_codec;
        channel.audio_codec = props.audio_codec;
        channel.width = props.width;
        channel.height = props.height;
        channel.audio_channels = props.audio_channels;
    }

    let mut plan = decision::decide(
        &live_source(&channel),
        &live_profile(&body.device_profile),
        PlaybackChoice::default(),
    );
    // A live stream is always repackaged into HLS, even when nothing is converted.
    if plan.method == PlayMethod::DirectPlay {
        plan.method = PlayMethod::Remux;
    }
    let ticket = state
        .livetv
        .watch(WatchRequest {
            user_id: user.user_id,
            channel_id: id,
            tuner_host_id: host.id,
            tuner_limit: host.tuner_count,
            source,
            plan: plan.clone(),
        })
        .await
        .map_err(live_error)?;
    Ok(Json(LiveSessionDto {
        playlist_url: format!("/api/v1/livetv/sessions/{}/live.m3u8", ticket.session_id),
        session_id: ticket.session_id,
        channel_id: id,
        plan,
        shared: ticket.shared,
        buffer_secs: state.livetv.buffer().as_secs() as i64,
    }))
}

fn session_id(id: &str) -> ApiResult<&str> {
    if is_session_id(id) {
        Ok(id)
    } else {
        Err(ApiError::not_found("Live-Sitzung nicht gefunden."))
    }
}

/// Ends a viewer session (the player closed); the stream stops with its last viewer.
#[utoipa::path(
    delete,
    path = "/livetv/sessions/{session}",
    operation_id = "stopLiveSession",
    tag = "livetv",
    params(("session" = String, Path, description = "Session id from watchChannel")),
    responses(
        (status = NO_CONTENT, description = "Stopped (or already gone)"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "Someone else's session", body = ErrorBody),
    )
)]
async fn stop_live_session(
    user: AuthUser,
    State(state): State<AppState>,
    Path(session): Path<String>,
) -> ApiResult<StatusCode> {
    let id = session_id(&session)?;
    match state.livetv.owner(id) {
        None => {}
        Some(owner) if owner == user.user_id || user.is_admin => state.livetv.stop(id).await,
        Some(_) => return Err(ApiError::not_found("Live-Sitzung nicht gefunden.")),
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The live HLS playlist of a viewer session: the whole timeshift buffer as a live
/// (DVR window) playlist with `EXT-X-PROGRAM-DATE-TIME`.
#[utoipa::path(
    get,
    path = "/livetv/sessions/{session}/live.m3u8",
    operation_id = "getLivePlaylist",
    tag = "livetv",
    security(()),
    params(("session" = String, Path, description = "Session id from watchChannel")),
    responses(
        (status = OK, description = "HLS playlist", content_type = "application/vnd.apple.mpegurl", body = String),
        (status = NOT_FOUND, description = "Unknown or expired session", body = ErrorBody),
        (status = GONE, description = "The stream ended", body = ErrorBody),
    )
)]
async fn get_live_playlist(
    State(state): State<AppState>,
    Path(session): Path<String>,
) -> ApiResult<Response> {
    let playlist = state
        .livetv
        .playlist(session_id(&session)?)
        .map_err(live_error)?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/vnd.apple.mpegurl"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        playlist,
    )
        .into_response())
}

/// One MPEG-TS segment (`seg{n}.ts`) of a viewer session.
#[utoipa::path(
    get,
    path = "/livetv/sessions/{session}/{segment}",
    operation_id = "getLiveSegment",
    tag = "livetv",
    security(()),
    params(
        ("session" = String, Path, description = "Session id from watchChannel"),
        ("segment" = String, Path, description = "Segment file name, e.g. `seg12.ts`"),
    ),
    responses(
        (status = OK, description = "Media segment", content_type = "video/mp2t"),
        (status = NOT_FOUND, description = "Unknown session", body = ErrorBody),
        (status = GONE, description = "Segment no longer available", body = ErrorBody),
    )
)]
async fn get_live_segment(
    State(state): State<AppState>,
    Path((session, segment)): Path<(String, String)>,
) -> ApiResult<Response> {
    let path = state
        .livetv
        .segment(session_id(&session)?, &segment)
        .map_err(live_error)?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| live_error(LiveError::Ended))?;
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("video/mp2t")),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=60"),
            ),
        ],
        bytes,
    )
        .into_response())
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LiveStreamDto {
    pub id: String,
    pub channel_id: i64,
    pub channel_name: Option<String>,
    pub tuner_id: i64,
    pub started_at: i64,
    pub video_copied: bool,
    pub audio_copied: bool,
    /// User ids of the viewers (one entry per player).
    pub viewers: Vec<i64>,
    /// Recordings fed by this stream.
    pub recordings: i64,
    /// Seconds of timeshift buffer on disk.
    pub buffered_secs: f64,
    /// Times the upstream was restarted after a failure.
    pub restarts: i64,
    /// Video encoder of the current run (`software` also for copied video).
    pub encoder: flixnet_media::hardware::EncoderKind,
}

/// Running live streams with their viewers (administrators).
#[utoipa::path(
    get,
    path = "/livetv/streams",
    operation_id = "listLiveStreams",
    tag = "livetv",
    responses(
        (status = OK, description = "Running streams", body = [LiveStreamDto]),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn list_live_streams(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<LiveStreamDto>>> {
    let mut out = Vec::new();
    for stream in state.livetv.streams() {
        let channel = livetv::get_channel(&state.db, stream.channel_id).await?;
        out.push(LiveStreamDto {
            id: stream.id,
            channel_id: stream.channel_id,
            channel_name: channel.map(|c| c.name),
            tuner_id: stream.tuner_host_id,
            started_at: stream.started_ms,
            video_copied: stream.video_copied,
            audio_copied: stream.audio_copied,
            viewers: stream.viewers,
            recordings: stream.recordings as i64,
            buffered_secs: stream.buffered_secs,
            restarts: i64::from(stream.restarts),
            encoder: stream.encoder,
        });
    }
    Ok(Json(out))
}

// ---- Settings ----------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct LiveTvSettingsDto {
    /// Timeshift buffer per running channel in minutes (0 = live window only).
    pub timeshift_minutes: i64,
    /// Folder recordings are written to (`Serien/…`, `Filme/…`; each becomes a
    /// library). `null` = the default below.
    pub recordings_path: Option<String>,
    /// The folder used when none is set.
    pub default_recordings_path: String,
    /// Default padding of new recordings (minutes).
    pub padding_before_minutes: i64,
    pub padding_after_minutes: i64,
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct UpdateLiveTvSettings {
    /// 0–360 minutes.
    pub timeshift_minutes: Option<i64>,
    /// Absolute folder; `null` returns to the default.
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub recordings_path: Option<Option<String>>,
    /// 0–120 minutes.
    pub padding_before_minutes: Option<i64>,
    pub padding_after_minutes: Option<i64>,
}

fn settings_dto(state: &AppState, config: &LiveTvConfig) -> LiveTvSettingsDto {
    LiveTvSettingsDto {
        timeshift_minutes: config.timeshift_minutes,
        recordings_path: config.recordings_path.clone(),
        default_recordings_path: state
            .config
            .paths
            .config_dir
            .join("recordings")
            .to_string_lossy()
            .into_owned(),
        padding_before_minutes: config.padding_before_minutes,
        padding_after_minutes: config.padding_after_minutes,
    }
}

/// Live TV settings (administrators).
#[utoipa::path(
    get,
    path = "/livetv/settings",
    operation_id = "getLiveTvSettings",
    tag = "livetv",
    responses(
        (status = OK, description = "Current settings", body = LiveTvSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn get_livetv_settings(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<LiveTvSettingsDto>> {
    Ok(Json(settings_dto(&state, &config::load(&state.db).await?)))
}

/// Changes Live TV settings; a new timeshift buffer applies to running streams at once.
#[utoipa::path(
    put,
    path = "/livetv/settings",
    operation_id = "updateLiveTvSettings",
    tag = "livetv",
    request_body = UpdateLiveTvSettings,
    responses(
        (status = OK, description = "Updated settings", body = LiveTvSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid value", body = ErrorBody),
    )
)]
async fn update_livetv_settings(
    _: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<UpdateLiveTvSettings>,
) -> ApiResult<Json<LiveTvSettingsDto>> {
    let mut current = config::load(&state.db).await?;
    if let Some(minutes) = body.timeshift_minutes {
        if !(0..=config::MAX_TIMESHIFT_MINUTES).contains(&minutes) {
            return Err(ApiError::validation(
                "Der Zeitversatz-Puffer muss zwischen 0 und 360 Minuten liegen.",
            ));
        }
        current.timeshift_minutes = minutes;
    }
    for (value, target) in [
        (
            body.padding_before_minutes,
            &mut current.padding_before_minutes,
        ),
        (
            body.padding_after_minutes,
            &mut current.padding_after_minutes,
        ),
    ] {
        if let Some(minutes) = value {
            if !(0..=config::MAX_PADDING_MINUTES).contains(&minutes) {
                return Err(ApiError::validation(
                    "Vor- und Nachlauf dürfen höchstens 120 Minuten lang sein.",
                ));
            }
            *target = minutes;
        }
    }
    match body.recordings_path {
        Some(Some(path)) if !path.trim().is_empty() => {
            let path = path.trim();
            if !is_absolute_path(path) || path.chars().any(char::is_control) || path.len() > 1024 {
                return Err(ApiError::validation(
                    "Der Aufnahme-Ordner braucht einen absoluten Pfad.",
                ));
            }
            current.recordings_path = Some(path.to_owned());
        }
        Some(_) => current.recordings_path = None,
        None => {}
    }
    config::save(&state.db, &current).await?;
    state.livetv.set_buffer(current.timeshift());
    Ok(Json(settings_dto(&state, &current)))
}

// ---- Schedules Direct ----------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct SchedulesDirectDto {
    pub username: Option<String>,
    /// Whether a password is stored (only its SHA-1, as the API wants it).
    pub has_password: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateSchedulesDirect {
    pub username: String,
    /// Omit to keep the stored one.
    pub password: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SdLineupDto {
    /// Lineup id for a guide source, e.g. `DEU-1000097-DEFAULT`.
    pub lineup: String,
    pub name: Option<String>,
    pub location: Option<String>,
}

async fn sd_credentials(state: &AppState) -> ApiResult<Option<SdCredentials>> {
    Ok(flixnet_db::settings::get(&state.db, schedules_direct::SETTINGS_KEY).await?)
}

fn sd_error(err: SdError) -> ApiError {
    match err {
        SdError::Http(_) => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "schedules_direct_unreachable",
            "Schedules Direct ist nicht erreichbar.",
        ),
        SdError::MissingCredentials => {
            ApiError::validation("Bitte zuerst die Schedules-Direct-Zugangsdaten eintragen.")
        }
        SdError::Api(message) => {
            ApiError::validation(format!("Schedules Direct lehnt ab: {message}"))
        }
    }
}

/// The Schedules Direct account (administrators).
#[utoipa::path(
    get,
    path = "/livetv/schedules-direct",
    operation_id = "getSchedulesDirect",
    tag = "livetv",
    responses(
        (status = OK, description = "The account", body = SchedulesDirectDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn get_schedules_direct(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<SchedulesDirectDto>> {
    let current = sd_credentials(&state).await?;
    Ok(Json(SchedulesDirectDto {
        has_password: current
            .as_ref()
            .is_some_and(|c| !c.password_sha1.is_empty()),
        username: current.map(|c| c.username).filter(|u| !u.is_empty()),
    }))
}

/// Stores the Schedules Direct account after signing in with it.
#[utoipa::path(
    put,
    path = "/livetv/schedules-direct",
    operation_id = "updateSchedulesDirect",
    tag = "livetv",
    request_body = UpdateSchedulesDirect,
    responses(
        (status = OK, description = "Stored", body = SchedulesDirectDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Schedules Direct refused the account", body = ErrorBody),
        (status = BAD_GATEWAY, description = "Schedules Direct unreachable", body = ErrorBody),
    )
)]
async fn update_schedules_direct(
    _: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<UpdateSchedulesDirect>,
) -> ApiResult<Json<SchedulesDirectDto>> {
    let current = sd_credentials(&state).await?.unwrap_or_default();
    let username = optional_text(Some(&body.username), 128)?
        .ok_or_else(|| ApiError::validation("Bitte den Benutzernamen angeben."))?;
    let password_sha1 = match body.password.as_deref().filter(|p| !p.is_empty()) {
        Some(password) => schedules_direct::password_hash(password),
        None => current.password_sha1.clone(),
    };
    let credentials = SdCredentials {
        username,
        password_sha1,
        base_url: current.base_url,
    };
    schedules_direct::token(&http::client(), &credentials)
        .await
        .map_err(sd_error)?;
    flixnet_db::settings::set(&state.db, schedules_direct::SETTINGS_KEY, &credentials).await?;
    Ok(Json(SchedulesDirectDto {
        username: Some(credentials.username),
        has_password: true,
    }))
}

/// Lineups of the Schedules Direct account, to add as guide sources.
#[utoipa::path(
    get,
    path = "/livetv/schedules-direct/lineups",
    operation_id = "listSchedulesDirectLineups",
    tag = "livetv",
    responses(
        (status = OK, description = "Lineups", body = [SdLineupDto]),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "No or refused account", body = ErrorBody),
        (status = BAD_GATEWAY, description = "Schedules Direct unreachable", body = ErrorBody),
    )
)]
async fn list_schedules_direct_lineups(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<SdLineupDto>>> {
    let credentials = sd_credentials(&state)
        .await?
        .ok_or_else(|| sd_error(SdError::MissingCredentials))?;
    let lineups = schedules_direct::lineups(&http::client(), &credentials)
        .await
        .map_err(sd_error)?;
    Ok(Json(
        lineups
            .into_iter()
            .map(|l| SdLineupDto {
                lineup: l.lineup,
                name: l.name,
                location: l.location,
            })
            .collect(),
    ))
}

/// Applies the stored Live TV settings to the running services (server start).
pub(crate) async fn start(state: &AppState) -> Result<(), flixnet_db::DbError> {
    let current = config::load(&state.db).await?;
    state.livetv.set_buffer(current.timeshift());
    Ok(())
}
