//! Single sign-on via OpenID Connect (Authelia, Authentik, Keycloak, …).
//!
//! Authorization code flow with PKCE:
//! 1. The login page asks `POST /auth/oidc/start` for the provider URL (state, nonce
//!    and PKCE verifier stay on the server) and sends the browser there.
//! 2. The provider redirects back to the SPA route `/login/oidc?code&state`, which
//!    hands both to `POST /auth/oidc/complete`.
//! 3. The server exchanges the code, verifies the ID token (signature via JWKS,
//!    issuer, audience, expiry, nonce), maps the account to a Flixnet user by `sub`
//!    (optionally by name, optionally creating it) and issues a device token like a
//!    password login.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use flixnet_core::events::ServerEvent;
use flixnet_db::{DbError, ops as db_ops, settings, users};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::jwt::{self, Jwks};
use crate::AppState;
use crate::auth::AdminUser;
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::session::{DeviceInfo, SessionDto, issue_session, validate_device};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_oidc_login_config))
        .routes(routes!(start_oidc_login))
        .routes(routes!(complete_oidc_login))
        .routes(routes!(get_oidc_settings, update_oidc_settings))
}

const SETTINGS_KEY: &str = "oidc";
/// A started sign-in must complete within this time.
const PENDING_TTL: Duration = Duration::from_secs(600);
const MAX_PENDING: usize = 1000;
/// Clock skew tolerated for `exp`/`iat`.
const LEEWAY_MS: i64 = 120_000;

/// Provider configuration (`settings` key `oidc`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct OidcSettings {
    enabled: bool,
    provider_name: String,
    issuer_url: String,
    client_id: String,
    client_secret: Option<String>,
    scopes: String,
    auto_create_users: bool,
    link_by_name: bool,
    admin_group: Option<String>,
    groups_claim: String,
}

impl Default for OidcSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider_name: "SSO".to_owned(),
            issuer_url: String::new(),
            client_id: String::new(),
            client_secret: None,
            scopes: "openid profile email".to_owned(),
            auto_create_users: false,
            link_by_name: false,
            admin_group: None,
            groups_claim: "groups".to_owned(),
        }
    }
}

impl OidcSettings {
    async fn load(db: &flixnet_db::Db) -> flixnet_db::Result<Self> {
        Ok(settings::get(db, SETTINGS_KEY).await?.unwrap_or_default())
    }

    fn usable(&self) -> bool {
        self.enabled && !self.issuer_url.is_empty() && !self.client_id.is_empty()
    }
}

struct Pending {
    verifier: String,
    nonce: String,
    redirect_uri: String,
    device_id: String,
    device_name: String,
    client: String,
    created: Instant,
}

/// Sign-ins in progress (in memory: a restart only cancels them).
#[derive(Clone, Default)]
pub struct OidcState {
    pending: Arc<Mutex<HashMap<String, Pending>>>,
}

#[derive(Debug, Clone, Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    #[serde(default)]
    userinfo_endpoint: Option<String>,
    /// Absent means `client_secret_basic` (OpenID Connect Discovery 1.0).
    #[serde(default)]
    token_endpoint_auth_methods_supported: Option<Vec<String>>,
}

/// How the client secret goes to the token endpoint. Exactly one method per request
/// (RFC 6749 §2.3: "MUST NOT use more than one").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientAuth {
    /// HTTP Basic with form-urlencoded id and secret (RFC 6749 §2.3.1).
    Basic,
    /// `client_id` and `client_secret` in the form body.
    Post,
}

/// `application/x-www-form-urlencoded` encoding of one value (RFC 6749 Appendix B),
/// required for the Basic credentials: providers decode them (Dex does), so a secret
/// with `+`, `/` or `=` would otherwise not match.
fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Basic unless the provider does not offer it. A secret that changes under form
/// encoding goes in the body where the provider allows that, since not every provider
/// decodes Basic credentials as the RFC asks.
fn client_auth(supported: Option<&[String]>, client_id: &str, secret: &str) -> ClientAuth {
    let offers = |method: &str| supported.is_none_or(|list| list.iter().any(|m| m == method));
    let offers_post = supported.is_some_and(|list| list.iter().any(|m| m == "client_secret_post"));
    let plain = form_encode(client_id) == client_id && form_encode(secret) == secret;
    if offers_post && (!offers("client_secret_basic") || !plain) {
        ClientAuth::Post
    } else {
        ClientAuth::Basic
    }
}

