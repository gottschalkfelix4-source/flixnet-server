//! DLNA/UPnP media server, so TVs, receivers and apps like VLC or BubbleUPnP find
//! Flixnet on the network and play from it.
//!
//! * **Off by default.** An administrator picks the Flixnet user whose libraries and
//!   parental controls DLNA uses (`PUT /api/v1/settings/network`); DLNA clients send
//!   no credentials, so everything on the network sees what that user sees.
//! * Runs on its own port (default 8099) with SSDP discovery on
//!   239.255.255.250:1900. Like the Jellyfin layer, this protocol is not part of
//!   Flixnet's OpenAPI document: it implements UPnP's MediaServer:1 device with
//!   ContentDirectory:1 (`Browse` with `BrowseMetadata`/`BrowseDirectChildren`) and
//!   ConnectionManager:1.
//! * Media and pictures are served on the same port through signed URLs bound to the
//!   DLNA user; every request re-checks that user's access, so lowering a limit or
//!   switching DLNA off takes effect at once.
//!
//! Object ids: `0` (root: the libraries), `lib:{id}`, `item:{id}` (films, series,
//! seasons, episodes, albums, pictures, books, …), `file:{id}` (an audiobook part).

pub mod didl;
pub mod soap;
pub mod ssdp;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use flixnet_db::browse::{ItemFilter, Restriction};
use flixnet_db::dlna::{self as tree, Node};
use flixnet_db::{libraries, metadata, settings, users};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use utoipa::ToSchema;

use self::didl::{Container, Item, Object, Resource};
use crate::AppState;
use crate::browse::libraries_of;

pub const SETTINGS_KEY: &str = "dlna";
pub const DEFAULT_PORT: u16 = 8099;
/// Most objects one Browse returns (clients page through the rest).
const MAX_PAGE: i64 = 500;

/// Stored DLNA settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DlnaSettings {
    /// The user whose libraries and parental controls DLNA uses; `None` = DLNA off.
    pub user_id: Option<i64>,
    /// Name shown on TVs (default "Flixnet").
    pub friendly_name: Option<String>,
    /// HTTP port (default 8099; 0 = any free port).
    pub port: Option<u16>,
    /// Stable device id, created on first start.
    pub uuid: Option<String>,
}

impl DlnaSettings {
    pub fn port(&self) -> u16 {
        self.port.unwrap_or(DEFAULT_PORT)
    }

    pub fn name(&self) -> String {
        self.friendly_name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "Flixnet".to_owned())
    }
}

pub async fn load_settings(db: &flixnet_db::Db) -> flixnet_db::Result<DlnaSettings> {
    Ok(settings::get(db, SETTINGS_KEY).await?.unwrap_or_default())
}

/// Whether the DLNA server runs, and why not.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct DlnaStatus {
    pub running: bool,
    /// The port it listens on.
    pub port: Option<u16>,
    /// Discovery (SSDP multicast) works; without it clients need the address.
    pub discovery: bool,
    /// What went wrong (German).
    pub error: Option<String>,
}

struct Running {
    settings: DlnaSettings,
    stop_http: Arc<Notify>,
    stop_ssdp: Arc<Notify>,
    http: JoinHandle<()>,
    ssdp: Option<JoinHandle<()>>,
}

/// Starts and stops the DLNA server as its settings change. Cheap to clone.
#[derive(Clone, Default)]
pub struct DlnaService {
    running: Arc<tokio::sync::Mutex<Option<Running>>>,
    status: Arc<std::sync::Mutex<DlnaStatus>>,
}

impl DlnaService {
    pub fn status(&self) -> DlnaStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_status(&self, status: DlnaStatus) -> DlnaStatus {
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status.clone();
        status
    }

