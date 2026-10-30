//! SyncPlay ("Gemeinsam schauen"): household members watch the same film or episode
//! in sync, each on their own device.
//!
//! Groups live in memory. REST creates, lists and joins them; members then connect to
//! `/syncplay/groups/{id}/ws`, a WebSocket that carries commands (play, pause, seek,
//! buffering, ready, next item, position reports, clock pings) and the group state.
//! The server owns the playback state machine ([`state`]); clients only follow it.
//!
//! Browsers cannot set headers on WebSockets, so the device token may also travel as
//! the subprotocol `flixnet.auth.<token>` (next to `flixnet`); it never appears in a
//! URL. Every member must be allowed to see the group's item (libraries and parental
//! controls), checked on join and before switching to another item.

pub mod state;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use axum::Json;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Path, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::Response;
use flixnet_db::{browse, items, users};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use self::state::{Command, Correction, GroupState, MemberId, MemberState, Playback};
use crate::AppState;
use crate::auth::{AuthUser, authenticate_token};
use crate::browse::{ensure_visible, libraries_of};
use crate::error::{ApiError, ApiResult, ErrorBody};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_groups, create_group))
        .routes(routes!(get_group, end_group))
        .routes(routes!(join_by_code))
        .routes(routes!(group_socket))
}

/// Subprotocol the server speaks; `flixnet.auth.<token>` carries the device token.
pub const PROTOCOL: &str = "flixnet";
const AUTH_PROTOCOL_PREFIX: &str = "flixnet.auth.";
/// Groups nobody is connected to are dropped after this (a page reload survives).
const EMPTY_GRACE_MS: i64 = 60_000;
/// Reactions members can send.
pub const REACTIONS: &[&str] = &["👍", "😂", "😮", "😢", "❤️", "🔥", "👏", "🍿"];

// ---- Messages -----------------------------------------------------------------------

/// What a member sends over the group WebSocket (JSON text frames).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncPlayClientMessage {
    Play,
    /// `position_ms`: where the member paused (the group position when omitted).
    Pause {
        position_ms: Option<i64>,
    },
    Seek {
        position_ms: i64,
    },
    /// The member's player is waiting for data.
    Buffering,
    /// The member's player can play at the group position.
    Ready,
    /// Everybody switches to this item (e.g. the next episode).
    SetItem {
        item_id: i64,
    },
    /// While playing (about every second): the member's position at its estimate of
    /// the server clock.
    Report {
        position_ms: i64,
        server_ms: i64,
    },
    /// Clock sync; answered with `pong`.
    Ping {
        client_ms: f64,
    },
    Reaction {
        emoji: String,
    },
}

/// The shared playback as every member should show it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SyncPlayStateDto {
    pub group_id: String,
    pub item_id: i64,
    pub playing: bool,
    /// Group position at server time `anchor_ms`; while playing it advances in real
    /// time from there (`anchor_ms` may lie slightly in the future: start then).
    pub position_ms: i64,
    pub anchor_ms: i64,
    /// Server time when this state was sent.
    pub server_ms: i64,
    /// Waiting for members to load or buffer; paused until everybody is ready.
    pub waiting: bool,
    pub members: Vec<MemberState>,
    /// Increases with every change; older states are stale.
    pub revision: u64,
}

/// What the server sends over the group WebSocket (JSON text frames).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncPlayServerMessage {
    /// First message after connecting: the member's id in this group.
    Welcome {
        member_id: MemberId,
    },
    State {
        state: SyncPlayStateDto,
    },
    Pong {
        client_ms: f64,
        server_ms: i64,
    },
    /// Drift correction for this member: play at `rate`, or jump to `seek_to_ms`.
    Correction {
        rate: f64,
        seek_to_ms: Option<i64>,
    },
    Reaction {
        member_id: MemberId,
        name: String,
        emoji: String,
    },
    Error {
        message: String,
    },
    /// The group ended (or the member may no longer see its item).
    Closed {
        reason: String,
    },
}