fn sso_error(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, "oidc_failed", message)
}

fn normalize_issuer(url: &str) -> String {
    url.trim().trim_end_matches('/').to_owned()
}

async fn discover(http: &reqwest::Client, issuer: &str) -> ApiResult<Discovery> {
    let url = format!(
        "{}/.well-known/openid-configuration",
        normalize_issuer(issuer)
    );
    let response = http.get(&url).send().await.map_err(|e| {
        sso_error(format!(
            "Der Anmeldedienst ist nicht erreichbar: {}",
            e.without_url()
        ))
    })?;
    if !response.status().is_success() {
        return Err(sso_error(format!(
            "Der Anmeldedienst antwortet mit HTTP {} auf die Discovery.",
            response.status()
        )));
    }
    let discovery: Discovery = response
        .json()
        .await
        .map_err(|_| sso_error("Die Discovery des Anmeldedienstes ist ungültig."))?;
    if normalize_issuer(&discovery.issuer) != normalize_issuer(issuer) {
        return Err(sso_error(
            "Der Anmeldedienst meldet einen anderen Issuer als eingestellt.",
        ));
    }
    Ok(discovery)
}

// ---- DTOs ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct OidcLoginConfigDto {
    /// Show the "Mit … anmelden" button.
    pub enabled: bool,
    /// Name on the button, e.g. "Authentik".
    pub provider_name: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct OidcStartRequest {
    /// Where the provider sends the browser back: the web UI's `/login/oidc` route
    /// (must be registered at the provider).
    pub redirect_uri: String,
    pub device: DeviceInfo,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OidcStartDto {
    /// Send the browser here.
    pub authorization_url: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct OidcCompleteRequest {
    pub code: String,
    pub state: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OidcSettingsDto {
    pub enabled: bool,
    pub provider_name: String,
    /// Issuer URL, e.g. `https://auth.example.com/application/o/flixnet/`.
    pub issuer_url: String,
    pub client_id: String,
    /// Whether a client secret is stored (it is never returned).
    pub has_client_secret: bool,
    pub scopes: String,
    /// Unknown accounts get a new Flixnet user.
    pub auto_create_users: bool,
    /// Unknown accounts are linked to the Flixnet user whose name equals their
    /// `preferred_username` (only enable if the provider controls these names).
    pub link_by_name: bool,
    /// Members of this group become administrators.
    pub admin_group: Option<String>,
    /// Claim holding the groups (default `groups`).
    pub groups_claim: String,
}

/// Provider settings. `client_secret` omitted or `null` keeps the stored one; `""`
/// removes it.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateOidcSettings {
    pub enabled: Option<bool>,
    pub provider_name: Option<String>,
    pub issuer_url: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub scopes: Option<String>,
    pub auto_create_users: Option<bool>,
    pub link_by_name: Option<bool>,
    /// Empty removes it.
    pub admin_group: Option<String>,
    pub groups_claim: Option<String>,
}

fn settings_dto(s: &OidcSettings) -> OidcSettingsDto {
    OidcSettingsDto {
        enabled: s.enabled,
        provider_name: s.provider_name.clone(),
        issuer_url: s.issuer_url.clone(),
        client_id: s.client_id.clone(),
        has_client_secret: s.client_secret.is_some(),
        scopes: s.scopes.clone(),
        auto_create_users: s.auto_create_users,
        link_by_name: s.link_by_name,
        admin_group: s.admin_group.clone(),
        groups_claim: s.groups_claim.clone(),
    }
}

// ---- Routes -------------------------------------------------------------------------

/// Whether single sign-on is offered on the login page (public).
#[utoipa::path(
    get,
    path = "/auth/oidc/config",
    operation_id = "getOidcLoginConfig",
    tag = "auth",
    security(()),
    responses((status = OK, description = "Login button configuration", body = OidcLoginConfigDto))
)]
async fn get_oidc_login_config(
    State(state): State<AppState>,
) -> ApiResult<Json<OidcLoginConfigDto>> {
    let s = OidcSettings::load(&state.db).await?;
    Ok(Json(OidcLoginConfigDto {
        enabled: s.usable(),
        provider_name: s.provider_name,
    }))
}