    async fn stop_running(running: Option<Running>) {
        let Some(running) = running else { return };
        running.stop_ssdp.notify_one();
        running.stop_http.notify_one();
        if let Some(ssdp) = running.ssdp {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(3), ssdp).await;
        }
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), running.http).await;
    }

    /// Stops the server (shutdown).
    pub async fn stop(&self) {
        let running = self.running.lock().await.take();
        Self::stop_running(running).await;
        self.set_status(DlnaStatus::default());
    }

    /// Starts, restarts or stops the server according to the stored settings.
    pub async fn apply(&self, state: &AppState) -> DlnaStatus {
        let mut settings = match load_settings(&state.db).await {
            Ok(settings) => settings,
            Err(err) => {
                tracing::warn!(%err, "DLNA settings unreadable");
                return self.status();
            }
        };
        let mut running = self.running.lock().await;
        if settings.user_id.is_none() {
            Self::stop_running(running.take()).await;
            return self.set_status(DlnaStatus::default());
        }
        if settings.uuid.is_none() {
            let token = flixnet_core::auth::new_token().token;
            settings.uuid = Some(format!(
                "{}-{}-{}-{}-{}",
                &token[..8],
                &token[8..12],
                &token[12..16],
                &token[16..20],
                &token[20..32]
            ));
            if let Err(err) = settings::set(&state.db, SETTINGS_KEY, &settings).await {
                tracing::warn!(%err, "storing the DLNA device id failed");
            }
        }
        if running.as_ref().is_some_and(|r| r.settings == settings) {
            return self.status();
        }
        Self::stop_running(running.take()).await;

        let bind = SocketAddr::from((Ipv4Addr::UNSPECIFIED, settings.port()));
        let listener = match tokio::net::TcpListener::bind(bind).await {
            Ok(listener) => listener,
            Err(err) => {
                tracing::warn!(%bind, %err, "DLNA server cannot listen");
                return self.set_status(DlnaStatus {
                    error: Some(format!(
                        "Port {} ist nicht verfügbar ({err}).",
                        settings.port()
                    )),
                    ..Default::default()
                });
            }
        };
        let port = listener.local_addr().map_or(settings.port(), |a| a.port());
        let uuid = settings.uuid.clone().unwrap_or_default();
        let context = DlnaContext {
            state: state.clone(),
            uuid: uuid.clone(),
            name: settings.name(),
        };
        let stop_http = Arc::new(Notify::new());
        let http = tokio::spawn({
            let stop = stop_http.clone();
            let app = router(context);
            async move {
                let result = axum::serve(listener, app)
                    .with_graceful_shutdown(async move { stop.notified().await })
                    .await;
                if let Err(err) = result {
                    tracing::warn!(%err, "DLNA server stopped with an error");
                }
            }
        });

        let stop_ssdp = Arc::new(Notify::new());
        let (ssdp, error) = match ssdp::bind_multicast(ssdp::SSDP_PORT) {
            Ok(socket) => {
                let device = ssdp::Device {
                    uuid,
                    http_port: port,
                    server: format!(
                        "{}/1 UPnP/1.0 Flixnet/{}",
                        std::env::consts::OS,
                        flixnet_core::VERSION
                    ),
                };
                let target = SocketAddr::from((ssdp::MULTICAST_ADDR, ssdp::SSDP_PORT));
                (
                    Some(tokio::spawn(ssdp::serve(
                        socket,
                        device,
                        Some(target),
                        stop_ssdp.clone(),
                    ))),
                    None,
                )
            }
            Err(err) => {
                tracing::warn!(%err, "SSDP discovery unavailable");
                (
                    None,
                    Some(format!(
                        "Automatische Erkennung (SSDP) nicht verfügbar ({err}); im Docker-Container Host-Netzwerk verwenden."
                    )),
                )
            }
        };
        tracing::info!(port, discovery = ssdp.is_some(), "DLNA server is listening");
        let status = DlnaStatus {
            running: true,
            port: Some(port),
            discovery: ssdp.is_some(),
            error,
        };
        *running = Some(Running {
            settings,
            stop_http,
            stop_ssdp,
            http,
            ssdp,
        });
        self.set_status(status)
    }
}

// ---- HTTP -----------------------------------------------------------------------------

/// What the DLNA handlers share.
#[derive(Clone)]
pub struct DlnaContext {
    pub state: AppState,
    pub uuid: String,
    pub name: String,
}

/// The DLNA HTTP server: device description, service descriptions, control, media.
pub fn router(context: DlnaContext) -> Router {
    Router::new()
        .route("/description.xml", get(description))
        .route("/ContentDirectory/scpd.xml", get(content_directory_scpd))
        .route("/ConnectionManager/scpd.xml", get(connection_manager_scpd))
        .route("/ContentDirectory/control", post(content_directory_control))
        .route(
            "/ConnectionManager/control",
            post(connection_manager_control),
        )
        .route("/ContentDirectory/event", any(subscribe))
        .route("/ConnectionManager/event", any(subscribe))
        .route("/media/{file_id}", get(media))
        .route("/art/{image_id}", get(art))
        .with_state(context)
}