// ---- Registry -------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SyncPlayGroupDto {
    pub id: String,
    /// Invitation code (`/watch/{code}`).
    pub code: String,
    pub name: String,
    pub owner_user_id: i64,
    pub owner_name: String,
    pub item_id: i64,
    pub item_title: Option<String>,
    /// Profile names of the connected members.
    pub members: Vec<String>,
    pub playing: bool,
    pub created_at: i64,
}

struct Group {
    id: String,
    code: String,
    name: String,
    owner_user_id: i64,
    owner_name: String,
    created_at: i64,
    empty_since: Option<i64>,
    state: GroupState,
    senders: HashMap<MemberId, mpsc::UnboundedSender<SyncPlayServerMessage>>,
}

impl Group {
    fn snapshot(&self, now: i64) -> SyncPlayStateDto {
        let (position_ms, anchor_ms) = match self.state.playback {
            Playback::Paused { position_ms } => (position_ms, now),
            Playback::Playing {
                position_ms,
                anchor_ms,
            } => (position_ms, anchor_ms),
        };
        SyncPlayStateDto {
            group_id: self.id.clone(),
            item_id: self.state.item_id,
            playing: self.state.is_playing(),
            position_ms,
            anchor_ms,
            server_ms: now,
            waiting: self.state.waiting.is_some(),
            members: self.state.members.values().cloned().collect(),
            revision: self.state.revision,
        }
    }

    fn broadcast(&mut self, message: &SyncPlayServerMessage) {
        self.senders
            .retain(|_, tx| tx.send(message.clone()).is_ok());
    }

    fn broadcast_state(&mut self, now: i64) {
        let state = self.snapshot(now);
        self.broadcast(&SyncPlayServerMessage::State { state });
    }

    fn info(&self) -> SyncPlayGroupDto {
        SyncPlayGroupDto {
            id: self.id.clone(),
            code: self.code.clone(),
            name: self.name.clone(),
            owner_user_id: self.owner_user_id,
            owner_name: self.owner_name.clone(),
            item_id: self.state.item_id,
            item_title: None,
            members: self
                .state
                .members
                .values()
                .map(|m| m.name.clone())
                .collect(),
            playing: self.state.is_playing(),
            created_at: self.created_at,
        }
    }
}

#[derive(Default)]
struct Registry {
    groups: HashMap<String, Group>,
    next_member: MemberId,
}

/// All SyncPlay groups of the server. Cheap to clone.
#[derive(Clone, Default)]
pub struct SyncPlay {
    inner: Arc<Mutex<Registry>>,
}