fn random() -> String {
    flixnet_core::auth::new_token().token
}

/// Start a single sign-on: returns the provider URL to send the browser to.
#[utoipa::path(
    post,
    path = "/auth/oidc/start",
    operation_id = "startOidcLogin",
    tag = "auth",
    security(()),
    request_body = OidcStartRequest,
    responses(
        (status = OK, description = "Provider URL", body = OidcStartDto),
        (status = NOT_FOUND, description = "Single sign-on is not enabled", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid redirect URI or device", body = ErrorBody),
        (status = BAD_GATEWAY, description = "Provider unreachable", body = ErrorBody),
    )
)]
async fn start_oidc_login(
    State(state): State<AppState>,
    Json(body): Json<OidcStartRequest>,
) -> ApiResult<Json<OidcStartDto>> {
    validate_device(&body.device)?;
    let s = OidcSettings::load(&state.db).await?;
    if !s.usable() {
        return Err(ApiError::not_found(
            "Single Sign-on ist nicht eingerichtet.",
        ));
    }
    let redirect = reqwest::Url::parse(&body.redirect_uri)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.path() == "/login/oidc")
        .ok_or_else(|| ApiError::validation("Ungültige Rücksprungadresse."))?;
    let discovery = discover(&state.ops.http, &s.issuer_url).await?;

    let verifier = random();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state_id = random();
    let nonce = random();
    let mut url = reqwest::Url::parse(&discovery.authorization_endpoint)
        .map_err(|_| sso_error("Die Anmeldeadresse des Dienstes ist ungültig."))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &s.client_id)
        .append_pair("redirect_uri", redirect.as_str())
        .append_pair("scope", &s.scopes)
        .append_pair("state", &state_id)
        .append_pair("nonce", &nonce)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");

    let mut pending = state.ops.oidc.pending.lock().expect("oidc lock");
    pending.retain(|_, p| p.created.elapsed() < PENDING_TTL);
    if pending.len() >= MAX_PENDING {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Zu viele Anmeldeversuche. Bitte gleich noch einmal.",
        ));
    }
    pending.insert(
        state_id,
        Pending {
            verifier,
            nonce,
            redirect_uri: redirect.to_string(),
            device_id: body.device.id.trim().to_owned(),
            device_name: body.device.name.trim().to_owned(),
            client: body.device.client.trim().to_owned(),
            created: Instant::now(),
        },
    );
    Ok(Json(OidcStartDto {
        authorization_url: url.to_string(),
    }))
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
    access_token: Option<String>,
}

