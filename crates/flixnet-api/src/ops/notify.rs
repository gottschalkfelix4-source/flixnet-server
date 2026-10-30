//! Notifications: server events go out as signed JSON webhooks, ntfy pushes, Discord
//! messages or e-mails (SMTP).
//!
//! A listener on the event bus turns each relevant event into one delivery job per
//! subscribed target, so failed deliveries are retried with backoff by the job system
//! and survive restarts.
//!
//! Webhook requests carry `X-Flixnet-Event` (the event kind) and, when the target has
//! a secret, `X-Flixnet-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256>` computed over
//! `"<t>.<raw body>"` with the secret as key.

use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use flixnet_core::events::{NOTIFICATION_KINDS, PlaybackEvent, ServerEvent};
use flixnet_core::jobs::{JobContext, JobHandler, JobOptions, JobQueue, Pool, priority};
use flixnet_db::ops::{self as db_ops, NewTarget, NotificationTarget};
use flixnet_db::{Db, settings};
use hmac::{Hmac, KeyInit, Mac};
use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AdminUser;
use crate::error::{ApiError, ApiResult, ErrorBody};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(
            list_notification_targets,
            create_notification_target
        ))
        .routes(routes!(
            update_notification_target,
            delete_notification_target
        ))
        .routes(routes!(test_notification_target))
        .routes(routes!(list_notification_events))
        .routes(routes!(get_smtp_settings, update_smtp_settings))
}

// ---- Stored shapes ------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    /// JSON POST to any URL (optionally HMAC-signed).
    Webhook,
    /// Push message via an ntfy server (ntfy.sh or self-hosted).
    Ntfy,
    /// Discord channel webhook.
    Discord,
    /// E-mail through the SMTP server from `/settings/smtp`.
    Email,
}

impl TargetKind {
    fn as_str(self) -> &'static str {
        match self {
            TargetKind::Webhook => "webhook",
            TargetKind::Ntfy => "ntfy",
            TargetKind::Discord => "discord",
            TargetKind::Email => "email",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "webhook" => TargetKind::Webhook,
            "ntfy" => TargetKind::Ntfy,
            "discord" => TargetKind::Discord,
            "email" => TargetKind::Email,
            _ => return None,
        })
    }
}

/// Kind-specific configuration as stored (`config_json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoredConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    topic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    priority: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    webhook_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    recipients: Vec<String>,
}

/// Server-wide SMTP settings (`settings` key `smtp`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SmtpSettings {
    host: Option<String>,
    port: Option<u16>,
    security: Option<SmtpSecurity>,
    username: Option<String>,
    password: Option<String>,
    from_address: Option<String>,
    from_name: Option<String>,
}

const SMTP_KEY: &str = "smtp";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SmtpSecurity {
    /// Plain connection upgraded with STARTTLS (usually port 587).
    #[default]
    Starttls,
    /// TLS from the first byte (usually port 465).
    Tls,
    /// Unencrypted (only for servers in the local network).
    None,
}