impl SyncPlay {
    fn lock(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Starts a group around `item_id` (paused at `position_ms`).
    pub fn create(
        &self,
        owner: &AuthUser,
        name: String,
        item_id: i64,
        position_ms: i64,
    ) -> SyncPlayGroupDto {
        let token = flixnet_core::auth::new_token().token;
        let now = flixnet_db::now_ms();
        let group = Group {
            id: token[..16].to_owned(),
            code: token[16..26].to_owned(),
            name,
            owner_user_id: owner.user_id,
            owner_name: owner.name.clone(),
            created_at: now,
            empty_since: Some(now),
            state: GroupState::new(item_id, position_ms),
            senders: HashMap::new(),
        };
        let info = group.info();
        self.lock().groups.insert(group.id.clone(), group);
        info
    }

    pub fn list(&self) -> Vec<SyncPlayGroupDto> {
        let mut groups: Vec<_> = self.lock().groups.values().map(Group::info).collect();
        groups.sort_by_key(|g| std::cmp::Reverse(g.created_at));
        groups
    }

    pub fn get(&self, id: &str) -> Option<SyncPlayGroupDto> {
        self.lock().groups.get(id).map(Group::info)
    }

    pub fn by_code(&self, code: &str) -> Option<SyncPlayGroupDto> {
        self.lock()
            .groups
            .values()
            .find(|g| g.code == code)
            .map(Group::info)
    }

    /// Ends a group; its members are told so.
    pub fn end(&self, id: &str, reason: &str) -> bool {
        let Some(mut group) = self.lock().groups.remove(id) else {
            return false;
        };
        group.broadcast(&SyncPlayServerMessage::Closed {
            reason: reason.to_owned(),
        });
        true
    }

    /// Adds a member; it gets a welcome and the state on its channel.
    fn join(
        &self,
        id: &str,
        user: &AuthUser,
    ) -> Option<(MemberId, mpsc::UnboundedReceiver<SyncPlayServerMessage>)> {
        let mut registry = self.lock();
        registry.next_member += 1;
        let member_id = registry.next_member;
        let group = registry.groups.get_mut(id)?;
        let now = flixnet_db::now_ms();
        let (tx, rx) = mpsc::unbounded_channel();
        let _ = tx.send(SyncPlayServerMessage::Welcome { member_id });
        group.senders.insert(member_id, tx);
        group.empty_since = None;
        group.state.join(
            MemberState {
                id: member_id,
                user_id: user.user_id,
                name: user.name.clone(),
                ready: false,
            },
            now,
        );
        group.broadcast_state(now);
        Some((member_id, rx))
    }

    fn leave(&self, id: &str, member: MemberId) {
        let mut registry = self.lock();
        let Some(group) = registry.groups.get_mut(id) else {
            return;
        };
        let now = flixnet_db::now_ms();
        group.senders.remove(&member);
        if group.state.leave(member, now) {
            group.broadcast_state(now);
        }
        if group.state.members.is_empty() {
            group.empty_since = Some(now);
        }
    }

    fn command(&self, id: &str, member: MemberId, command: Command) {
        let mut registry = self.lock();
        let Some(group) = registry.groups.get_mut(id) else {
            return;
        };
        let now = flixnet_db::now_ms();
        if let Ok(true) = group.state.apply(member, command, now) {
            group.broadcast_state(now);
        }
    }

    fn report(&self, id: &str, position_ms: i64, at_ms: i64) -> Option<Correction> {
        self.lock()
            .groups
            .get(id)
            .and_then(|g| g.state.drift(position_ms, at_ms))
    }

    fn send_to(&self, id: &str, member: MemberId, message: SyncPlayServerMessage) {
        if let Some(tx) = self
            .lock()
            .groups
            .get(id)
            .and_then(|g| g.senders.get(&member))
        {
            let _ = tx.send(message);
        }
    }

    fn broadcast(&self, id: &str, message: &SyncPlayServerMessage) {
        if let Some(group) = self.lock().groups.get_mut(id) {
            group.broadcast(message);
        }
    }

    fn member_name(&self, id: &str, member: MemberId) -> Option<String> {
        self.lock()
            .groups
            .get(id)
            .and_then(|g| g.state.members.get(&member).map(|m| m.name.clone()))
    }

    /// Users of the connected members (deduplicated).
    fn member_users(&self, id: &str) -> Vec<i64> {
        let mut users: Vec<i64> = self
            .lock()
            .groups
            .get(id)
            .map(|g| g.state.members.values().map(|m| m.user_id).collect())
            .unwrap_or_default();
        users.sort_unstable();
        users.dedup();
        users
    }

    /// Housekeeping once a second: wait timeouts, empty groups.
    fn tick(&self, now: i64) {
        let mut registry = self.lock();
        registry.groups.retain(|_, g| {
            g.empty_since
                .is_none_or(|since| now - since < EMPTY_GRACE_MS)
        });
        for group in registry.groups.values_mut() {
            if group.state.tick(now) {
                group.broadcast_state(now);
            }
        }
    }

    /// Runs [`SyncPlay::tick`] in the background while the registry exists.
    pub fn spawn_ticker(&self) {
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(ticker(Arc::downgrade(&self.inner)));
        }
    }
}

async fn ticker(inner: Weak<Mutex<Registry>>) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    loop {
        interval.tick().await;
        let Some(inner) = inner.upgrade() else {
            return;
        };
        SyncPlay { inner }.tick(flixnet_db::now_ms());
    }
}

// ---- Access ---------------------------------------------------------------------------

/// Whether a user (by id, with their current libraries and parental controls) may
/// see an item.
async fn user_may_see(state: &AppState, user_id: i64, item_id: i64) -> ApiResult<bool> {
    let Some(user) = users::get(&state.db, user_id).await? else {
        return Ok(false);
    };
    let library_ids = libraries_of(state, user.id, user.is_admin, None).await?;
    Ok(browse::is_visible(&state.db, item_id, &library_ids, user.restriction()).await?)
}