fn claim_str(claims: &Map<String, Value>, key: &str) -> Option<String> {
    claims
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn groups(claims: &Map<String, Value>, claim: &str) -> Vec<String> {
    match claims.get(claim) {
        Some(Value::Array(list)) => list
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Some(Value::String(s)) => s
            .split([',', ' '])
            .filter(|g| !g.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// Checks issuer, audience, authorized party, expiry and nonce of ID token claims.
fn check_claims(
    claims: &Map<String, Value>,
    issuer: &str,
    client_id: &str,
    nonce: &str,
    now_ms: i64,
) -> Result<(), &'static str> {
    if claim_str(claims, "iss").map(|i| normalize_issuer(&i)) != Some(normalize_issuer(issuer)) {
        return Err("issuer");
    }
    let audiences: Vec<&str> = match claims.get("aud") {
        Some(Value::String(a)) => vec![a.as_str()],
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !audiences.contains(&client_id) {
        return Err("audience");
    }
    if audiences.len() > 1 && claim_str(claims, "azp").as_deref() != Some(client_id) {
        return Err("authorized party");
    }
    let exp = claims.get("exp").and_then(Value::as_i64).ok_or("expiry")?;
    if exp * 1000 + LEEWAY_MS < now_ms {
        return Err("expired");
    }
    if claims
        .get("iat")
        .and_then(Value::as_i64)
        .is_some_and(|iat| iat * 1000 > now_ms + LEEWAY_MS)
    {
        return Err("issued in the future");
    }
    if claim_str(claims, "nonce").as_deref() != Some(nonce) {
        return Err("nonce");
    }
    if claim_str(claims, "sub").is_none() {
        return Err("subject");
    }
    Ok(())
}

/// A free user name based on `wanted` ("felix", "felix 2", …).
async fn free_name(db: &flixnet_db::Db, wanted: &str) -> ApiResult<String> {
    let base: String = wanted
        .chars()
        .filter(|c| !c.is_control())
        .take(56)
        .collect::<String>()
        .trim()
        .to_owned();
    let base = if base.is_empty() {
        "SSO-Nutzer".to_owned()
    } else {
        base
    };
    for n in 1..100 {
        let name = if n == 1 {
            base.clone()
        } else {
            format!("{base} {n}")
        };
        if users::find_by_name(db, &name).await?.is_none() {
            return Ok(name);
        }
    }
    Err(ApiError::conflict(
        "name_taken",
        "Für dieses Konto ließ sich kein freier Name finden.",
    ))
}

/// Finish a single sign-on: exchanges the code and signs in like a password login.
#[utoipa::path(
    post,
    path = "/auth/oidc/complete",
    operation_id = "completeOidcLogin",
    tag = "auth",
    security(()),
    request_body = OidcCompleteRequest,
    responses(
        (status = OK, description = "Signed in", body = SessionDto),
        (status = UNAUTHORIZED, description = "Unknown or expired sign-in, or an invalid ID token", body = ErrorBody),
        (status = FORBIDDEN, description = "No Flixnet account for this login", body = ErrorBody),
        (status = BAD_GATEWAY, description = "Provider unreachable or failing", body = ErrorBody),
    )
)]
async fn complete_oidc_login(
    State(state): State<AppState>,
    Json(body): Json<OidcCompleteRequest>,
) -> ApiResult<Json<SessionDto>> {
    let pending = state
        .ops
        .oidc
        .pending
        .lock()
        .expect("oidc lock")
        .remove(&body.state)
        .filter(|p| p.created.elapsed() < PENDING_TTL)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "oidc_expired",
                "Die Anmeldung ist abgelaufen. Bitte noch einmal starten.",
            )
        })?;
    let s = OidcSettings::load(&state.db).await?;
    if !s.usable() {
        return Err(ApiError::not_found(
            "Single Sign-on ist nicht eingerichtet.",
        ));
    }
    let http = &state.ops.http;
    let discovery = discover(http, &s.issuer_url).await?;

    // Exchange the code (PKCE verifier + client credentials).
    let mut form = vec![
        ("grant_type", "authorization_code".to_owned()),
        ("code", body.code.clone()),
        ("redirect_uri", pending.redirect_uri.clone()),
        ("client_id", s.client_id.clone()),
        ("code_verifier", pending.verifier.clone()),
    ];
    let mut request = http.post(&discovery.token_endpoint);
    if let Some(secret) = &s.client_secret {
        match client_auth(
            discovery.token_endpoint_auth_methods_supported.as_deref(),
            &s.client_id,
            secret,
        ) {
            ClientAuth::Basic => {
                request = request.basic_auth(form_encode(&s.client_id), Some(form_encode(secret)));
            }
            ClientAuth::Post => form.push(("client_secret", secret.clone())),
        }
    }
    let response = request.form(&form).send().await.map_err(|e| {
        sso_error(format!(
            "Der Anmeldedienst ist nicht erreichbar: {}",
            e.without_url()
        ))
    })?;
    if !response.status().is_success() {
        let status = response.status();
        let text: String = response
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect();
        tracing::warn!(%status, body = %text, "OIDC token exchange failed");
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "oidc_rejected",
            "Der Anmeldedienst hat die Anmeldung abgelehnt.",
        ));
    }
    let tokens: TokenResponse = response
        .json()
        .await
        .map_err(|_| sso_error("Ungültige Antwort des Anmeldedienstes."))?;
    let id_token = tokens
        .id_token
        .ok_or_else(|| sso_error("Der Anmeldedienst hat kein ID-Token geliefert."))?;

    // Verify the ID token.
    let jwks: Jwks = http
        .get(&discovery.jwks_uri)
        .send()
        .await
        .map_err(|e| sso_error(format!("Schlüssel nicht abrufbar: {}", e.without_url())))?
        .json()
        .await
        .map_err(|_| sso_error("Ungültige Schlüssel des Anmeldedienstes."))?;
    let invalid = |why: String| {
        tracing::warn!(reason = %why, "OIDC ID token rejected");
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "oidc_invalid_token",
            "Das ID-Token des Anmeldedienstes ist ungültig.",
        )
    };
    let mut claims = jwt::verify(&id_token, &jwks.keys).map_err(|e| invalid(e.to_string()))?;
    check_claims(
        &claims,
        &discovery.issuer,
        &s.client_id,
        &pending.nonce,
        flixnet_db::now_ms(),
    )
    .map_err(|why| invalid(why.to_owned()))?;
    let subject = claim_str(&claims, "sub").expect("checked");

    // Groups or e-mail often only come from the userinfo endpoint.
    if let (Some(endpoint), Some(access)) = (&discovery.userinfo_endpoint, &tokens.access_token)
        && let Ok(response) = http.get(endpoint).bearer_auth(access).send().await
        && response.status().is_success()
        && let Ok(info) = response.json::<Map<String, Value>>().await
        && claim_str(&info, "sub").as_deref() == Some(subject.as_str())
    {
        for (key, value) in info {
            claims.entry(key).or_insert(value);
        }
    }

    let email = claim_str(&claims, "email")
        .filter(|_| claims.get("email_verified").and_then(Value::as_bool) != Some(false));
    let preferred = claim_str(&claims, "preferred_username");
    let in_admin_group = s
        .admin_group
        .as_deref()
        .is_some_and(|g| groups(&claims, &s.groups_claim).iter().any(|m| m == g));
    let issuer = normalize_issuer(&discovery.issuer);

    // Map to a Flixnet user.
    let mut user_id = db_ops::oidc_user(&state.db, &issuer, &subject).await?;
    if user_id.is_none()
        && let Some(email) = &email
        && let Some(id) = db_ops::oidc_user_by_email(&state.db, email).await?
    {
        user_id = Some(id);
    }
    if user_id.is_none() && s.link_by_name {
        for name in [preferred.as_deref(), email.as_deref()]
            .into_iter()
            .flatten()
        {
            if let Some(user) = users::find_by_name(&state.db, name).await? {
                user_id = Some(user.id);
                break;
            }
        }
    }
    let mut created = false;
    if user_id.is_none() && s.auto_create_users {
        let wanted = preferred
            .clone()
            .or_else(|| claim_str(&claims, "name"))
            .or_else(|| {
                email
                    .as_deref()
                    .and_then(|e| e.split('@').next())
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        let name = free_name(&state.db, &wanted).await?;
        let user = users::create(
            &state.db,
            &users::NewUser {
                name: &name,
                password_hash: None,
                pin_hash: None,
                is_admin: in_admin_group,
                is_hidden: false,
                max_age_rating: None,
            },
        )
        .await?;
        tracing::info!(user = %user.name, "user created from single sign-on");
        state.events.publish(ServerEvent::UserCreated {
            user_id: user.id,
            name: user.name.clone(),
        });
        user_id = Some(user.id);
        created = true;
    }
    let Some(user_id) = user_id else {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "oidc_no_account",
            "Für diese Anmeldung gibt es kein Flixnet-Konto. Bitte einen Administrator fragen.",
        ));
    };
    match db_ops::link_oidc(&state.db, user_id, &issuer, &subject, email.as_deref()).await {
        Ok(()) | Err(DbError::Conflict) => {}
        Err(err) => return Err(err.into()),
    }
    let mut user = users::get(&state.db, user_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Nutzer nicht gefunden."))?;
    if in_admin_group && !user.is_admin {
        user = users::update(
            &state.db,
            user.id,
            &users::UserPatch {
                is_admin: Some(true),
                ..Default::default()
            },
        )
        .await?
        .unwrap_or(user);
    }

    let device = DeviceInfo {
        id: pending.device_id,
        name: pending.device_name,
        client: pending.client,
    };
    let known = db_ops::device_known(&state.db, user.id, &device.id).await?;
    let (user_name, user_id) = (user.name.clone(), user.id);
    let session = issue_session(&state, user, &device).await?;
    if !known && !created {
        state.events.publish(ServerEvent::NewDeviceLogin {
            user_id,
            user_name,
            device_name: device.name,
            client: device.client,
        });
    }
    Ok(Json(session))
}