fn xml(body: String) -> Response {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/xml; charset=\"utf-8\""),
        )],
        body,
    )
        .into_response()
}

fn soap_fault(code: u16, description: &str) -> Response {
    let mut response = xml(soap::fault(code, description));
    *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
    response
}

/// `http://host:port` as the client reached us (URLs must work from its side).
fn base_url(headers: &HeaderMap) -> String {
    headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .filter(|h| !h.is_empty() && !h.contains(['/', ' ', '"', '<', '>']))
        .map(|h| format!("http://{h}"))
        .unwrap_or_else(|| format!("http://{}:{DEFAULT_PORT}", ssdp::lan_ip()))
}

async fn description(State(ctx): State<DlnaContext>) -> Response {
    xml(format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<root xmlns="urn:schemas-upnp-org:device-1-0" xmlns:dlna="urn:schemas-dlna-org:device-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <device>
    <deviceType>{device}</deviceType>
    <friendlyName>{name}</friendlyName>
    <manufacturer>Flixnet</manufacturer>
    <manufacturerURL>https://github.com/gottschalkfelix4-source/flixnet-server</manufacturerURL>
    <modelDescription>Flixnet Media Server</modelDescription>
    <modelName>Flixnet</modelName>
    <modelNumber>{version}</modelNumber>
    <UDN>uuid:{uuid}</UDN>
    <dlna:X_DLNADOC>DMS-1.50</dlna:X_DLNADOC>
    <serviceList>
      <service>
        <serviceType>{cd}</serviceType>
        <serviceId>urn:upnp-org:serviceId:ContentDirectory</serviceId>
        <SCPDURL>/ContentDirectory/scpd.xml</SCPDURL>
        <controlURL>/ContentDirectory/control</controlURL>
        <eventSubURL>/ContentDirectory/event</eventSubURL>
      </service>
      <service>
        <serviceType>{cm}</serviceType>
        <serviceId>urn:upnp-org:serviceId:ConnectionManager</serviceId>
        <SCPDURL>/ConnectionManager/scpd.xml</SCPDURL>
        <controlURL>/ConnectionManager/control</controlURL>
        <eventSubURL>/ConnectionManager/event</eventSubURL>
      </service>
    </serviceList>
  </device>
</root>"#,
        device = ssdp::DEVICE_TYPE,
        name = didl::escape(&ctx.name),
        version = flixnet_core::VERSION,
        uuid = didl::escape(&ctx.uuid),
        cd = ssdp::CONTENT_DIRECTORY,
        cm = ssdp::CONNECTION_MANAGER,
    ))
}

fn argument(name: &str, direction: &str, variable: &str) -> String {
    format!(
        "<argument><name>{name}</name><direction>{direction}</direction><relatedStateVariable>{variable}</relatedStateVariable></argument>"
    )
}

fn scpd(actions: &[(&str, Vec<String>)], variables: &[(&str, &str, bool)]) -> String {
    let mut out = String::from(
        r#"<?xml version="1.0" encoding="utf-8"?><scpd xmlns="urn:schemas-upnp-org:service-1-0"><specVersion><major>1</major><minor>0</minor></specVersion><actionList>"#,
    );
    for (name, args) in actions {
        out.push_str(&format!(
            "<action><name>{name}</name><argumentList>{}</argumentList></action>",
            args.concat()
        ));
    }
    out.push_str("</actionList><serviceStateTable>");
    for (name, kind, events) in variables {
        out.push_str(&format!(
            r#"<stateVariable sendEvents="{}"><name>{name}</name><dataType>{kind}</dataType></stateVariable>"#,
            if *events { "yes" } else { "no" }
        ));
    }
    out.push_str("</serviceStateTable></scpd>");
    out
}

