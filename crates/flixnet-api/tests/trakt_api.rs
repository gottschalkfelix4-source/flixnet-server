//! Trakt.tv against a mocked API: device code connection, scrobbling from playback
//! reports, two-way sync of watched state and ratings, token refresh, disconnect.

mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::{TestApp, app_with};
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::{libraries, userdata};
use flixnet_library::scan::scan_library;
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

struct Fixture {
    admin: String,
    admin_id: i64,
    matrix: i64,
    alpha: i64,
    dark_e1: i64,
    dark_e2: i64,
}

async fn library(app: &TestApp) -> Fixture {
    let root = app.dir.path().join("media");
    for file in [
        "Filme/The Matrix (1999) {tmdb-603}/The Matrix (1999).mkv",
        "Filme/Alpha (2020)/Alpha.2020.mkv",
        "Serien/Dark {tvdb-334824}/Staffel 1/Dark.S01E01.mkv",
        "Serien/Dark {tvdb-334824}/Staffel 1/Dark.S01E02.mkv",
    ] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"v").unwrap();
    }
    let queue = JobRuntime::new(app.db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    for (name, kind) in [("Filme", "movies"), ("Serien", "shows")] {
        let path = root.join(name).to_string_lossy().into_owned();
        let id = libraries::create(&app.db, name, kind, &[path])
            .await
            .unwrap()
            .id;
        scan_library(&app.db, &queue, id).await.unwrap();
    }
    let admin = app.setup_admin().await;
    let find = |list: &Value, title: &str| {
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["title"].as_str().unwrap().contains(title))
            .unwrap_or_else(|| panic!("{title} in {list}"))["id"]
            .as_i64()
            .unwrap()
    };
    let movies = app.get("/api/v1/items?kind=movie", Some(&admin)).await.json;
    let series = app
        .get("/api/v1/items?kind=series", Some(&admin))
        .await
        .json;
    let dark = find(&series, "Dark");
    let episodes = app
        .get(
            &format!("/api/v1/items?series_id={dark}&kind=episode"),
            Some(&admin),
        )
        .await
        .json;
    let me = app.get("/api/v1/users/me", Some(&admin)).await.json;
    Fixture {
        admin_id: me["id"].as_i64().unwrap(),
        matrix: find(&movies, "Matrix"),
        alpha: find(&movies, "Alpha"),
        dark_e1: episodes["items"][0]["id"].as_i64().unwrap(),
        dark_e2: episodes["items"][1]["id"].as_i64().unwrap(),
        admin,
    }
}

async fn requests_to(server: &MockServer, wanted: &str) -> Vec<Request> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.url.path() == wanted)
        .collect()
}