/// Single sign-on provider settings (administrators).
#[utoipa::path(
    get,
    path = "/settings/oidc",
    operation_id = "getOidcSettings",
    tag = "settings",
    responses(
        (status = OK, description = "Current settings", body = OidcSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn get_oidc_settings(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<OidcSettingsDto>> {
    Ok(Json(settings_dto(&OidcSettings::load(&state.db).await?)))
}

/// Change the single sign-on provider. Enabling checks the provider's discovery.
#[utoipa::path(
    put,
    path = "/settings/oidc",
    operation_id = "updateOidcSettings",
    tag = "settings",
    request_body = UpdateOidcSettings,
    responses(
        (status = OK, description = "Updated settings", body = OidcSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid settings or provider not reachable", body = ErrorBody),
    )
)]
async fn update_oidc_settings(
    _: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<UpdateOidcSettings>,
) -> ApiResult<Json<OidcSettingsDto>> {
    let mut s = OidcSettings::load(&state.db).await?;
    let trimmed = |v: String| v.trim().to_owned();
    if let Some(v) = body.provider_name {
        let v = trimmed(v);
        if v.is_empty() || v.chars().count() > 40 {
            return Err(ApiError::validation(
                "Der Anzeigename muss zwischen 1 und 40 Zeichen lang sein.",
            ));
        }
        s.provider_name = v;
    }
    if let Some(v) = body.issuer_url {
        let v = trimmed(v);
        if !v.is_empty()
            && !reqwest::Url::parse(&v).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
        {
            return Err(ApiError::validation(
                "Die Issuer-Adresse muss eine http(s)-Adresse sein.",
            ));
        }
        s.issuer_url = v;
    }
    if let Some(v) = body.client_id {
        s.client_id = trimmed(v);
    }
    match body.client_secret {
        Some(v) if v.trim().is_empty() => s.client_secret = None,
        Some(v) => s.client_secret = Some(trimmed(v)),
        None => {}
    }
    if let Some(v) = body.scopes {
        let v = trimmed(v);
        s.scopes = if v.split_whitespace().any(|scope| scope == "openid") {
            v
        } else {
            format!("openid {v}").trim().to_owned()
        };
    }
    if let Some(v) = body.auto_create_users {
        s.auto_create_users = v;
    }
    if let Some(v) = body.link_by_name {
        s.link_by_name = v;
    }
    if let Some(v) = body.admin_group {
        s.admin_group = Some(trimmed(v)).filter(|g| !g.is_empty());
    }
    if let Some(v) = body.groups_claim {
        let v = trimmed(v);
        s.groups_claim = if v.is_empty() { "groups".to_owned() } else { v };
    }
    if let Some(v) = body.enabled {
        s.enabled = v;
    }
    if s.enabled {
        if s.issuer_url.is_empty() || s.client_id.is_empty() {
            return Err(ApiError::validation(
                "Zum Einschalten werden Issuer-Adresse und Client-ID gebraucht.",
            ));
        }
        discover(&state.ops.http, &s.issuer_url)
            .await
            .map_err(|_| {
                ApiError::validation(
                    "Der Anmeldedienst ist unter dieser Issuer-Adresse nicht erreichbar.",
                )
            })?;
    }
    settings::set(&state.db, SETTINGS_KEY, &s).await?;
    Ok(Json(settings_dto(&s)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn claims(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn claims_are_checked() {
        let now = 1_700_000_000_000;
        let good = json!({
            "iss": "https://auth.example/", "aud": "flixnet", "exp": 1_700_000_300,
            "iat": 1_700_000_000, "nonce": "n", "sub": "u1"
        });
        assert_eq!(
            check_claims(
                &claims(good.clone()),
                "https://auth.example",
                "flixnet",
                "n",
                now
            ),
            Ok(())
        );
        let mut c = claims(good.clone());
        c.insert("aud".into(), json!(["other"]));
        assert_eq!(
            check_claims(&c, "https://auth.example", "flixnet", "n", now),
            Err("audience")
        );
        let mut c = claims(good.clone());
        c.insert("aud".into(), json!(["flixnet", "other"]));
        assert_eq!(
            check_claims(&c, "https://auth.example", "flixnet", "n", now),
            Err("authorized party")
        );
        c.insert("azp".into(), json!("flixnet"));
        assert_eq!(
            check_claims(&c, "https://auth.example", "flixnet", "n", now),
            Ok(())
        );
        assert_eq!(
            check_claims(&claims(good.clone()), "https://evil", "flixnet", "n", now),
            Err("issuer")
        );
        assert_eq!(
            check_claims(
                &claims(good.clone()),
                "https://auth.example",
                "flixnet",
                "x",
                now
            ),
            Err("nonce")
        );
        assert_eq!(
            check_claims(
                &claims(good),
                "https://auth.example",
                "flixnet",
                "n",
                now + 3_600_000
            ),
            Err("expired")
        );
    }

    /// Observed against Dex v2.46: it url-decodes Basic credentials and ignores a
    /// body secret when a Basic header is present, so the raw secret `a+b/c=` sent as
    /// Basic (and in the body, as before) was rejected with `invalid_client`.
    #[test]
    fn client_secret_is_sent_one_way_and_encoded() {
        assert_eq!(form_encode("fx+W0X/x= ü"), "fx%2BW0X%2Fx%3D+%C3%BC");
        assert_eq!(form_encode("Abc-1.2_3*"), "Abc-1.2_3*");
        let both = [
            "client_secret_basic".to_owned(),
            "client_secret_post".to_owned(),
        ];
        let basic_only = ["client_secret_basic".to_owned()];
        let post_only = ["client_secret_post".to_owned()];
        // Default (no discovery entry) and plain secrets: Basic.
        assert_eq!(client_auth(None, "flixnet", "geheim"), ClientAuth::Basic);
        assert_eq!(client_auth(None, "flixnet", "a+b/c="), ClientAuth::Basic);
        assert_eq!(
            client_auth(Some(&both), "flixnet", "geheim"),
            ClientAuth::Basic
        );
        // Secrets that change under encoding go in the body where allowed.
        assert_eq!(
            client_auth(Some(&both), "flixnet", "a+b/c="),
            ClientAuth::Post
        );
        assert_eq!(
            client_auth(Some(&basic_only), "flixnet", "a+b/c="),
            ClientAuth::Basic
        );
        assert_eq!(
            client_auth(Some(&post_only), "flixnet", "geheim"),
            ClientAuth::Post
        );
    }

    #[test]
    fn groups_from_arrays_and_strings() {
        let c = claims(json!({ "groups": ["a", "admins"], "roles": "x y" }));
        assert_eq!(groups(&c, "groups"), ["a", "admins"]);
        assert_eq!(groups(&c, "roles"), ["x", "y"]);
        assert!(groups(&c, "missing").is_empty());
    }
}