// ---- DTOs ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct NotificationTargetDto {
    pub id: i64,
    pub name: String,
    pub kind: TargetKind,
    pub enabled: bool,
    /// Event kinds this target receives (see `listNotificationEvents`).
    pub events: Vec<String>,
    /// Webhook: the URL requests go to.
    pub url: Option<String>,
    /// Webhook: requests are signed (the secret itself is never returned).
    pub has_secret: bool,
    /// ntfy: server, e.g. `https://ntfy.sh`.
    pub server_url: Option<String>,
    /// ntfy: topic.
    pub topic: Option<String>,
    /// ntfy: an access token is stored.
    pub has_token: bool,
    /// ntfy: priority 1–5.
    pub priority: Option<u8>,
    /// Discord: the webhook URL with its secret part masked.
    pub webhook_url: Option<String>,
    /// E-mail: recipient addresses.
    pub recipients: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A notification target to create or replace. Secrets (`secret`, `token`,
/// `webhook_url`) that are omitted or `null` keep their stored value; an empty
/// string removes them.
#[derive(Debug, Deserialize, ToSchema)]
pub struct NotificationTargetInput {
    pub name: String,
    /// Required when creating; ignored on updates.
    pub kind: Option<TargetKind>,
    /// Default `true`.
    pub enabled: Option<bool>,
    #[serde(default)]
    pub events: Vec<String>,
    pub url: Option<String>,
    pub secret: Option<String>,
    pub server_url: Option<String>,
    pub topic: Option<String>,
    pub token: Option<String>,
    pub priority: Option<u8>,
    pub webhook_url: Option<String>,
    #[serde(default)]
    pub recipients: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NotificationEventDto {
    /// Stable name, e.g. `playback.started`.
    pub kind: String,
    /// German label for the UI.
    pub label: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TestResultDto {
    pub ok: bool,
    /// Why it failed (German or the remote server's answer).
    pub error: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SmtpSettingsDto {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub security: SmtpSecurity,
    pub username: Option<String>,
    /// Whether a password is stored (it is never returned).
    pub has_password: bool,
    pub from_address: Option<String>,
    pub from_name: Option<String>,
}

/// SMTP settings. `password` omitted or `null` keeps the stored one; `""` removes it.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateSmtpSettings {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub security: Option<SmtpSecurity>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from_address: Option<String>,
    pub from_name: Option<String>,
}

fn mask_discord(url: &str) -> String {
    // https://discord.com/api/webhooks/<id>/<token> → keep the id, hide the token.
    match url.rsplit_once('/') {
        Some((head, token)) if !token.is_empty() => format!("{head}/••••••"),
        _ => "••••••".to_owned(),
    }
}

fn to_dto(target: &NotificationTarget) -> NotificationTargetDto {
    let config: StoredConfig = serde_json::from_str(&target.config_json).unwrap_or_default();
    NotificationTargetDto {
        id: target.id,
        name: target.name.clone(),
        kind: TargetKind::parse(&target.kind).unwrap_or(TargetKind::Webhook),
        enabled: target.enabled,
        events: serde_json::from_str(&target.events_json).unwrap_or_default(),
        url: config.url,
        has_secret: config.secret.is_some(),
        server_url: config.server_url,
        topic: config.topic,
        has_token: config.token.is_some(),
        priority: config.priority,
        webhook_url: config.webhook_url.as_deref().map(mask_discord),
        recipients: config.recipients,
        created_at: target.created_at,
        updated_at: target.updated_at,
    }
}

// ---- Validation ---------------------------------------------------------------------

fn http_url(value: &str, what: &str) -> ApiResult<String> {
    let value = value.trim();
    match reqwest::Url::parse(value) {
        Ok(url) if matches!(url.scheme(), "http" | "https") && url.host().is_some() => {
            Ok(value.to_owned())
        }
        _ => Err(ApiError::validation(format!(
            "{what} muss eine http(s)-Adresse sein."
        ))),
    }
}

/// `Some("")` removes, `Some(v)` replaces, `None` keeps.
fn merge_secret(stored: Option<String>, input: Option<String>) -> Option<String> {
    match input {
        Some(v) if v.trim().is_empty() => None,
        Some(v) => Some(v.trim().to_owned()),
        None => stored,
    }
}

fn parse_mailbox(value: &str) -> ApiResult<Mailbox> {
    value
        .trim()
        .parse::<Mailbox>()
        .map_err(|_| ApiError::validation(format!("„{value}“ ist keine gültige E-Mail-Adresse.")))
}

/// Validates input against the kind and merges it into the stored configuration.
fn build_config(
    kind: TargetKind,
    stored: StoredConfig,
    input: &NotificationTargetInput,
) -> ApiResult<StoredConfig> {
    let mut config = StoredConfig::default();
    match kind {
        TargetKind::Webhook => {
            let url = input
                .url
                .as_deref()
                .or(stored.url.as_deref())
                .ok_or_else(|| ApiError::validation("Bitte die Webhook-Adresse angeben."))?;
            config.url = Some(http_url(url, "Die Webhook-Adresse")?);
            config.secret = merge_secret(stored.secret, input.secret.clone());
        }
        TargetKind::Ntfy => {
            let server = input
                .server_url
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .or(stored.server_url.as_deref())
                .unwrap_or("https://ntfy.sh");
            config.server_url = Some(http_url(server, "Der ntfy-Server")?);
            let topic = input
                .topic
                .as_deref()
                .or(stored.topic.as_deref())
                .map(str::trim)
                .unwrap_or("");
            if topic.is_empty()
                || topic.len() > 64
                || !topic
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(ApiError::validation(
                    "Das ntfy-Thema darf nur Buchstaben, Ziffern, - und _ enthalten.",
                ));
            }
            config.topic = Some(topic.to_owned());
            config.token = merge_secret(stored.token, input.token.clone());
            if let Some(p) = input.priority {
                if !(1..=5).contains(&p) {
                    return Err(ApiError::validation(
                        "Die ntfy-Priorität liegt zwischen 1 und 5.",
                    ));
                }
                config.priority = Some(p);
            }
        }
        TargetKind::Discord => {
            let url = match input
                .webhook_url
                .as_deref()
                .filter(|u| !u.trim().is_empty())
            {
                Some(url) => Some(http_url(url, "Die Discord-Webhook-Adresse")?),
                None => stored.webhook_url,
            };
            config.webhook_url = Some(url.ok_or_else(|| {
                ApiError::validation("Bitte die Discord-Webhook-Adresse angeben.")
            })?);
        }
        TargetKind::Email => {
            let recipients: Vec<String> = if input.recipients.is_empty() {
                stored.recipients
            } else {
                input
                    .recipients
                    .iter()
                    .map(|r| r.trim().to_owned())
                    .filter(|r| !r.is_empty())
                    .collect()
            };
            if recipients.is_empty() {
                return Err(ApiError::validation(
                    "Bitte mindestens eine E-Mail-Adresse angeben.",
                ));
            }
            for r in &recipients {
                parse_mailbox(r)?;
            }
            config.recipients = recipients;
        }
    }
    Ok(config)
}

fn validate_common(input: &NotificationTargetInput) -> ApiResult<(String, String)> {
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(ApiError::validation(
            "Der Name muss zwischen 1 und 64 Zeichen lang sein.",
        ));
    }
    let mut events: Vec<String> = Vec::new();
    for event in &input.events {
        if !NOTIFICATION_KINDS.iter().any(|(k, _)| k == event) {
            return Err(ApiError::validation(format!(
                "Unbekanntes Ereignis „{event}“."
            )));
        }
        if !events.contains(event) {
            events.push(event.clone());
        }
    }
    Ok((
        name.to_owned(),
        serde_json::to_string(&events).expect("json"),
    ))
}

// ---- Routes -------------------------------------------------------------------------

/// Notification targets (administrators).
#[utoipa::path(
    get,
    path = "/notifications/targets",
    operation_id = "listNotificationTargets",
    tag = "notifications",
    responses(
        (status = OK, description = "All targets", body = [NotificationTargetDto]),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn list_notification_targets(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<NotificationTargetDto>>> {
    let targets = db_ops::list_targets(&state.db).await?;
    Ok(Json(targets.iter().map(to_dto).collect()))
}

/// Add a notification target.
#[utoipa::path(
    post,
    path = "/notifications/targets",
    operation_id = "createNotificationTarget",
    tag = "notifications",
    request_body = NotificationTargetInput,
    responses(
        (status = OK, description = "Created", body = NotificationTargetDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid target", body = ErrorBody),
    )
)]
async fn create_notification_target(
    _: AdminUser,
    State(state): State<AppState>,
    Json(input): Json<NotificationTargetInput>,
) -> ApiResult<Json<NotificationTargetDto>> {
    let kind = input
        .kind
        .ok_or_else(|| ApiError::validation("Bitte die Art des Ziels angeben."))?;
    let (name, events) = validate_common(&input)?;
    let config = build_config(kind, StoredConfig::default(), &input)?;
    let target = db_ops::create_target(
        &state.db,
        &NewTarget {
            name: &name,
            kind: kind.as_str(),
            enabled: input.enabled.unwrap_or(true),
            config_json: &serde_json::to_string(&config).expect("json"),
            events_json: &events,
        },
    )
    .await?;
    Ok(Json(to_dto(&target)))
}

/// Change a notification target.
#[utoipa::path(
    put,
    path = "/notifications/targets/{id}",
    operation_id = "updateNotificationTarget",
    tag = "notifications",
    params(("id" = i64, Path, description = "Target id")),
    request_body = NotificationTargetInput,
    responses(
        (status = OK, description = "Updated", body = NotificationTargetDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such target", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid target", body = ErrorBody),
    )
)]
async fn update_notification_target(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<NotificationTargetInput>,
) -> ApiResult<Json<NotificationTargetDto>> {
    let existing = db_ops::get_target(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Ziel nicht gefunden."))?;
    let kind = TargetKind::parse(&existing.kind).unwrap_or(TargetKind::Webhook);
    let (name, events) = validate_common(&input)?;
    let stored: StoredConfig = serde_json::from_str(&existing.config_json).unwrap_or_default();
    let config = build_config(kind, stored, &input)?;
    let target = db_ops::update_target(
        &state.db,
        id,
        &NewTarget {
            name: &name,
            kind: kind.as_str(),
            enabled: input.enabled.unwrap_or(existing.enabled),
            config_json: &serde_json::to_string(&config).expect("json"),
            events_json: &events,
        },
    )
    .await?
    .ok_or_else(|| ApiError::not_found("Ziel nicht gefunden."))?;
    Ok(Json(to_dto(&target)))
}

/// Remove a notification target.
#[utoipa::path(
    delete,
    path = "/notifications/targets/{id}",
    operation_id = "deleteNotificationTarget",
    tag = "notifications",
    params(("id" = i64, Path, description = "Target id")),
    responses(
        (status = NO_CONTENT, description = "Removed"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such target", body = ErrorBody),
    )
)]
async fn delete_notification_target(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    if db_ops::delete_target(&state.db, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("Ziel nicht gefunden."))
    }
}