/// Only films and episodes are watched together.
async fn ensure_watchable(state: &AppState, user: &AuthUser, item_id: i64) -> ApiResult<String> {
    ensure_visible(state, user, item_id).await?;
    let item = items::get(&state.db, item_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Eintrag nicht gefunden."))?;
    if item.kind != "movie" && item.kind != "episode" {
        return Err(ApiError::validation(
            "Gemeinsam schauen geht nur mit Filmen und Folgen.",
        ));
    }
    Ok(item.title)
}

async fn with_title(state: &AppState, mut group: SyncPlayGroupDto) -> ApiResult<SyncPlayGroupDto> {
    group.item_title = items::get(&state.db, group.item_id).await?.map(|i| i.title);
    Ok(group)
}

/// A group the user may see (its item is visible to them), else 404.
async fn visible_group(
    state: &AppState,
    user: &AuthUser,
    group: Option<SyncPlayGroupDto>,
) -> ApiResult<SyncPlayGroupDto> {
    let not_found = || ApiError::not_found("Gruppe nicht gefunden.");
    let group = group.ok_or_else(not_found)?;
    ensure_visible(state, user, group.item_id)
        .await
        .map_err(|_| not_found())?;
    with_title(state, group).await
}

// ---- REST -----------------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateSyncPlayGroup {
    /// Film or episode to watch.
    pub item_id: i64,
    /// Where to start (ms, default 0).
    pub position_ms: Option<i64>,
    /// Shown in the group list (default: "Gruppe von <Name>").
    pub name: Option<String>,
}

/// Active groups of the household whose item the user may see.
#[utoipa::path(
    get,
    path = "/syncplay/groups",
    operation_id = "listSyncPlayGroups",
    tag = "syncplay",
    responses(
        (status = OK, description = "Groups, newest first", body = Vec<SyncPlayGroupDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn list_groups(
    user: AuthUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<SyncPlayGroupDto>>> {
    let mut out = Vec::new();
    for group in state.syncplay.list() {
        if ensure_visible(&state, &user, group.item_id).await.is_ok() {
            out.push(with_title(&state, group).await?);
        }
    }
    Ok(Json(out))
}

/// Start a group (the caller joins it over the WebSocket).
#[utoipa::path(
    post,
    path = "/syncplay/groups",
    operation_id = "createSyncPlayGroup",
    tag = "syncplay",
    request_body = CreateSyncPlayGroup,
    responses(
        (status = CREATED, description = "Group created", body = SyncPlayGroupDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) item", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Not a film or episode, or invalid name", body = ErrorBody),
    )
)]
async fn create_group(
    user: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<CreateSyncPlayGroup>,
) -> ApiResult<(StatusCode, Json<SyncPlayGroupDto>)> {
    ensure_watchable(&state, &user, body.item_id).await?;
    let name = match body.name.as_deref().map(str::trim) {
        Some(name) if name.chars().count() > 80 => {
            return Err(ApiError::validation(
                "Der Name ist zu lang (höchstens 80 Zeichen).",
            ));
        }
        Some(name) if !name.is_empty() => name.to_owned(),
        _ => format!("Gruppe von {}", user.name),
    };
    let group = state.syncplay.create(
        &user,
        name,
        body.item_id,
        body.position_ms.unwrap_or(0).max(0),
    );
    tracing::info!(group = %group.id, user = user.user_id, item = body.item_id, "SyncPlay group created");
    Ok((StatusCode::CREATED, Json(with_title(&state, group).await?)))
}

/// One group.
#[utoipa::path(
    get,
    path = "/syncplay/groups/{id}",
    operation_id = "getSyncPlayGroup",
    tag = "syncplay",
    params(("id" = String, Path, description = "Group id")),
    responses(
        (status = OK, description = "The group", body = SyncPlayGroupDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) group", body = ErrorBody),
    )
)]
async fn get_group(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<SyncPlayGroupDto>> {
    let group = state.syncplay.get(&id);
    Ok(Json(visible_group(&state, &user, group).await?))
}

/// End a group for everybody (its creator or an administrator).
#[utoipa::path(
    delete,
    path = "/syncplay/groups/{id}",
    operation_id = "endSyncPlayGroup",
    tag = "syncplay",
    params(("id" = String, Path, description = "Group id")),
    responses(
        (status = NO_CONTENT, description = "Group ended"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Someone else's group", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) group", body = ErrorBody),
    )
)]
async fn end_group(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let group = visible_group(&state, &user, state.syncplay.get(&id)).await?;
    if group.owner_user_id != user.user_id && !user.is_admin {
        return Err(ApiError::forbidden());
    }
    state.syncplay.end(&id, "Die Gruppe wurde beendet.");
    Ok(StatusCode::NO_CONTENT)
}

