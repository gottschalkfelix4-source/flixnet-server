//! Server information, branding, Quick Connect (off), display preferences.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use flixnet_api::error::{ApiError, ApiResult};
use serde_json::{Value, json};

use crate::JfState;
use crate::auth::JfUser;
use crate::dto::{PublicSystemInfo, SystemInfo};
use crate::util::{Jf, Params};

pub(crate) fn router() -> Router<JfState> {
    Router::new()
        .route("/system/info/public", get(public_info))
        .route("/system/info", get(info))
        .route("/system/ping", get(ping).post(ping))
        .route("/system/endpoint", get(endpoint))
        .route("/branding/configuration", get(branding))
        .route("/branding/css", get(branding_css))
        .route("/branding/css.css", get(branding_css))
        .route("/quickconnect/enabled", get(quick_connect))
        .route("/plugins", get(plugins))
        .route(
            "/displaypreferences/{id}",
            get(get_display_preferences).post(set_display_preferences),
        )
}

pub(crate) fn public_system_info(state: &JfState, headers: &HeaderMap) -> PublicSystemInfo {
    let local_address = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .filter(|h| {
            h.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '[' | ']'))
        })
        .map(|host| format!("http://{host}"));
    PublicSystemInfo {
        local_address,
        server_name: state.server_name.to_string(),
        version: crate::JELLYFIN_VERSION.to_owned(),
        // Clients (e.g. the Kotlin SDK) check this to recognise a Jellyfin server.
        product_name: "Jellyfin Server".to_owned(),
        operating_system: String::new(),
        id: state.server_id.to_string(),
        startup_wizard_completed: true,
    }
}

async fn public_info(State(state): State<JfState>, headers: HeaderMap) -> Jf<PublicSystemInfo> {
    Jf(public_system_info(&state, &headers))
}

async fn info(_: JfUser, State(state): State<JfState>, headers: HeaderMap) -> Jf<SystemInfo> {
    Jf(SystemInfo {
        public: public_system_info(&state, &headers),
        operating_system_display_name: String::new(),
        has_pending_restart: false,
        is_shutting_down: false,
        supports_library_monitor: true,
        web_socket_port_number: 0,
        completed_installations: Vec::new(),
        can_self_restart: false,
        can_launch_web_browser: false,
        has_update_available: false,
        cast_receiver_applications: Vec::new(),
    })
}

async fn ping() -> Json<&'static str> {
    Json("Jellyfin Server")
}

async fn endpoint(_: JfUser) -> Json<Value> {
    Json(json!({ "IsLocal": false, "IsInNetwork": true }))
}

async fn branding() -> Json<Value> {
    Json(json!({ "SplashscreenEnabled": false }))
}

async fn branding_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css")], "")
}

async fn quick_connect() -> Json<bool> {
    Json(false)
}

async fn plugins(_: JfUser) -> Json<Vec<Value>> {
    Json(Vec::new())
}

/// Ids and client names are short, plain identifiers (they become settings keys).
fn preference_key(user_id: i64, id: &str, client: &str) -> ApiResult<String> {
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 64
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
    };
    if ok(id) && ok(client) {
        Ok(format!("jellyfin.display_prefs.{user_id}.{client}.{id}"))
    } else {
        Err(ApiError::not_found("Ansicht nicht gefunden."))
    }
}

fn client_name(params: &Params) -> String {
    params.get("client").unwrap_or("emby").to_ascii_lowercase()
}

async fn get_display_preferences(
    jf: JfUser,
    State(state): State<JfState>,
    Path(id): Path<String>,
    params: Params,
) -> ApiResult<Jf<Value>> {
    let client = client_name(&params);
    let key = preference_key(jf.user.user_id, &id, &client)?;
    let stored: Option<Value> = flixnet_db::settings::get(&state.app.db, &key).await?;
    let mut prefs = json!({
        "Id": id,
        "SortBy": "SortName",
        "RememberIndexing": false,
        "PrimaryImageHeight": 250,
        "PrimaryImageWidth": 250,
        "CustomPrefs": {},
        "ScrollDirection": "Horizontal",
        "ShowBackdrop": true,
        "RememberSorting": false,
        "SortOrder": "Ascending",
        "ShowSidebar": false,
        "Client": client,
    });
    if let (Some(Value::Object(stored)), Some(prefs)) = (stored, prefs.as_object_mut()) {
        for (k, v) in stored {
            if k != "Id" {
                prefs.insert(k, v);
            }
        }
    }
    Ok(Jf(prefs))
}

/// Stores what the client sends (jellyfin-web keeps its home screen layout here).
async fn set_display_preferences(
    jf: JfUser,
    State(state): State<JfState>,
    Path(id): Path<String>,
    params: Params,
    Json(body): Json<Value>,
) -> ApiResult<StatusCode> {
    let client = client_name(&params);
    let key = preference_key(jf.user.user_id, &id, &client)?;
    let Value::Object(body) = body else {
        return Err(crate::util::bad_request("Objekt erwartet."));
    };
    if serde_json::to_string(&body).map_or(0, |s| s.len()) > 64 * 1024 {
        return Err(crate::util::bad_request("Zu groß."));
    }
    flixnet_db::settings::set(&state.app.db, &key, &Value::Object(body)).await?;
    Ok(StatusCode::NO_CONTENT)
}
