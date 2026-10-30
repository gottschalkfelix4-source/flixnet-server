mod common;

use axum::http::{Method, StatusCode};
use common::{PASSWORD, app, credentials};
use serde_json::json;

#[tokio::test]
async fn setup_creates_admin_exactly_once() {
    let app = app().await;

    let status = app.get("/api/v1/setup/status", None).await;
    assert_eq!(status.status, StatusCode::OK);
    assert_eq!(status.json["setup_required"], true);

    let token = app.setup_admin().await;
    assert_eq!(token.len(), 64);

    let status = app.get("/api/v1/setup/status", None).await;
    assert_eq!(status.json["setup_required"], false);

    let again = app
        .post(
            "/api/v1/setup",
            credentials("Mallory", PASSWORD, "evil"),
            None,
        )
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.json["code"], "setup_completed");

    let me = app.get("/api/v1/users/me", Some(&token)).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.json["name"], "Felix");
    assert_eq!(me.json["is_admin"], true);
}

#[tokio::test]
async fn setup_validates_input() {
    let app = app().await;
    let short = app
        .post("/api/v1/setup", credentials("Felix", "kurz", "d"), None)
        .await;
    assert_eq!(short.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(short.json["code"], "validation");

    let blank = app
        .post("/api/v1/setup", credentials("   ", PASSWORD, "d"), None)
        .await;
    assert_eq!(blank.status, StatusCode::UNPROCESSABLE_ENTITY);

    let no_device = app
        .post("/api/v1/setup", credentials("Felix", PASSWORD, " "), None)
        .await;
    assert_eq!(no_device.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Nothing was created by the rejected attempts.
    let status = app.get("/api/v1/setup/status", None).await;
    assert_eq!(status.json["setup_required"], true);
}

#[tokio::test]
async fn login_logout_and_token_transport() {
    let app = app().await;
    app.setup_admin().await;

    let wrong = app
        .post(
            "/api/v1/auth/login",
            credentials("Felix", "falsches-passwort", "tv"),
            None,
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.json["code"], "invalid_credentials");

    let unknown = app
        .post(
            "/api/v1/auth/login",
            credentials("Niemand", PASSWORD, "tv"),
            None,
        )
        .await;
    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        unknown.json["code"], "invalid_credentials",
        "no user enumeration"
    );

    // Case-insensitive name, surrounding whitespace ignored.
    let login = app
        .post(
            "/api/v1/auth/login",
            credentials("  fElIx ", PASSWORD, "tv"),
            None,
        )
        .await;
    assert_eq!(login.status, StatusCode::OK, "{}", login.json);
    let token = login.json["token"].as_str().unwrap().to_owned();
    assert_eq!(login.json["user"]["name"], "Felix");

    // Token via query parameter (for media URLs).
    let via_query = app
        .get(&format!("/api/v1/users/me?api_key={token}"), None)
        .await;
    assert_eq!(via_query.status, StatusCode::OK);

    let logout = app
        .request(Method::POST, "/api/v1/auth/logout", None, Some(&token))
        .await;
    assert_eq!(logout.status, StatusCode::NO_CONTENT);

    let after = app.get("/api/v1/users/me", Some(&token)).await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);
    assert_eq!(after.json["code"], "unauthorized");
}