/// Resolve an invitation code to its group.
#[utoipa::path(
    post,
    path = "/syncplay/join/{code}",
    operation_id = "joinSyncPlayGroup",
    tag = "syncplay",
    params(("code" = String, Path, description = "Invitation code of the group")),
    responses(
        (status = OK, description = "The group to connect to", body = SyncPlayGroupDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "Unknown code, or the item is not visible to the caller", body = ErrorBody),
    )
)]
async fn join_by_code(
    user: AuthUser,
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> ApiResult<Json<SyncPlayGroupDto>> {
    let group = state.syncplay.by_code(code.trim());
    Ok(Json(visible_group(&state, &user, group).await?))
}

// ---- WebSocket ------------------------------------------------------------------------

/// The caller of the WebSocket: token from the `flixnet.auth.<token>` subprotocol,
/// else as for every route (header or `api_key`). Runs before the upgrade, so a
/// request without a valid token gets 401.
pub struct SocketUser(pub AuthUser);

fn protocol_token(parts: &Parts) -> Option<String> {
    parts
        .headers
        .get_all(axum::http::header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .find_map(|p| {
            p.trim()
                .strip_prefix(AUTH_PROTOCOL_PREFIX)
                .map(str::to_owned)
        })
        .filter(|t| !t.is_empty())
}

impl FromRequestParts<AppState> for SocketUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        match protocol_token(parts) {
            Some(token) => authenticate_token(state, &token).await.map(SocketUser),
            None => AuthUser::from_request_parts(parts, state)
                .await
                .map(SocketUser),
        }
    }
}

/// The group's WebSocket (see the module docs and the message schemas
/// `SyncPlayClientMessage` / `SyncPlayServerMessage`).
#[utoipa::path(
    get,
    path = "/syncplay/groups/{id}/ws",
    operation_id = "connectSyncPlayGroup",
    tag = "syncplay",
    params(("id" = String, Path, description = "Group id")),
    responses(
        (status = SWITCHING_PROTOCOLS, description = "WebSocket with subprotocol `flixnet`; JSON text frames: `SyncPlayClientMessage` from the client, `SyncPlayServerMessage` from the server", body = SyncPlayServerMessage),
        (status = BAD_REQUEST, description = "Not a WebSocket upgrade", body = ErrorBody),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) group", body = ErrorBody),
    )
)]
async fn group_socket(
    SocketUser(user): SocketUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    upgrade: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> ApiResult<Response> {
    visible_group(&state, &user, state.syncplay.get(&id)).await?;
    let upgrade = upgrade.map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "websocket_required",
            "Nur als WebSocket erreichbar.",
        )
    })?;
    Ok(upgrade
        .protocols([PROTOCOL])
        .on_upgrade(move |socket| run_member(state, user, id, socket)))
}

async fn run_member(state: AppState, user: AuthUser, group_id: String, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let Some((member_id, mut outgoing)) = state.syncplay.join(&group_id, &user) else {
        let closed = SyncPlayServerMessage::Closed {
            reason: "Die Gruppe gibt es nicht mehr.".to_owned(),
        };
        if let Ok(text) = serde_json::to_string(&closed) {
            let _ = sink.send(Message::Text(text.into())).await;
        }
        return;
    };
    tracing::debug!(group = %group_id, member = member_id, user = user.user_id, "SyncPlay member joined");
    loop {
        tokio::select! {
            message = outgoing.recv() => {
                let Some(message) = message else { break };
                let closing = matches!(message, SyncPlayServerMessage::Closed { .. });
                let Ok(text) = serde_json::to_string(&message) else { continue };
                if sink.send(Message::Text(text.into())).await.is_err() || closing {
                    break;
                }
            }
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    handle_message(&state, &group_id, member_id, text.as_str()).await;
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                Some(Ok(_)) => {} // binary frames are ignored; pings are answered by axum
            }
        }
    }
    state.syncplay.leave(&group_id, member_id);
    tracing::debug!(group = %group_id, member = member_id, "SyncPlay member left");
}