async fn content_directory_scpd() -> Response {
    xml(scpd(
        &[
            (
                "Browse",
                vec![
                    argument("ObjectID", "in", "A_ARG_TYPE_ObjectID"),
                    argument("BrowseFlag", "in", "A_ARG_TYPE_BrowseFlag"),
                    argument("Filter", "in", "A_ARG_TYPE_Filter"),
                    argument("StartingIndex", "in", "A_ARG_TYPE_Index"),
                    argument("RequestedCount", "in", "A_ARG_TYPE_Count"),
                    argument("SortCriteria", "in", "A_ARG_TYPE_SortCriteria"),
                    argument("Result", "out", "A_ARG_TYPE_Result"),
                    argument("NumberReturned", "out", "A_ARG_TYPE_Count"),
                    argument("TotalMatches", "out", "A_ARG_TYPE_Count"),
                    argument("UpdateID", "out", "A_ARG_TYPE_UpdateID"),
                ],
            ),
            (
                "GetSearchCapabilities",
                vec![argument("SearchCaps", "out", "SearchCapabilities")],
            ),
            (
                "GetSortCapabilities",
                vec![argument("SortCaps", "out", "SortCapabilities")],
            ),
            (
                "GetSystemUpdateID",
                vec![argument("Id", "out", "SystemUpdateID")],
            ),
        ],
        &[
            ("A_ARG_TYPE_ObjectID", "string", false),
            ("A_ARG_TYPE_BrowseFlag", "string", false),
            ("A_ARG_TYPE_Filter", "string", false),
            ("A_ARG_TYPE_Index", "ui4", false),
            ("A_ARG_TYPE_Count", "ui4", false),
            ("A_ARG_TYPE_SortCriteria", "string", false),
            ("A_ARG_TYPE_Result", "string", false),
            ("A_ARG_TYPE_UpdateID", "ui4", false),
            ("SearchCapabilities", "string", false),
            ("SortCapabilities", "string", false),
            ("SystemUpdateID", "ui4", true),
        ],
    ))
}

async fn connection_manager_scpd() -> Response {
    xml(scpd(
        &[
            (
                "GetProtocolInfo",
                vec![
                    argument("Source", "out", "SourceProtocolInfo"),
                    argument("Sink", "out", "SinkProtocolInfo"),
                ],
            ),
            (
                "GetCurrentConnectionIDs",
                vec![argument("ConnectionIDs", "out", "CurrentConnectionIDs")],
            ),
            (
                "GetCurrentConnectionInfo",
                vec![
                    argument("ConnectionID", "in", "A_ARG_TYPE_ConnectionID"),
                    argument("RcsID", "out", "A_ARG_TYPE_RcsID"),
                    argument("AVTransportID", "out", "A_ARG_TYPE_AVTransportID"),
                    argument("ProtocolInfo", "out", "A_ARG_TYPE_ProtocolInfo"),
                    argument(
                        "PeerConnectionManager",
                        "out",
                        "A_ARG_TYPE_ConnectionManager",
                    ),
                    argument("PeerConnectionID", "out", "A_ARG_TYPE_ConnectionID"),
                    argument("Direction", "out", "A_ARG_TYPE_Direction"),
                    argument("Status", "out", "A_ARG_TYPE_ConnectionStatus"),
                ],
            ),
        ],
        &[
            ("SourceProtocolInfo", "string", true),
            ("SinkProtocolInfo", "string", true),
            ("CurrentConnectionIDs", "string", true),
            ("A_ARG_TYPE_ConnectionID", "i4", false),
            ("A_ARG_TYPE_RcsID", "i4", false),
            ("A_ARG_TYPE_AVTransportID", "i4", false),
            ("A_ARG_TYPE_ProtocolInfo", "string", false),
            ("A_ARG_TYPE_ConnectionManager", "string", false),
            ("A_ARG_TYPE_Direction", "string", false),
            ("A_ARG_TYPE_ConnectionStatus", "string", false),
        ],
    ))
}

/// Eventing is not implemented; subscriptions are accepted so strict clients go on.
async fn subscribe(method: Method) -> Response {
    let mut response = StatusCode::OK.into_response();
    if method.as_str() == "SUBSCRIBE" {
        let sid = format!("uuid:{}", &flixnet_core::auth::new_token().token[..32]);
        if let Ok(sid) = HeaderValue::from_str(&sid) {
            response.headers_mut().insert("SID", sid);
        }
        response
            .headers_mut()
            .insert("TIMEOUT", HeaderValue::from_static("Second-1800"));
    }
    response
}

/// Every protocol the server can stream (GetProtocolInfo).
const SOURCE_MIMES: &[&str] = &[
    "video/x-matroska",
    "video/mp4",
    "video/quicktime",
    "video/x-msvideo",
    "video/mp2t",
    "video/webm",
    "video/mpeg",
    "audio/mpeg",
    "audio/mp4",
    "audio/aac",
    "audio/flac",
    "audio/ogg",
    "audio/wav",
    "image/jpeg",
];

