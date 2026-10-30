//! SyncPlay: groups over REST, two members over real WebSockets, parental controls.

mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::{PASSWORD, TestApp, app, credentials};
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::{items, libraries};
use flixnet_library::refresh::refresh_local;
use flixnet_library::scan::scan_library;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Two films: Kinderfilm (FSK 0) and Geheimfilm (FSK 18). Returns their ids.
async fn films(app: &TestApp, root: &std::path::Path) -> (i64, i64) {
    for (dir, title, mpaa) in [
        ("Kinderfilm (2020)", "Kinderfilm", "FSK 0"),
        ("Geheimfilm (2019)", "Geheimfilm", "FSK 18"),
    ] {
        let folder = root.join(dir);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(format!("{title}.mkv")), b"video").unwrap();
        std::fs::write(
            folder.join("movie.nfo"),
            format!("<movie><title>{title}</title><mpaa>{mpaa}</mpaa></movie>"),
        )
        .unwrap();
    }
    let library = libraries::create(
        &app.db,
        "Filme",
        "movies",
        &[root.to_string_lossy().into_owned()],
    )
    .await
    .unwrap()
    .id;
    let queue = JobRuntime::new(app.db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    scan_library(&app.db, &queue, library).await.unwrap();
    let movies = items::list_by_kind(&app.db, library, "movie")
        .await
        .unwrap();
    for movie in &movies {
        refresh_local(&app.db, movie.id).await.unwrap();
    }
    let id = |title: &str| movies.iter().find(|m| m.title == title).unwrap().id;
    (id("Kinderfilm"), id("Geheimfilm"))
}

async fn kid(app: &TestApp, admin: &str) -> String {
    let created = app
        .post(
            "/api/v1/users",
            json!({ "name": "Kind", "password": PASSWORD, "max_age_rating": 12 }),
            Some(admin),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    app.post(
        "/api/v1/auth/login",
        credentials("Kind", PASSWORD, "kid-device"),
        None,
    )
    .await
    .json["token"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Serves the app on a real port (WebSockets need a connection).
async fn serve(app: &TestApp) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = app.router.clone();
    tokio::spawn(async move { axum::serve(listener, router).await });
    port
}

async fn connect(port: u16, group: &str, token: Option<&str>) -> Result<Socket, WsError> {
    let mut request = format!("ws://127.0.0.1:{port}/api/v1/syncplay/groups/{group}/ws")
        .into_client_request()
        .unwrap();
    let protocols = match token {
        Some(token) => format!("flixnet, flixnet.auth.{token}"),
        None => "flixnet".to_owned(),
    };
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_str(&protocols).unwrap(),
    );
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(socket, response)| {
            assert_eq!(response.headers()["sec-websocket-protocol"], "flixnet");
            socket
        })
}

async fn send(socket: &mut Socket, message: Value) {
    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .unwrap();
}

/// The next message of `kind` matching `accept` (others are skipped), within 5 s.
async fn next(socket: &mut Socket, kind: &str, accept: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let frame = tokio::time::timeout_at(deadline, socket.next())
            .await
            .unwrap_or_else(|_| panic!("no {kind} message within 5 s"))
            .expect("socket open")
            .expect("frame");
        let Message::Text(text) = frame else { continue };
        let message: Value = serde_json::from_str(&text).unwrap();
        if message["type"] == kind && accept(&message) {
            return message;
        }
    }
}

async fn state(socket: &mut Socket, accept: impl Fn(&Value) -> bool) -> Value {
    next(socket, "state", |m| accept(&m["state"])).await["state"].clone()
}