/// Waits until `count` requests went to `wanted`.
async fn wait_for(server: &MockServer, wanted: &str, count: usize) -> Vec<Request> {
    for _ in 0..200 {
        let found = requests_to(server, wanted).await;
        if found.len() >= count {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("expected {count} request(s) to {wanted}");
}

fn report(item: i64, position_ms: i64, paused: bool) -> Value {
    json!({
        "item_id": item, "position_ms": position_ms, "duration_ms": 6_000_000,
        "play_method": "direct_play", "is_paused": paused
    })
}

async fn mount(server: &MockServer, verb: &str, route: &str, status: u16, body: Value) {
    Mock::given(method(verb))
        .and(path(route))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

async fn connect(app: &TestApp, server: &MockServer, token: &str) {
    let settings = app
        .request(
            Method::PUT,
            "/api/v1/settings/trakt",
            Some(json!({ "client_id": "cid", "client_secret": "csec" })),
            Some(token),
        )
        .await;
    assert_eq!(settings.status, StatusCode::OK, "{}", settings.json);
    assert_eq!(settings.json["has_client_secret"], true);
    assert!(!settings.json.to_string().contains("csec"));

    mount(
        server,
        "POST",
        "/oauth/device/code",
        200,
        json!({
            "device_code": "dev-1", "user_code": "ABCD1234",
            "verification_url": "https://trakt.tv/activate", "expires_in": 600, "interval": 5
        }),
    )
    .await;
    // Not confirmed on the first poll, confirmed on the second.
    Mock::given(method("POST"))
        .and(path("/oauth/device/token"))
        .respond_with(ResponseTemplate::new(400))
        .up_to_n_times(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/device/token"))
        .and(body_partial_json(json!({ "code": "dev-1", "client_id": "cid", "client_secret": "csec" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access-1", "token_type": "bearer", "expires_in": 7_776_000,
            "refresh_token": "refresh-1", "scope": "public", "created_at": flixnet_db::now_ms() / 1000
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/settings"))
        .and(header("authorization", "Bearer access-1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "user": { "username": "felix" } })),
        )
        .mount(server)
        .await;

    let status = app.get("/api/v1/trakt", Some(token)).await;
    assert_eq!(status.json["configured"], true);
    assert_eq!(status.json["connected"], false);

    let code = app
        .request(Method::POST, "/api/v1/trakt/connect", None, Some(token))
        .await;
    assert_eq!(code.status, StatusCode::OK, "{}", code.json);
    assert_eq!(code.json["user_code"], "ABCD1234");
    assert!(
        code.json.get("device_code").is_none(),
        "the device code stays on the server"
    );

    let poll = app
        .request(
            Method::POST,
            "/api/v1/trakt/connect/poll",
            None,
            Some(token),
        )
        .await;
    assert_eq!(poll.json["state"], "pending", "{}", poll.json);
    assert_eq!(poll.json["status"]["pending"]["user_code"], "ABCD1234");
    let poll = app
        .request(
            Method::POST,
            "/api/v1/trakt/connect/poll",
            None,
            Some(token),
        )
        .await;
    assert_eq!(poll.json["state"], "connected", "{}", poll.json);
    assert_eq!(poll.json["status"]["connected"], true);
    assert_eq!(poll.json["status"]["username"], "felix");
    assert_eq!(poll.json["status"]["scrobble"], true);
    assert!(!poll.json.to_string().contains("access-1"));
}

#[tokio::test]
async fn connect_scrobble_sync_and_disconnect() {
    let server = MockServer::start().await;
    let uri = server.uri();
    let app = app_with(|c| c.integrations.trakt_api_url = uri).await;
    let f = library(&app).await;
    let token = f.admin.clone();

    // Not configured yet.
    let early = app
        .request(Method::POST, "/api/v1/trakt/connect", None, Some(&token))
        .await;
    assert_eq!(early.status, StatusCode::CONFLICT);
    assert_eq!(early.json["code"], "trakt_not_configured");

    connect(&app, &server, &token).await;

    // ---- Scrobbling ----
    for action in ["start", "pause", "stop"] {
        mount(
            &server,
            "POST",
            &format!("/scrobble/{action}"),
            201,
            json!({ "id": 1 }),
        )
        .await;
    }
    let started = app
        .post(
            "/api/v1/playback/sessions",
            report(f.matrix, 600_000, false),
            Some(&token),
        )
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.json);
    let session = started.json["id"].as_str().unwrap().to_owned();
    let start = wait_for(&server, "/scrobble/start", 1).await;
    let body: Value = serde_json::from_slice(&start[0].body).unwrap();
    assert_eq!(body["movie"]["ids"]["tmdb"], 603);
    assert_eq!(body["progress"], 10.0);
    assert_eq!(start[0].headers.get("trakt-api-key").unwrap(), "cid");
    assert_eq!(
        start[0].headers.get("authorization").unwrap(),
        "Bearer access-1"
    );

    let paused = app
        .post(
            &format!("/api/v1/playback/sessions/{session}/progress"),
            report(f.matrix, 1_200_000, true),
            Some(&token),
        )
        .await;
    assert_eq!(paused.status, StatusCode::NO_CONTENT);
    let pause = wait_for(&server, "/scrobble/pause", 1).await;
    let body: Value = serde_json::from_slice(&pause[0].body).unwrap();
    assert_eq!(body["progress"], 20.0);

    let stopped = app
        .post(
            &format!("/api/v1/playback/sessions/{session}/stop"),
            report(f.matrix, 5_700_000, false),
            Some(&token),
        )
        .await;
    assert_eq!(stopped.json["played"], true);
    let stop = wait_for(&server, "/scrobble/stop", 1).await;
    let body: Value = serde_json::from_slice(&stop[0].body).unwrap();
    assert_eq!(body["progress"], 95.0);

    // Episodes are scrobbled by show ids plus season/episode number.
    let started = app
        .post(
            "/api/v1/playback/sessions",
            report(f.dark_e1, 0, false),
            Some(&token),
        )
        .await;
    let session = started.json["id"].as_str().unwrap().to_owned();
    let start = wait_for(&server, "/scrobble/start", 2).await;
    let body: Value = serde_json::from_slice(&start[1].body).unwrap();
    assert_eq!(body["show"]["ids"]["tvdb"], 334824);
    assert_eq!(body["episode"], json!({ "season": 1, "number": 1 }));
    app.post(
        &format!("/api/v1/playback/sessions/{session}/stop"),
        report(f.dark_e1, 5_900_000, false),
        Some(&token),
    )
    .await;
    wait_for(&server, "/scrobble/stop", 2).await;

    // Scrobbling off: nothing goes out.
    let off = app
        .request(
            Method::PUT,
            "/api/v1/trakt",
            Some(json!({ "scrobble": false })),
            Some(&token),
        )
        .await;
    assert_eq!(off.json["scrobble"], false);
    app.post(
        "/api/v1/playback/sessions",
        report(f.alpha, 0, false),
        Some(&token),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(requests_to(&server, "/scrobble/start").await.len(), 2);

    // ---- Two-way sync ----
    mount(
        &server,
        "GET",
        "/sync/watched/movies",
        200,
        json!([
            { "plays": 2, "last_watched_at": "2024-01-15T20:31:00.000Z",
              "movie": { "title": "Alpha", "year": 2020, "ids": { "trakt": 1, "slug": "alpha-2020", "tmdb": 999001 } } },
            { "plays": 1, "last_watched_at": "2023-05-01T10:00:00.000Z",
              "movie": { "title": "Gibt es nicht", "year": 1990, "ids": { "trakt": 2, "tmdb": 12345 } } }
        ]),
    )
    .await;
    mount(
        &server,
        "GET",
        "/sync/watched/shows",
        200,
        json!([{
            "plays": 1, "last_watched_at": "2024-02-01T21:00:00.000Z",
            "show": { "title": "Dark", "year": 2017, "ids": { "trakt": 3, "tvdb": 334824, "imdb": "tt5753856" } },
            "seasons": [{ "number": 1, "episodes": [{ "number": 2, "plays": 1, "last_watched_at": "2024-02-01T21:00:00.000Z" }] }]
        }]),
    )
    .await;
    mount(
        &server,
        "GET",
        "/sync/ratings/movies",
        200,
        json!([{ "rated_at": "2024-01-01T00:00:00.000Z", "rating": 9, "type": "movie",
                 "movie": { "title": "The Matrix", "year": 1999, "ids": { "tmdb": 603, "imdb": "tt0133093" } } }]),
    )
    .await;
    mount(&server, "GET", "/sync/ratings/episodes", 200, json!([])).await;
    mount(
        &server,
        "POST",
        "/sync/history",
        201,
        json!({ "added": { "movies": 1, "episodes": 1 } }),
    )
    .await;
    mount(
        &server,
        "POST",
        "/sync/ratings",
        201,
        json!({ "added": { "movies": 1 } }),
    )
    .await;

    // A local rating that differs from Trakt's: Flixnet's wins and is sent.
    let matrix_identity = userdata::identity_key(&app.db, f.matrix)
        .await
        .unwrap()
        .unwrap();
    flixnet_db::history::set_rating(&app.db, f.admin_id, &matrix_identity, Some(8.0))
        .await
        .unwrap();

    let sync = app
        .request(Method::POST, "/api/v1/trakt/sync", None, Some(&token))
        .await;
    assert_eq!(sync.status, StatusCode::OK, "{}", sync.json);
    assert_eq!(
        sync.json,
        json!({ "pulled_watched": 2, "pulled_ratings": 0, "pushed_watched": 2, "pushed_ratings": 1, "unmatched": 1 })
    );

    let alpha = userdata::get(&app.db, f.admin_id, f.alpha).await.unwrap();
    assert!(alpha.played);
    assert_eq!(alpha.play_count, 2);
    assert_eq!(alpha.last_played_at, Some(1_705_350_660_000));
    assert!(
        userdata::get(&app.db, f.admin_id, f.dark_e2)
            .await
            .unwrap()
            .played
    );
    assert_eq!(
        userdata::get(&app.db, f.admin_id, f.matrix)
            .await
            .unwrap()
            .rating,
        Some(8.0)
    );

    let pushed = requests_to(&server, "/sync/history").await;
    let body: Value = serde_json::from_slice(&pushed[0].body).unwrap();
    assert_eq!(body["movies"].as_array().unwrap().len(), 1);
    assert_eq!(body["movies"][0]["ids"]["tmdb"], 603);
    assert!(
        body["movies"][0]["watched_at"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    assert_eq!(body["shows"][0]["ids"]["tvdb"], 334824);
    assert_eq!(body["shows"][0]["seasons"][0]["number"], 1);
    assert_eq!(body["shows"][0]["seasons"][0]["episodes"][0]["number"], 1);
    let ratings = requests_to(&server, "/sync/ratings").await;
    let body: Value = serde_json::from_slice(&ratings[0].body).unwrap();
    assert_eq!(body["movies"][0]["rating"], 8);

    let status = app.get("/api/v1/trakt", Some(&token)).await;
    assert!(status.json["last_sync_at"].is_i64());
    assert_eq!(status.json["last_sync"]["pushed_watched"], 2);

    // A second sync pushes nothing new and adds no duplicate history.
    let events_before = flixnet_db::playstate::events(&app.db, f.admin_id)
        .await
        .unwrap()
        .len();
    let again = app
        .request(Method::POST, "/api/v1/trakt/sync", None, Some(&token))
        .await;
    assert_eq!(again.json["pulled_watched"], 0, "{}", again.json);
    assert_eq!(
        flixnet_db::playstate::events(&app.db, f.admin_id)
            .await
            .unwrap()
            .len(),
        events_before
    );

    // ---- Token refresh ----
    flixnet_db::ops::save_trakt_tokens(
        &app.db,
        f.admin_id,
        "access-1",
        "refresh-1",
        flixnet_db::now_ms() + 1000,
        None,
    )
    .await
    .unwrap();
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_partial_json(
            json!({ "refresh_token": "refresh-1", "grant_type": "refresh_token" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access-2", "token_type": "bearer", "expires_in": 7_776_000,
            "refresh_token": "refresh-2", "created_at": flixnet_db::now_ms() / 1000
        })))
        .mount(&server)
        .await;
    let refreshed = app
        .request(Method::POST, "/api/v1/trakt/sync", None, Some(&token))
        .await;
    assert_eq!(refreshed.status, StatusCode::OK, "{}", refreshed.json);
    let last = requests_to(&server, "/sync/watched/movies")
        .await
        .pop()
        .unwrap();
    assert_eq!(
        last.headers.get("authorization").unwrap(),
        "Bearer access-2"
    );
    let account = flixnet_db::ops::trakt_account(&app.db, f.admin_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(account.refresh_token, "refresh-2");

    // ---- Disconnect ----
    mount(&server, "POST", "/oauth/revoke", 200, json!({})).await;
    let gone = app
        .request(Method::DELETE, "/api/v1/trakt", None, Some(&token))
        .await;
    assert_eq!(gone.status, StatusCode::NO_CONTENT);
    assert_eq!(requests_to(&server, "/oauth/revoke").await.len(), 1);
    let status = app.get("/api/v1/trakt", Some(&token)).await;
    assert_eq!(status.json["connected"], false);
    let sync = app
        .request(Method::POST, "/api/v1/trakt/sync", None, Some(&token))
        .await;
    assert_eq!(sync.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn denied_and_expired_codes() {
    let server = MockServer::start().await;
    let uri = server.uri();
    let app = app_with(|c| c.integrations.trakt_api_url = uri).await;
    let admin = app.setup_admin().await;
    app.request(
        Method::PUT,
        "/api/v1/settings/trakt",
        Some(json!({ "client_id": "cid", "client_secret": "csec" })),
        Some(&admin),
    )
    .await;
    mount(
        &server,
        "POST",
        "/oauth/device/code",
        200,
        json!({ "device_code": "d", "user_code": "U", "verification_url": "https://trakt.tv/activate", "expires_in": 600, "interval": 5 }),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/oauth/device/token"))
        .respond_with(ResponseTemplate::new(418))
        .mount(&server)
        .await;

    let none = app
        .request(
            Method::POST,
            "/api/v1/trakt/connect/poll",
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(none.status, StatusCode::NOT_FOUND);
    app.request(Method::POST, "/api/v1/trakt/connect", None, Some(&admin))
        .await;
    let denied = app
        .request(
            Method::POST,
            "/api/v1/trakt/connect/poll",
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(denied.json["state"], "denied");
    assert_eq!(denied.json["status"]["connected"], false);
    let after = app
        .request(
            Method::POST,
            "/api/v1/trakt/connect/poll",
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(
        after.status,
        StatusCode::NOT_FOUND,
        "a denied attempt is forgotten"
    );
}