async fn handle_message(state: &AppState, group_id: &str, member: MemberId, text: &str) {
    let syncplay = &state.syncplay;
    let message = match serde_json::from_str::<SyncPlayClientMessage>(text) {
        Ok(message) => message,
        Err(_) => {
            syncplay.send_to(
                group_id,
                member,
                SyncPlayServerMessage::Error {
                    message: "Unbekannte Nachricht.".to_owned(),
                },
            );
            return;
        }
    };
    match message {
        SyncPlayClientMessage::Play => syncplay.command(group_id, member, Command::Play),
        SyncPlayClientMessage::Pause { position_ms } => {
            syncplay.command(group_id, member, Command::Pause(position_ms))
        }
        SyncPlayClientMessage::Seek { position_ms } => {
            syncplay.command(group_id, member, Command::Seek(position_ms))
        }
        SyncPlayClientMessage::Buffering => syncplay.command(group_id, member, Command::Buffering),
        SyncPlayClientMessage::Ready => syncplay.command(group_id, member, Command::Ready),
        SyncPlayClientMessage::SetItem { item_id } => {
            match check_new_item(state, group_id, item_id).await {
                Ok(()) => syncplay.command(group_id, member, Command::SetItem(item_id)),
                Err(message) => {
                    syncplay.send_to(group_id, member, SyncPlayServerMessage::Error { message })
                }
            }
        }
        SyncPlayClientMessage::Report {
            position_ms,
            server_ms,
        } => {
            // A report from the future (bad clock estimate) counts as now.
            let at = server_ms.min(flixnet_db::now_ms());
            match syncplay.report(group_id, position_ms, at) {
                Some(Correction::Rate(rate)) => syncplay.send_to(
                    group_id,
                    member,
                    SyncPlayServerMessage::Correction {
                        rate,
                        seek_to_ms: None,
                    },
                ),
                Some(Correction::Seek(position)) => syncplay.send_to(
                    group_id,
                    member,
                    SyncPlayServerMessage::Correction {
                        rate: 1.0,
                        seek_to_ms: Some(position),
                    },
                ),
                Some(Correction::InSync) => syncplay.send_to(
                    group_id,
                    member,
                    SyncPlayServerMessage::Correction {
                        rate: 1.0,
                        seek_to_ms: None,
                    },
                ),
                None => {}
            }
        }
        SyncPlayClientMessage::Ping { client_ms } => syncplay.send_to(
            group_id,
            member,
            SyncPlayServerMessage::Pong {
                client_ms,
                server_ms: flixnet_db::now_ms(),
            },
        ),
        SyncPlayClientMessage::Reaction { emoji } => {
            if REACTIONS.contains(&emoji.as_str())
                && let Some(name) = syncplay.member_name(group_id, member)
            {
                syncplay.broadcast(
                    group_id,
                    &SyncPlayServerMessage::Reaction {
                        member_id: member,
                        name,
                        emoji,
                    },
                );
            }
        }
    }
}

/// Every member must be allowed to see the next item.
async fn check_new_item(state: &AppState, group_id: &str, item_id: i64) -> Result<(), String> {
    let item = items::get(&state.db, item_id)
        .await
        .map_err(|e| e.to_string())?
        .filter(|i| i.kind == "movie" || i.kind == "episode")
        .ok_or_else(|| "Dieser Titel lässt sich nicht gemeinsam schauen.".to_owned())?;
    for user_id in state.syncplay.member_users(group_id) {
        let allowed = user_may_see(state, user_id, item.id)
            .await
            .map_err(|_| "Prüfung fehlgeschlagen.".to_owned())?;
        if !allowed {
            return Err("Nicht alle Mitglieder dürfen diesen Titel sehen.".to_owned());
        }
    }
    Ok(())
}