/// Send a test message to a target right away (no retries).
#[utoipa::path(
    post,
    path = "/notifications/targets/{id}/test",
    operation_id = "testNotificationTarget",
    tag = "notifications",
    params(("id" = i64, Path, description = "Target id")),
    responses(
        (status = OK, description = "Whether the test message was accepted", body = TestResultDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such target", body = ErrorBody),
    )
)]
async fn test_notification_target(
    _: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<TestResultDto>> {
    let target = db_ops::get_target(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Ziel nicht gefunden."))?;
    let notification = Notification {
        kind: "test".to_owned(),
        title: "Testnachricht von Flixnet".to_owned(),
        message: "Wenn du das liest, kommen Benachrichtigungen an.".to_owned(),
        timestamp: flixnet_db::now_ms(),
        data: serde_json::json!({ "type": "test" }),
    };
    Ok(Json(
        match send(&state.ops.http, &state.db, &target, &notification).await {
            Ok(()) => TestResultDto {
                ok: true,
                error: None,
            },
            Err(err) => TestResultDto {
                ok: false,
                error: Some(format!("{err:#}")),
            },
        },
    ))
}

/// Event kinds notification targets can subscribe to.
#[utoipa::path(
    get,
    path = "/notifications/events",
    operation_id = "listNotificationEvents",
    tag = "notifications",
    responses(
        (status = OK, description = "Event kinds", body = [NotificationEventDto]),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn list_notification_events(_: AdminUser) -> Json<Vec<NotificationEventDto>> {
    Json(
        NOTIFICATION_KINDS
            .iter()
            .map(|(kind, label)| NotificationEventDto {
                kind: (*kind).to_owned(),
                label: (*label).to_owned(),
            })
            .collect(),
    )
}

fn smtp_dto(s: &SmtpSettings) -> SmtpSettingsDto {
    SmtpSettingsDto {
        host: s.host.clone(),
        port: s.port,
        security: s.security.unwrap_or_default(),
        username: s.username.clone(),
        has_password: s.password.is_some(),
        from_address: s.from_address.clone(),
        from_name: s.from_name.clone(),
    }
}

/// SMTP server for e-mail notifications.
#[utoipa::path(
    get,
    path = "/settings/smtp",
    operation_id = "getSmtpSettings",
    tag = "notifications",
    responses(
        (status = OK, description = "Current settings", body = SmtpSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn get_smtp_settings(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<SmtpSettingsDto>> {
    let current: SmtpSettings = settings::get(&state.db, SMTP_KEY)
        .await?
        .unwrap_or_default();
    Ok(Json(smtp_dto(&current)))
}

/// Change the SMTP server.
#[utoipa::path(
    put,
    path = "/settings/smtp",
    operation_id = "updateSmtpSettings",
    tag = "notifications",
    request_body = UpdateSmtpSettings,
    responses(
        (status = OK, description = "Updated settings", body = SmtpSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid settings", body = ErrorBody),
    )
)]
async fn update_smtp_settings(
    _: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<UpdateSmtpSettings>,
) -> ApiResult<Json<SmtpSettingsDto>> {
    let mut current: SmtpSettings = settings::get(&state.db, SMTP_KEY)
        .await?
        .unwrap_or_default();
    let blank = |v: Option<String>| v.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    if let Some(host) = body.host {
        current.host = blank(Some(host));
    }
    if let Some(port) = body.port {
        if port == 0 {
            return Err(ApiError::validation("Ungültiger Port."));
        }
        current.port = Some(port);
    }
    if let Some(security) = body.security {
        current.security = Some(security);
    }
    if let Some(username) = body.username {
        current.username = blank(Some(username));
    }
    current.password = merge_secret(current.password, body.password);
    if let Some(from) = body.from_address {
        let from = blank(Some(from));
        if let Some(from) = &from {
            parse_mailbox(from)?;
        }
        current.from_address = from;
    }
    if let Some(name) = body.from_name {
        current.from_name = blank(Some(name));
    }
    settings::set(&state.db, SMTP_KEY, &current).await?;
    Ok(Json(smtp_dto(&current)))
}

// ---- Rendering ----------------------------------------------------------------------

/// A rendered notification, as queued for delivery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// Event kind (`playback.started`, … or `test`).
    pub kind: String,
    pub title: String,
    pub message: String,
    /// Unix milliseconds.
    pub timestamp: i64,
    /// The event itself.
    pub data: serde_json::Value,
}

fn clock(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn method_label(method: &str) -> &'static str {
    match method {
        "transcode" => "Transcode",
        "remux" => "Remux",
        _ => "Direct Play",
    }
}

async fn label(db: &Db, item_id: Option<i64>) -> String {
    match item_id {
        Some(id) => flixnet_db::history::item_label(db, id)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "einen Eintrag".to_owned()),
        None => "einen Eintrag".to_owned(),
    }
}

fn device_line(p: &PlaybackEvent) -> String {
    let device = match (&p.device_name, &p.client) {
        (Some(d), Some(c)) => format!("{d} ({c})"),
        (Some(d), None) => d.clone(),
        (None, Some(c)) => c.clone(),
        (None, None) => "Unbekanntes Gerät".to_owned(),
    };
    format!("{device} · {}", method_label(&p.play_method))
}

fn position_line(p: &PlaybackEvent) -> String {
    match p.duration_ms {
        Some(d) if d > 0 => format!("bei {} von {}", clock(p.position_ms), clock(d)),
        _ => format!("bei {}", clock(p.position_ms)),
    }
}

/// Title and text for an event; `None` for events that never notify.
pub async fn render(db: &Db, event: &ServerEvent) -> Option<Notification> {
    let kind = event.notification_kind()?;
    let (title, message) = match event {
        ServerEvent::ItemsAdded {
            library_name,
            count,
            items,
            ..
        } => {
            let title = match (count, items.first()) {
                (1, Some(item)) => format!("Neu in {library_name}: {}", item.title),
                _ => format!("Neu in {library_name}: {count} Einträge"),
            };
            let mut lines: Vec<String> = items.iter().map(|i| format!("• {}", i.title)).collect();
            if *count > items.len() {
                lines.push(format!("… und {} weitere", count - items.len()));
            }
            (title, lines.join("\n"))
        }
        ServerEvent::ScanFinished {
            library_name,
            added,
            changed,
            missing,
            ..
        } => (
            format!("Scan abgeschlossen: {library_name}"),
            format!("{added} neu, {changed} geändert, {missing} fehlen"),
        ),
        ServerEvent::ScanFailed {
            library_name,
            error,
            ..
        } => (
            format!("Scan fehlgeschlagen: {library_name}"),
            error.clone(),
        ),
        ServerEvent::PlaybackStarted(p) => (
            format!(
                "{} schaut {}",
                p.user_name,
                label(db, Some(p.item_id)).await
            ),
            device_line(p),
        ),
        ServerEvent::PlaybackPaused(p) => (
            format!(
                "{} hat {} pausiert",
                p.user_name,
                label(db, Some(p.item_id)).await
            ),
            position_line(p),
        ),
        ServerEvent::PlaybackResumed(p) => (
            format!(
                "{} schaut {} weiter",
                p.user_name,
                label(db, Some(p.item_id)).await
            ),
            position_line(p),
        ),
        ServerEvent::PlaybackStopped(p) => {
            let item = label(db, Some(p.item_id)).await;
            if p.completed {
                (
                    format!("{} hat {item} zu Ende geschaut", p.user_name),
                    device_line(p),
                )
            } else {
                (
                    format!("{} hat {item} beendet", p.user_name),
                    position_line(p),
                )
            }
        }
        ServerEvent::TranscodeFailed { item_id, error, .. } => (
            "Transcoding fehlgeschlagen".to_owned(),
            format!("{}: {error}", label(db, *item_id).await),
        ),
        ServerEvent::RecordingFinished { title, channel, .. } => (
            format!("Aufnahme fertig: {title}"),
            channel.as_ref().map_or_else(
                || "Die Aufnahme ist in der Bibliothek.".to_owned(),
                |c| format!("Aufgenommen auf {c}."),
            ),
        ),
        ServerEvent::UserCreated { name, .. } => (
            format!("Neues Nutzerkonto: {name}"),
            format!("{name} wurde in Flixnet angelegt."),
        ),
        ServerEvent::NewDeviceLogin {
            user_name,
            device_name,
            client,
            ..
        } => (
            format!("Neue Anmeldung: {user_name}"),
            format!(
                "{user_name} hat sich auf einem neuen Gerät angemeldet: {device_name} ({client})."
            ),
        ),
        ServerEvent::UpdateAvailable { current, latest } => (
            format!("Flixnet {latest} ist verfügbar"),
            format!("Installiert ist {current}."),
        ),
        ServerEvent::JobUpdated { .. } | ServerEvent::LibraryChanged { .. } => return None,
    };
    Some(Notification {
        kind: kind.to_owned(),
        title,
        message,
        timestamp: flixnet_db::now_ms(),
        data: serde_json::to_value(event).unwrap_or_default(),
    })
}

// ---- Sending ------------------------------------------------------------------------

async fn check(response: reqwest::Response) -> anyhow::Result<()> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.text().await.unwrap_or_default();
    let body: String = body.chars().take(200).collect();
    anyhow::bail!("HTTP {status}: {body}")
}

/// Delivers one notification to one target.
pub async fn send(
    http: &reqwest::Client,
    db: &Db,
    target: &NotificationTarget,
    n: &Notification,
) -> anyhow::Result<()> {
    let config: StoredConfig = serde_json::from_str(&target.config_json)?;
    match TargetKind::parse(&target.kind) {
        Some(TargetKind::Webhook) => {
            let url = config.url.as_deref().unwrap_or_default();
            let body = serde_json::to_vec(&serde_json::json!({
                "event": n.kind,
                "timestamp": n.timestamp,
                "title": n.title,
                "message": n.message,
                "server": { "name": flixnet_core::NAME, "version": flixnet_core::VERSION },
                "data": n.data,
            }))?;
            let mut request = http
                .post(url)
                .header("Content-Type", "application/json")
                .header("X-Flixnet-Event", &n.kind);
            if let Some(secret) = &config.secret {
                let t = flixnet_db::now_ms() / 1000;
                let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
                    .expect("HMAC accepts any key length");
                mac.update(format!("{t}.").as_bytes());
                mac.update(&body);
                let signature = hex::encode(mac.finalize().into_bytes());
                request = request.header("X-Flixnet-Signature", format!("t={t},v1={signature}"));
            }
            check(request.body(body).send().await?).await
        }
        Some(TargetKind::Ntfy) => {
            let server = config.server_url.as_deref().unwrap_or("https://ntfy.sh");
            let mut request = http
                .post(server.trim_end_matches('/'))
                .json(&serde_json::json!({
                    "topic": config.topic,
                    "title": n.title,
                    "message": n.message,
                    "priority": config.priority.unwrap_or(3),
                    "tags": [n.kind.split('.').next().unwrap_or("flixnet")],
                }));
            if let Some(token) = &config.token {
                request = request.bearer_auth(token);
            }
            check(request.send().await?).await
        }
        Some(TargetKind::Discord) => {
            let url = config.webhook_url.as_deref().unwrap_or_default();
            let request = http.post(url).json(&serde_json::json!({
                "username": "Flixnet",
                "embeds": [{
                    "title": n.title,
                    "description": n.message,
                    "color": 0xe5_2e_4d,
                    "footer": { "text": n.kind },
                }],
            }));
            check(request.send().await?).await
        }
        Some(TargetKind::Email) => send_mail(db, &config.recipients, n).await,
        None => anyhow::bail!("unknown target kind {}", target.kind),
    }
}

async fn send_mail(db: &Db, recipients: &[String], n: &Notification) -> anyhow::Result<()> {
    let smtp: SmtpSettings = settings::get(db, SMTP_KEY).await?.unwrap_or_default();
    let host = smtp
        .host
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Kein SMTP-Server eingerichtet."))?;
    let from_address = smtp
        .from_address
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Keine Absenderadresse eingerichtet."))?;
    let from = Mailbox::new(
        Some(
            smtp.from_name
                .clone()
                .unwrap_or_else(|| "Flixnet".to_owned()),
        ),
        from_address.parse()?,
    );
    let mut message = lettre::Message::builder()
        .from(from)
        .subject(n.title.clone());
    for recipient in recipients {
        message = message.to(recipient.parse()?);
    }
    let email = message
        .header(ContentType::TEXT_PLAIN)
        .body(format!("{}\n\n— Flixnet", n.message))?;

    let security = smtp.security.unwrap_or_default();
    let builder = match security {
        SmtpSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(host)?,
        SmtpSecurity::Starttls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)?,
        SmtpSecurity::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host),
    };
    let port = smtp.port.unwrap_or(match security {
        SmtpSecurity::Tls => 465,
        SmtpSecurity::Starttls => 587,
        SmtpSecurity::None => 25,
    });
    let mut builder = builder.port(port).timeout(Some(Duration::from_secs(20)));
    if let Some(user) = &smtp.username {
        builder = builder.credentials(Credentials::new(
            user.clone(),
            smtp.password.clone().unwrap_or_default(),
        ));
    }
    builder.build().send(email).await?;
    Ok(())
}