#[tokio::test]
async fn relogin_on_the_same_device_invalidates_the_previous_token() {
    let app = app().await;
    app.setup_admin().await;

    let first = app
        .post(
            "/api/v1/auth/login",
            credentials("Felix", PASSWORD, "phone"),
            None,
        )
        .await;
    let first_token = first.json["token"].as_str().unwrap().to_owned();
    let second = app
        .post(
            "/api/v1/auth/login",
            credentials("Felix", PASSWORD, "phone"),
            None,
        )
        .await;
    let second_token = second.json["token"].as_str().unwrap().to_owned();

    assert_eq!(
        app.get("/api/v1/users/me", Some(&first_token)).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.get("/api/v1/users/me", Some(&second_token))
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn garbage_tokens_are_rejected() {
    let app = app().await;
    app.setup_admin().await;
    for token in ["", "x", "Bearer", &"0".repeat(64)] {
        let response = app.get("/api/v1/users/me", Some(token)).await;
        assert_eq!(response.status, StatusCode::UNAUTHORIZED, "token {token:?}");
    }
    let empty_query = app.get("/api/v1/users/me?api_key=", None).await;
    assert_eq!(empty_query.status, StatusCode::UNAUTHORIZED);
}

/// Milestone 1.1 gate: every operation in the OpenAPI spec answers 401 without a
/// token, except the ones explicitly documented as public (`security: [{}]`).
#[tokio::test]
async fn every_protected_operation_requires_a_token() {
    let app = app().await;
    app.setup_admin().await;

    let spec = serde_json::to_value(flixnet_api::openapi()).unwrap();
    let paths = spec["paths"].as_object().unwrap();
    let mut public = Vec::new();
    let mut checked = 0;

    for (path, item) in paths {
        for (method, operation) in item.as_object().unwrap() {
            let Ok(method) = method.to_uppercase().parse::<Method>() else {
                continue;
            };
            let is_public = operation["security"] == json!([{}]);
            if is_public {
                public.push(format!("{method} {path}"));
                continue;
            }
            // Fill path parameters with a plausible id; auth must fail before lookup.
            let uri = path
                .split('/')
                .map(|seg| if seg.starts_with('{') { "1" } else { seg })
                .collect::<Vec<_>>()
                .join("/");
            let body = (method == Method::POST || method == Method::PUT || method == Method::PATCH)
                .then(|| json!({}));
            let response = app.request(method.clone(), &uri, body, None).await;
            assert_eq!(
                response.status,
                StatusCode::UNAUTHORIZED,
                "{method} {path} must require authentication"
            );
            checked += 1;
        }
    }

    public.sort();
    assert_eq!(
        public,
        [
            // Single sign-on: whether the login button shows, and the sign-in itself
            // (state + PKCE verifier stay on the server; oidc_api.rs).
            "GET /api/v1/auth/oidc/config",
            // Signed URL, or a token of a user who may see the collection (collections_api.rs).
            "GET /api/v1/collections/{id}/images/{kind}",
            // Signed URLs bound to user + file for seek-bar previews and chapter
            // images, or a token (trickplay_api.rs).
            "GET /api/v1/files/{file_id}/chapters/{chapter}",
            "GET /api/v1/files/{file_id}/trickplay/{sheet}",
            "GET /api/v1/images/{id}",
            "GET /api/v1/invitations/redeem/{code}",
            // Signed URL for <img> tags, or a token; channel logos are not
            // sensitive (dvr_api.rs).
            "GET /api/v1/livetv/channels/{id}/logo",
            // The 128-bit live viewer session id is the capability (livetv_api.rs).
            "GET /api/v1/livetv/sessions/{session}/live.m3u8",
            "GET /api/v1/livetv/sessions/{session}/{segment}",
            // Signed URL bound to user + file (AAC conversion of a track for <audio>),
            // or a token (music_api.rs).
            "GET /api/v1/music/stream/{file_id}",
            "GET /api/v1/setup/status",
            // Signed URL bound to user + file, or a token (playback_api.rs).
            "GET /api/v1/stream/{file_id}",
            // Signed URL bound to user + file for <track> elements, or a token
            // (subtitles_api.rs).
            "GET /api/v1/subtitles/{file_id}/{track}",
            "GET /api/v1/system/info",
            // The 128-bit session id is the capability (playback_api.rs).
            "GET /api/v1/transcode/{session}/init.mp4",
            "GET /api/v1/transcode/{session}/main.m3u8",
            "GET /api/v1/transcode/{session}/{segment}",
            "POST /api/v1/auth/login",
            "POST /api/v1/auth/oidc/complete",
            "POST /api/v1/auth/oidc/start",
            "POST /api/v1/invitations/redeem/{code}",
            "POST /api/v1/setup",
        ],
        "the set of public endpoints changed; review this deliberately"
    );
    assert!(checked >= 2, "expected protected operations to check");
}