async fn connection_manager_control(body: String) -> Response {
    let Some(action) = soap::parse_action(&body) else {
        return soap_fault(401, "Invalid Action");
    };
    let service = ssdp::CONNECTION_MANAGER;
    match action.name.as_str() {
        "GetProtocolInfo" => {
            let source = SOURCE_MIMES
                .iter()
                .map(|m| didl::protocol_info(m))
                .collect::<Vec<_>>()
                .join(",");
            xml(soap::response(
                service,
                "GetProtocolInfo",
                &[("Source", source), ("Sink", String::new())],
            ))
        }
        "GetCurrentConnectionIDs" => xml(soap::response(
            service,
            "GetCurrentConnectionIDs",
            &[("ConnectionIDs", "0".to_owned())],
        )),
        "GetCurrentConnectionInfo" => xml(soap::response(
            service,
            "GetCurrentConnectionInfo",
            &[
                ("RcsID", "-1".to_owned()),
                ("AVTransportID", "-1".to_owned()),
                ("ProtocolInfo", String::new()),
                ("PeerConnectionManager", String::new()),
                ("PeerConnectionID", "-1".to_owned()),
                ("Direction", "Output".to_owned()),
                ("Status", "OK".to_owned()),
            ],
        )),
        _ => soap_fault(401, "Invalid Action"),
    }
}

/// The DLNA user's view: libraries and parental controls, read on every request.
struct Viewer {
    user_id: i64,
    library_ids: Vec<i64>,
    restriction: Restriction,
}

impl Viewer {
    fn filter(&self, library_ids: Vec<i64>) -> ItemFilter {
        let mut filter = ItemFilter {
            library_ids,
            ..Default::default()
        };
        filter.restrict(&self.restriction);
        filter
    }

    fn all(&self) -> ItemFilter {
        self.filter(self.library_ids.clone())
    }
}

/// `None` while DLNA is off or its user is gone.
async fn viewer(state: &AppState) -> Option<Viewer> {
    let settings = load_settings(&state.db).await.ok()?;
    let user = users::get(&state.db, settings.user_id?).await.ok()??;
    let library_ids = libraries_of(state, user.id, user.is_admin, None)
        .await
        .ok()?;
    Some(Viewer {
        user_id: user.id,
        restriction: user.restriction(),
        library_ids,
    })
}

async fn content_directory_control(
    State(ctx): State<DlnaContext>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let Some(action) = soap::parse_action(&body) else {
        return soap_fault(401, "Invalid Action");
    };
    let service = ssdp::CONTENT_DIRECTORY;
    match action.name.as_str() {
        "GetSearchCapabilities" => xml(soap::response(
            service,
            "GetSearchCapabilities",
            &[("SearchCaps", String::new())],
        )),
        "GetSortCapabilities" => xml(soap::response(
            service,
            "GetSortCapabilities",
            &[("SortCaps", String::new())],
        )),
        "GetSystemUpdateID" => xml(soap::response(
            service,
            "GetSystemUpdateID",
            &[("Id", system_update_id())],
        )),
        "Browse" => {
            let Some(viewer) = viewer(&ctx.state).await else {
                return soap_fault(501, "Action Failed");
            };
            let object_id = action.arg("ObjectID").unwrap_or("0").trim();
            let start: i64 = action
                .arg("StartingIndex")
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0)
                .max(0);
            let requested: i64 = action
                .arg("RequestedCount")
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            let count = if requested <= 0 {
                MAX_PAGE
            } else {
                requested.min(MAX_PAGE)
            };
            let base = base_url(&headers);
            let browser = Browser {
                ctx: &ctx,
                viewer: &viewer,
                base: &base,
            };
            let result = match action.arg("BrowseFlag").map(str::trim) {
                Some("BrowseMetadata") => browser
                    .metadata(object_id)
                    .await
                    .map(|o| (o.into_iter().collect::<Vec<_>>(), 1)),
                Some("BrowseDirectChildren") => browser.children(object_id, start, count).await,
                _ => return soap_fault(402, "Invalid Args"),
            };
            match result {
                Ok((objects, total)) => xml(soap::response(
                    service,
                    "Browse",
                    &[
                        ("Result", didl::didl(&objects)),
                        ("NumberReturned", objects.len().to_string()),
                        ("TotalMatches", total.to_string()),
                        ("UpdateID", system_update_id()),
                    ],
                )),
                Err(BrowseError::NoSuchObject) => soap_fault(701, "No such object"),
                Err(BrowseError::Internal) => soap_fault(501, "Action Failed"),
            }
        }
        _ => soap_fault(401, "Invalid Action"),
    }
}