// ---- Delivery jobs ------------------------------------------------------------------

/// Sends one queued notification; failures are retried with backoff.
pub struct DeliverNotification {
    pub http: reqwest::Client,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryPayload {
    pub target_id: i64,
    pub notification: Notification,
}

impl JobHandler for DeliverNotification {
    const KIND: &'static str = "notify.deliver";
    const POOL: Pool = Pool::Io;
    const MAX_ATTEMPTS: i64 = 5;
    type Payload = DeliveryPayload;

    async fn run(&self, ctx: JobContext, payload: DeliveryPayload) -> anyhow::Result<()> {
        let Some(target) = db_ops::get_target(ctx.db(), payload.target_id).await? else {
            return Ok(()); // removed meanwhile
        };
        if !target.enabled {
            return Ok(());
        }
        send(&self.http, ctx.db(), &target, &payload.notification).await
    }
}

/// Queues deliveries for every enabled target subscribed to the event.
pub async fn dispatch(db: &Db, queue: &JobQueue, event: &ServerEvent) -> anyhow::Result<usize> {
    let Some(kind) = event.notification_kind() else {
        return Ok(0);
    };
    let targets: Vec<NotificationTarget> = db_ops::list_targets(db)
        .await?
        .into_iter()
        .filter(|t| {
            t.enabled
                && serde_json::from_str::<Vec<String>>(&t.events_json)
                    .unwrap_or_default()
                    .iter()
                    .any(|e| e == kind)
        })
        .collect();
    if targets.is_empty() {
        return Ok(0);
    }
    let Some(notification) = render(db, event).await else {
        return Ok(0);
    };
    for target in &targets {
        queue
            .enqueue::<DeliverNotification>(
                &DeliveryPayload {
                    target_id: target.id,
                    notification: notification.clone(),
                },
                JobOptions::priority(priority::INTERACTIVE),
            )
            .await?;
    }
    Ok(targets.len())
}

/// Listens on the event bus and queues deliveries (needs a Tokio runtime).
pub(crate) fn spawn_listener(state: &AppState) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let mut events = state.events.subscribe();
    let db = state.db.clone();
    let queue = state.jobs.queue().clone();
    runtime.spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if let Err(err) = dispatch(&db, &queue, &event).await {
                        tracing::warn!(%err, "queueing notifications failed");
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "notification listener missed events");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_urls_are_masked() {
        assert_eq!(
            mask_discord("https://discord.com/api/webhooks/123/abcdef"),
            "https://discord.com/api/webhooks/123/••••••"
        );
    }

    #[test]
    fn clock_formats() {
        assert_eq!(clock(61_000), "1:01");
        assert_eq!(clock(3_725_000), "1:02:05");
    }
}