#[tokio::test]
async fn two_members_play_pause_and_seek_together() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let kid = kid(&app, &admin).await;
    let media = tempfile::tempdir().unwrap();
    let (kinderfilm, geheimfilm) = films(&app, media.path()).await;

    // Only films and episodes, only visible ones.
    let not_found = app
        .post(
            "/api/v1/syncplay/groups",
            json!({ "item_id": geheimfilm }),
            Some(&kid),
        )
        .await;
    assert_eq!(not_found.status, StatusCode::NOT_FOUND);
    let secret = app
        .post(
            "/api/v1/syncplay/groups",
            json!({ "item_id": geheimfilm }),
            Some(&admin),
        )
        .await;
    assert_eq!(secret.status, StatusCode::CREATED, "{}", secret.json);
    let secret_id = secret.json["id"].as_str().unwrap().to_owned();
    let created = app
        .post(
            "/api/v1/syncplay/groups",
            json!({ "item_id": kinderfilm, "name": "Filmabend" }),
            Some(&admin),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    assert_eq!(created.json["item_title"], "Kinderfilm");
    assert_eq!(created.json["name"], "Filmabend");
    let group = created.json["id"].as_str().unwrap().to_owned();
    let code = created.json["code"].as_str().unwrap().to_owned();

    // The child sees (and can join) only the group of a film it may watch.
    let listed = app.get("/api/v1/syncplay/groups", Some(&kid)).await;
    let ids: Vec<&str> = listed
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [group.as_str()]);
    assert_eq!(
        app.get("/api/v1/syncplay/groups", Some(&admin))
            .await
            .json
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        app.get(&format!("/api/v1/syncplay/groups/{secret_id}"), Some(&kid))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let secret_code = secret.json["code"].as_str().unwrap();
    assert_eq!(
        app.post(
            &format!("/api/v1/syncplay/join/{secret_code}"),
            json!({}),
            Some(&kid)
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let joined = app
        .post(
            &format!("/api/v1/syncplay/join/{code}"),
            json!({}),
            Some(&kid),
        )
        .await;
    assert_eq!(joined.status, StatusCode::OK);
    assert_eq!(joined.json["id"], group);

    let port = serve(&app).await;
    // No token: refused before the upgrade; restricted group: not found.
    match connect(port, &group, None).await {
        Err(WsError::Http(response)) => assert_eq!(response.status(), 401),
        other => panic!("expected 401, got {other:?}"),
    }
    match connect(port, &secret_id, Some(&kid)).await {
        Err(WsError::Http(response)) => assert_eq!(response.status(), 404),
        other => panic!("expected 404, got {other:?}"),
    }

    let mut a = connect(port, &group, Some(&admin)).await.unwrap();
    let welcome = next(&mut a, "welcome", |_| true).await;
    let a_id = welcome["member_id"].as_u64().unwrap();
    let mut b = connect(port, &group, Some(&kid)).await.unwrap();
    next(&mut b, "welcome", |_| true).await;
    let joined = state(&mut a, |s| s["members"].as_array().unwrap().len() == 2).await;
    assert_eq!(joined["playing"], false);
    assert_eq!(joined["item_id"], kinderfilm);
    let names: Vec<&str> = joined["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Felix", "Kind"]);

    // Play in A → B plays, starting slightly in the future.
    send(&mut a, json!({ "type": "play" })).await;
    let playing = state(&mut b, |s| s["playing"] == true).await;
    assert!(playing["anchor_ms"].as_i64().unwrap() >= playing["server_ms"].as_i64().unwrap());

    // Clock sync.
    send(&mut b, json!({ "type": "ping", "client_ms": 123.5 })).await;
    let pong = next(&mut b, "pong", |_| true).await;
    assert_eq!(pong["client_ms"], 123.5);
    assert!(pong["server_ms"].as_i64().unwrap() > 0);

    // B is far ahead: told to seek back to the group position.
    tokio::time::sleep(Duration::from_millis(700)).await;
    let now = flixnet_db::now_ms();
    send(
        &mut b,
        json!({ "type": "report", "position_ms": 600_000, "server_ms": now }),
    )
    .await;
    let correction = next(&mut b, "correction", |_| true).await;
    let seek_to = correction["seek_to_ms"].as_i64().expect("a seek");
    assert!(seek_to < 5_000, "{correction}");

    // Pause in A → B pauses at that position.
    send(&mut a, json!({ "type": "pause", "position_ms": 1_500 })).await;
    let paused = state(&mut b, |s| s["playing"] == false).await;
    assert_eq!(paused["position_ms"], 1_500);
    assert_eq!(paused["waiting"], false);

    // A seek makes everybody wait until all are ready; then it stays paused.
    send(&mut b, json!({ "type": "seek", "position_ms": 30_000 })).await;
    let waiting = state(&mut a, |s| s["waiting"] == true).await;
    assert_eq!(waiting["position_ms"], 30_000);
    send(&mut a, json!({ "type": "play" })).await;
    send(&mut a, json!({ "type": "ready" })).await;
    send(&mut b, json!({ "type": "ready" })).await;
    let resumed = state(&mut a, |s| s["playing"] == true).await;
    assert_eq!(resumed["position_ms"], 30_000);
    assert_eq!(resumed["waiting"], false);

    // A buffering member pauses the group until it is ready again.
    send(&mut b, json!({ "type": "buffering" })).await;
    let held = state(&mut a, |s| s["waiting"] == true).await;
    assert_eq!(held["playing"], false);
    send(&mut b, json!({ "type": "ready" })).await;
    state(&mut a, |s| s["playing"] == true && s["waiting"] == false).await;

    // The child may not see Geheimfilm: nobody switches to it.
    send(&mut a, json!({ "type": "set_item", "item_id": geheimfilm })).await;
    let refused = next(&mut a, "error", |_| true).await;
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .contains("Nicht alle Mitglieder")
    );

    // Reactions reach everybody; garbage gets an error.
    send(&mut a, json!({ "type": "reaction", "emoji": "🍿" })).await;
    let reaction = next(&mut b, "reaction", |_| true).await;
    assert_eq!(reaction["emoji"], "🍿");
    assert_eq!(reaction["member_id"], a_id);
    assert_eq!(reaction["name"], "Felix");
    b.send(Message::Text("{\"type\":\"explode\"}".into()))
        .await
        .unwrap();
    next(&mut b, "error", |_| true).await;

    // A leaves: B sees itself alone (the group goes on).
    a.close(None).await.unwrap();
    let alone = state(&mut b, |s| s["members"].as_array().unwrap().len() == 1).await;
    assert_eq!(alone["members"][0]["name"], "Kind");

    // Only the creator (or an administrator) ends the group; members are told.
    let forbidden = app
        .request(
            Method::DELETE,
            &format!("/api/v1/syncplay/groups/{group}"),
            None,
            Some(&kid),
        )
        .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);
    let ended = app
        .request(
            Method::DELETE,
            &format!("/api/v1/syncplay/groups/{group}"),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(ended.status, StatusCode::NO_CONTENT);
    let closed = next(&mut b, "closed", |_| true).await;
    assert!(closed["reason"].as_str().unwrap().contains("beendet"));
    assert_eq!(
        app.get(&format!("/api/v1/syncplay/groups/{group}"), Some(&admin))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn groups_validate_their_input() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let media = tempfile::tempdir().unwrap();
    let (kinderfilm, _) = films(&app, media.path()).await;
    let long = app
        .post(
            "/api/v1/syncplay/groups",
            json!({ "item_id": kinderfilm, "name": "x".repeat(81) }),
            Some(&admin),
        )
        .await;
    assert_eq!(long.status, StatusCode::UNPROCESSABLE_ENTITY);
    let default_name = app
        .post(
            "/api/v1/syncplay/groups",
            json!({ "item_id": kinderfilm }),
            Some(&admin),
        )
        .await;
    assert_eq!(default_name.json["name"], "Gruppe von Felix");
    assert_eq!(
        app.post(
            "/api/v1/syncplay/groups",
            json!({ "item_id": 999 }),
            Some(&admin)
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // Without an upgrade the socket route says so (after authentication).
    let group = default_name.json["id"].as_str().unwrap();
    let plain = app
        .get(&format!("/api/v1/syncplay/groups/{group}/ws"), Some(&admin))
        .await;
    assert_eq!(plain.status, StatusCode::BAD_REQUEST);
    assert_eq!(plain.json["code"], "websocket_required");
}