/// Changes whenever the server restarts (clients re-read their caches then).
fn system_update_id() -> String {
    static STARTED: std::sync::LazyLock<i64> =
        std::sync::LazyLock::new(|| flixnet_db::now_ms() / 1000 % 1_000_000);
    STARTED.to_string()
}

#[derive(Debug)]
enum BrowseError {
    NoSuchObject,
    Internal,
}

impl From<flixnet_db::DbError> for BrowseError {
    fn from(err: flixnet_db::DbError) -> Self {
        tracing::warn!(%err, "DLNA browse failed");
        BrowseError::Internal
    }
}

enum ObjectId {
    Root,
    Library(i64),
    Item(i64),
    File(i64),
}

fn parse_object_id(id: &str) -> Option<ObjectId> {
    if id == "0" {
        return Some(ObjectId::Root);
    }
    let (prefix, number) = id.split_once(':')?;
    let number: i64 = number.parse().ok()?;
    match prefix {
        "lib" => Some(ObjectId::Library(number)),
        "item" => Some(ObjectId::Item(number)),
        "file" => Some(ObjectId::File(number)),
        _ => None,
    }
}

fn container_class(kind: &str) -> &'static str {
    match kind {
        "photo_album" => "object.container.album.photoAlbum",
        "album" | "audiobook" => "object.container.album.musicAlbum",
        "artist" => "object.container.person.musicArtist",
        _ => "object.container.storageFolder",
    }
}

fn item_class(kind: &str, mime: &str) -> &'static str {
    match kind {
        "movie" => "object.item.videoItem.movie",
        "episode" | "video" => "object.item.videoItem",
        "photo" => "object.item.imageItem.photo",
        "track" => "object.item.audioItem.musicTrack",
        _ if mime.starts_with("audio/") => "object.item.audioItem",
        _ if mime.starts_with("image/") => "object.item.imageItem",
        _ => "object.item.videoItem",
    }
}

fn extension(path: &str) -> String {
    std::path::Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Browse logic for one request.
struct Browser<'a> {
    ctx: &'a DlnaContext,
    viewer: &'a Viewer,
    base: &'a str,
}

impl Browser<'_> {
    fn media_url(&self, file_id: i64) -> String {
        let (exp, sig) = self
            .ctx
            .state
            .signer
            .sign(&format!("stream:{}", self.viewer.user_id), file_id);
        format!(
            "{}/media/{file_id}?u={}&exp={exp}&sig={sig}",
            self.base, self.viewer.user_id
        )
    }

    fn art_url(&self, image_id: i64, width: u32) -> String {
        let (exp, sig) = self.ctx.state.signer.sign("img", image_id);
        format!("{}/art/{image_id}?w={width}&exp={exp}&sig={sig}", self.base)
    }

    async fn libraries(&self) -> Result<Vec<libraries::Library>, BrowseError> {
        Ok(libraries::list(&self.ctx.state.db)
            .await?
            .into_iter()
            .filter(|l| self.viewer.library_ids.contains(&l.id))
            .collect())
    }

    async fn library_object(&self, library: &libraries::Library) -> Result<Object, BrowseError> {
        let (_, total) = tree::children(
            &self.ctx.state.db,
            &self.viewer.filter(vec![library.id]),
            None,
            1,
            0,
        )
        .await?;
        Ok(Object::Container(Container {
            id: format!("lib:{}", library.id),
            parent_id: "0".to_owned(),
            title: library.name.clone(),
            class: "object.container.storageFolder",
            child_count: Some(total),
            album_art: None,
        }))
    }

    fn node_parent(node: &Node) -> String {
        match node.parent_id {
            Some(parent) => format!("item:{parent}"),
            None => format!("lib:{}", node.library_id),
        }
    }

    fn node_title(node: &Node) -> String {
        match (node.kind.as_str(), node.index_number) {
            ("season", Some(n)) if node.title.trim().is_empty() => format!("Staffel {n}"),
            ("episode", Some(n)) => format!("{n}. {}", node.title),
            _ => node.title.clone(),
        }
    }

    fn node_object(&self, node: &Node) -> Object {
        let is_container = node.child_count > 0
            || matches!(
                node.kind.as_str(),
                "series" | "season" | "photo_album" | "artist" | "album" | "audiobook"
            )
            || node.file_id.is_none();
        let parent_id = Self::node_parent(node);
        if is_container {
            let child_count = if node.kind == "audiobook" {
                node.file_count
            } else {
                node.child_count
            };
            return Object::Container(Container {
                id: format!("item:{}", node.id),
                parent_id,
                title: Self::node_title(node),
                class: container_class(&node.kind),
                child_count: Some(child_count),
                album_art: node.poster_id.map(|id| self.art_url(id, 342)),
            });
        }
        let path = node.file_path.clone().unwrap_or_default();
        let resource = if node.kind == "photo" {
            // Pictures go out as JPEG from the image cache (TVs choke on huge originals).
            node.poster_id.map(|id| Resource {
                url: self.art_url(id, 1920),
                mime: "image/jpeg",
                size: None,
                duration_ms: None,
                resolution: None,
            })
        } else {
            node.file_id.map(|file_id| Resource {
                url: self.media_url(file_id),
                mime: didl::mime_type(&extension(&path)),
                size: node.file_size,
                duration_ms: node.duration_ms,
                resolution: node.width.zip(node.height),
            })
        };
        let mime = resource.as_ref().map_or("", |r| r.mime);
        Object::Item(Item {
            id: format!("item:{}", node.id),
            parent_id,
            title: Self::node_title(node),
            class: item_class(&node.kind, mime),
            date: node
                .premiere_date
                .clone()
                .or_else(|| node.year.map(|y| y.to_string())),
            creator: node.author.clone(),
            album: None,
            description: node.overview.clone(),
            album_art: node.poster_id.map(|id| self.art_url(id, 342)),
            resource,
        })
    }

    async fn book_parts(&self, book: &Node) -> Result<Vec<Object>, BrowseError> {
        let parts = tree::parts(&self.ctx.state.db, book.id).await?;
        Ok(parts
            .into_iter()
            .map(|part| {
                let title = std::path::Path::new(&part.path)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Object::Item(Item {
                    id: format!("file:{}", part.id),
                    parent_id: format!("item:{}", book.id),
                    title,
                    class: "object.item.audioItem.audioBook",
                    date: book.year.map(|y| y.to_string()),
                    creator: book.author.clone(),
                    album: Some(book.title.clone()),
                    description: None,
                    album_art: book.poster_id.map(|id| self.art_url(id, 342)),
                    resource: Some(Resource {
                        url: self.media_url(part.id),
                        mime: didl::mime_type(&extension(&part.path)),
                        size: Some(part.size),
                        duration_ms: part.duration_ms,
                        resolution: None,
                    }),
                })
            })
            .collect())
    }

    async fn visible_node(&self, id: i64) -> Result<Node, BrowseError> {
        tree::node(&self.ctx.state.db, &self.viewer.all(), id)
            .await?
            .ok_or(BrowseError::NoSuchObject)
    }

    async fn metadata(&self, object_id: &str) -> Result<Option<Object>, BrowseError> {
        match parse_object_id(object_id).ok_or(BrowseError::NoSuchObject)? {
            ObjectId::Root => {
                let count = self.libraries().await?.len() as i64;
                Ok(Some(Object::Container(Container {
                    id: "0".to_owned(),
                    parent_id: "-1".to_owned(),
                    title: self.ctx.name.clone(),
                    class: "object.container.storageFolder",
                    child_count: Some(count),
                    album_art: None,
                })))
            }
            ObjectId::Library(id) => {
                let library = self
                    .libraries()
                    .await?
                    .into_iter()
                    .find(|l| l.id == id)
                    .ok_or(BrowseError::NoSuchObject)?;
                Ok(Some(self.library_object(&library).await?))
            }
            ObjectId::Item(id) => Ok(Some(self.node_object(&self.visible_node(id).await?))),
            ObjectId::File(id) => {
                let part = tree::part(&self.ctx.state.db, id)
                    .await?
                    .ok_or(BrowseError::NoSuchObject)?;
                let book = self.visible_node(part.item_id).await?;
                let parts = self.book_parts(&book).await?;
                Ok(parts
                    .into_iter()
                    .find(|o| matches!(o, Object::Item(i) if i.id == object_id)))
            }
        }
    }

    async fn children(
        &self,
        object_id: &str,
        start: i64,
        count: i64,
    ) -> Result<(Vec<Object>, i64), BrowseError> {
        match parse_object_id(object_id).ok_or(BrowseError::NoSuchObject)? {
            ObjectId::Root => {
                let libraries = self.libraries().await?;
                let total = libraries.len() as i64;
                let mut out = Vec::new();
                for library in libraries.iter().skip(start as usize).take(count as usize) {
                    out.push(self.library_object(library).await?);
                }
                Ok((out, total))
            }
            ObjectId::Library(id) => {
                if !self.viewer.library_ids.contains(&id) {
                    return Err(BrowseError::NoSuchObject);
                }
                let (nodes, total) = tree::children(
                    &self.ctx.state.db,
                    &self.viewer.filter(vec![id]),
                    None,
                    count,
                    start,
                )
                .await?;
                Ok((nodes.iter().map(|n| self.node_object(n)).collect(), total))
            }
            ObjectId::Item(id) => {
                let node = self.visible_node(id).await?;
                if node.kind == "audiobook" {
                    let parts = self.book_parts(&node).await?;
                    let total = parts.len() as i64;
                    let page = parts
                        .into_iter()
                        .skip(start as usize)
                        .take(count as usize)
                        .collect();
                    return Ok((page, total));
                }
                let (nodes, total) = tree::children(
                    &self.ctx.state.db,
                    &self.viewer.all(),
                    Some(id),
                    count,
                    start,
                )
                .await?;
                Ok((nodes.iter().map(|n| self.node_object(n)).collect(), total))
            }
            ObjectId::File(_) => {
                // Leaves have no children, but must exist.
                self.metadata(object_id)
                    .await?
                    .ok_or(BrowseError::NoSuchObject)?;
                Ok((Vec::new(), 0))
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct MediaQuery {
    u: Option<i64>,
    exp: Option<i64>,
    sig: Option<String>,
}

/// A media file for the DLNA user (signed URL from a Browse result).
async fn media(
    State(ctx): State<DlnaContext>,
    Path(file_id): Path<i64>,
    Query(query): Query<MediaQuery>,
    request: Request,
) -> Response {
    let not_found = || StatusCode::NOT_FOUND.into_response();
    let (Some(u), Some(exp), Some(sig)) = (query.u, query.exp, query.sig.as_deref()) else {
        return not_found();
    };
    // Only the current DLNA user's URLs work (switching the user revokes old ones).
    let Some(viewer) = viewer(&ctx.state).await else {
        return not_found();
    };
    if viewer.user_id != u || !ctx.state.signer.verify_stream(file_id, u, exp, sig) {
        return not_found();
    }
    match crate::playback::accessible_file(&ctx.state, None, Some(u), file_id).await {
        Ok(file) => {
            let mut response = match crate::playback::serve_file(&file, request).await {
                Ok(response) => response,
                Err(err) => return err.into_response(),
            };
            let headers = response.headers_mut();
            headers.insert(
                "transferMode.dlna.org",
                HeaderValue::from_static("Streaming"),
            );
            headers.insert(
                "contentFeatures.dlna.org",
                HeaderValue::from_static(
                    "DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000",
                ),
            );
            if let Ok(mime) = HeaderValue::from_str(didl::mime_type(&extension(&file.path))) {
                headers.insert(header::CONTENT_TYPE, mime);
            }
            response
        }
        Err(err) => err.into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct ArtQuery {
    w: Option<u32>,
    exp: Option<i64>,
    sig: Option<String>,
}

/// A picture or cover (signed URL from a Browse result), resized by the image cache.
async fn art(
    State(ctx): State<DlnaContext>,
    Path(image_id): Path<i64>,
    Query(query): Query<ArtQuery>,
) -> Response {
    let not_found = || StatusCode::NOT_FOUND.into_response();
    let (Some(exp), Some(sig)) = (query.exp, query.sig.as_deref()) else {
        return not_found();
    };
    if !ctx.state.signer.verify("img", image_id, exp, sig) {
        return not_found();
    }
    let Some(viewer) = viewer(&ctx.state).await else {
        return not_found();
    };
    let Ok(Some(image)) = metadata::image(&ctx.state.db, image_id).await else {
        return not_found();
    };
    // The DLNA user must (still) see the picture's item.
    match tree::node(&ctx.state.db, &viewer.all(), image.item_id).await {
        Ok(Some(_)) => {}
        _ => return not_found(),
    }
    match crate::images::serve_image(&ctx.state, &image, query.w).await {
        Ok(response) => response,
        Err(err) => err.into_response(),
    }
}
