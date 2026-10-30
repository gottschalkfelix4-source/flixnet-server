//! Notification targets: CRUD with masked secrets, test messages for every kind
//! (webhook with HMAC signature, ntfy, Discord, e-mail via a local SMTP stand-in) and
//! event-driven delivery through the job system, including retries.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::{TestApp, app};
use flixnet_api::ops::notify::DeliverNotification;
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

async fn create(app: &TestApp, token: &str, body: Value) -> Value {
    let response = app
        .post("/api/v1/notifications/targets", body, Some(token))
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.json);
    response.json
}

async fn test_send(app: &TestApp, token: &str, id: i64) -> Value {
    let response = app
        .request(
            Method::POST,
            &format!("/api/v1/notifications/targets/{id}/test"),
            None,
            Some(token),
        )
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.json);
    response.json
}

/// Waits until the mock server saw `n` requests.
async fn wait_for_requests(server: &MockServer, n: usize) -> Vec<Request> {
    for _ in 0..200 {
        let received = server.received_requests().await.unwrap_or_default();
        if received.len() >= n {
            return received;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("expected {n} requests at the mock server");
}

#[tokio::test]
async fn targets_crud_masks_secrets_and_validates() {
    let app = app().await;
    let admin = app.setup_admin().await;

    let events = app.get("/api/v1/notifications/events", Some(&admin)).await;
    assert_eq!(events.status, StatusCode::OK);
    let kinds: Vec<&str> = events
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"playback.started"));
    assert!(kinds.contains(&"dvr.recording_finished"));

    let webhook = create(
        &app,
        &admin,
        json!({
            "name": "Home Assistant", "kind": "webhook", "url": "https://ha.local/api/webhook/x",
            "secret": "geheim", "events": ["playback.started", "playback.started", "user.created"]
        }),
    )
    .await;
    assert_eq!(webhook["has_secret"], true);
    assert!(webhook.get("secret").is_none());
    assert_eq!(
        webhook["events"],
        json!(["playback.started", "user.created"])
    );
    assert_eq!(webhook["enabled"], true);

    let discord = create(
        &app,
        &admin,
        json!({ "name": "Discord", "kind": "discord", "webhook_url": "https://discord.com/api/webhooks/1/token123" }),
    )
    .await;
    assert_eq!(
        discord["webhook_url"],
        "https://discord.com/api/webhooks/1/••••••"
    );
    let listed = app.get("/api/v1/notifications/targets", Some(&admin)).await;
    assert!(!listed.json.to_string().contains("token123"));
    assert!(!listed.json.to_string().contains("geheim"));

    // Updating without a secret keeps it; "" removes it.
    let id = webhook["id"].as_i64().unwrap();
    let uri = format!("/api/v1/notifications/targets/{id}");
    let kept = app
        .request(
            Method::PUT,
            &uri,
            Some(json!({ "name": "HA", "events": [], "enabled": false })),
            Some(&admin),
        )
        .await;
    assert_eq!(kept.status, StatusCode::OK, "{}", kept.json);
    assert_eq!(kept.json["has_secret"], true);
    assert_eq!(kept.json["enabled"], false);
    assert_eq!(kept.json["url"], "https://ha.local/api/webhook/x");
    let removed = app
        .request(
            Method::PUT,
            &uri,
            Some(json!({ "name": "HA", "secret": "" })),
            Some(&admin),
        )
        .await;
    assert_eq!(removed.json["has_secret"], false);

    for (body, why) in [
        (json!({ "name": "x", "kind": "webhook" }), "missing URL"),
        (
            json!({ "name": "x", "kind": "webhook", "url": "ftp://a" }),
            "bad scheme",
        ),
        (
            json!({ "name": "x", "kind": "ntfy", "topic": "a/b" }),
            "bad topic",
        ),
        (
            json!({ "name": "x", "kind": "email", "recipients": ["kein-mail"] }),
            "bad address",
        ),
        (
            json!({ "name": "x", "kind": "webhook", "url": "https://a", "events": ["nope"] }),
            "unknown event",
        ),
        (
            json!({ "name": "", "kind": "webhook", "url": "https://a" }),
            "empty name",
        ),
        (json!({ "name": "x" }), "missing kind"),
    ] {
        let response = app
            .post("/api/v1/notifications/targets", body, Some(&admin))
            .await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY, "{why}");
    }

    let deleted = app.request(Method::DELETE, &uri, None, Some(&admin)).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    let again = app.request(Method::DELETE, &uri, None, Some(&admin)).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);

    // Administrators only.
    let user = app
        .post(
            "/api/v1/users",
            json!({ "name": "Kind", "password": common::PASSWORD }),
            Some(&admin),
        )
        .await;
    assert_eq!(user.status, StatusCode::CREATED);
    let login = app
        .post(
            "/api/v1/auth/login",
            common::credentials("Kind", common::PASSWORD, "kid"),
            None,
        )
        .await;
    let kid = login.json["token"].as_str().unwrap();
    assert_eq!(
        app.get("/api/v1/notifications/targets", Some(kid))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.get("/api/v1/settings/smtp", Some(kid)).await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn webhook_test_messages_are_signed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/hook"))
        .and(header("X-Flixnet-Event", "test"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let app = app().await;
    let admin = app.setup_admin().await;
    let target = create(
        &app,
        &admin,
        json!({ "name": "Hook", "kind": "webhook", "url": format!("{}/hook", server.uri()), "secret": "s3cret" }),
    )
    .await;
    let result = test_send(&app, &admin, target["id"].as_i64().unwrap()).await;
    assert_eq!(result["ok"], true, "{result}");

    let request = &server.received_requests().await.unwrap()[0];
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["event"], "test");
    assert_eq!(body["title"], "Testnachricht von Flixnet");
    assert_eq!(body["server"]["name"], "Flixnet");
    let signature = request
        .headers
        .get("X-Flixnet-Signature")
        .unwrap()
        .to_str()
        .unwrap();
    let (t, v1) = signature.split_once(',').unwrap();
    let t = t.strip_prefix("t=").unwrap();
    let mut mac = Hmac::<Sha256>::new_from_slice(b"s3cret").unwrap();
    mac.update(format!("{t}.").as_bytes());
    mac.update(&request.body);
    assert_eq!(
        v1,
        format!("v1={}", hex::encode(mac.finalize().into_bytes()))
    );

    // A failing endpoint is reported, not hidden.
    let broken = create(
        &app,
        &admin,
        json!({ "name": "Kaputt", "kind": "webhook", "url": format!("{}/missing", server.uri()) }),
    )
    .await;
    let result = test_send(&app, &admin, broken["id"].as_i64().unwrap()).await;
    assert_eq!(result["ok"], false);
    assert!(
        result["error"].as_str().unwrap().contains("404"),
        "{result}"
    );
}

#[tokio::test]
async fn ntfy_and_discord_payloads() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(header("authorization", "Bearer tk_123"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/webhooks/1/abc"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let app = app().await;
    let admin = app.setup_admin().await;
    let ntfy = create(
        &app,
        &admin,
        json!({ "name": "Handy", "kind": "ntfy", "server_url": server.uri(), "topic": "flixnet-felix", "token": "tk_123", "priority": 4 }),
    )
    .await;
    assert_eq!(ntfy["has_token"], true);
    assert_eq!(
        test_send(&app, &admin, ntfy["id"].as_i64().unwrap()).await["ok"],
        true
    );
    let discord = create(
        &app,
        &admin,
        json!({ "name": "Discord", "kind": "discord", "webhook_url": format!("{}/api/webhooks/1/abc", server.uri()) }),
    )
    .await;
    assert_eq!(
        test_send(&app, &admin, discord["id"].as_i64().unwrap()).await["ok"],
        true
    );

    let requests = server.received_requests().await.unwrap();
    let ntfy_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(ntfy_body["topic"], "flixnet-felix");
    assert_eq!(ntfy_body["priority"], 4);
    assert_eq!(ntfy_body["title"], "Testnachricht von Flixnet");
    let discord_body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(discord_body["username"], "Flixnet");
    assert_eq!(
        discord_body["embeds"][0]["title"],
        "Testnachricht von Flixnet"
    );
}

/// A minimal SMTP server: accepts one message and hands it out.
async fn fake_smtp() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mails = Arc::new(Mutex::new(Vec::new()));
    let store = mails.clone();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let store = store.clone();
            tokio::spawn(async move {
                let (read, mut write) = socket.into_split();
                let mut lines = BufReader::new(read).lines();
                write.write_all(b"220 test ESMTP\r\n").await.unwrap();
                let mut envelope = String::new();
                while let Ok(Some(line)) = lines.next_line().await {
                    let upper = line.to_ascii_uppercase();
                    let reply: &[u8] = if upper.starts_with("EHLO") || upper.starts_with("HELO") {
                        b"250-test\r\n250 8BITMIME\r\n"
                    } else if upper.starts_with("DATA") {
                        write.write_all(b"354 go ahead\r\n").await.unwrap();
                        let mut data = String::new();
                        while let Ok(Some(line)) = lines.next_line().await {
                            if line == "." {
                                break;
                            }
                            data.push_str(&line);
                            data.push('\n');
                        }
                        store.lock().unwrap().push(format!("{envelope}{data}"));
                        b"250 queued\r\n"
                    } else if upper.starts_with("QUIT") {
                        let _ = write.write_all(b"221 bye\r\n").await;
                        return;
                    } else {
                        envelope.push_str(&line);
                        envelope.push('\n');
                        b"250 OK\r\n"
                    };
                    write.write_all(reply).await.unwrap();
                }
            });
        }
    });
    (port, mails)
}

#[tokio::test]
async fn email_goes_through_smtp() {
    let (port, mails) = fake_smtp().await;
    let app = app().await;
    let admin = app.setup_admin().await;

    let target = create(
        &app,
        &admin,
        json!({ "name": "Mail", "kind": "email", "recipients": ["felix@example.org"] }),
    )
    .await;
    // Without SMTP settings the test explains what is missing.
    let result = test_send(&app, &admin, target["id"].as_i64().unwrap()).await;
    assert_eq!(result["ok"], false);
    assert!(
        result["error"].as_str().unwrap().contains("SMTP"),
        "{result}"
    );

    let smtp = app
        .request(
            Method::PUT,
            "/api/v1/settings/smtp",
            Some(json!({
                "host": "127.0.0.1", "port": port, "security": "none",
                "from_address": "flixnet@example.org", "from_name": "Flixnet Server",
                "password": "nicht-zurückgeben"
            })),
            Some(&admin),
        )
        .await;
    assert_eq!(smtp.status, StatusCode::OK, "{}", smtp.json);
    assert_eq!(smtp.json["has_password"], true);
    assert!(smtp.json.get("password").is_none());
    let invalid = app
        .request(
            Method::PUT,
            "/api/v1/settings/smtp",
            Some(json!({ "from_address": "kaputt" })),
            Some(&admin),
        )
        .await;
    assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);

    let result = test_send(&app, &admin, target["id"].as_i64().unwrap()).await;
    assert_eq!(result["ok"], true, "{result}");
    let mails = mails.lock().unwrap();
    assert_eq!(mails.len(), 1);
    let mail = &mails[0];
    assert!(mail.contains("<felix@example.org>"), "{mail}");
    assert!(
        mail.contains("Subject: Testnachricht von Flixnet"),
        "{mail}"
    );
    assert!(
        mail.contains("From: \"Flixnet Server\" <flixnet@example.org>")
            || mail.contains("From: Flixnet Server <flixnet@example.org>"),
        "{mail}"
    );
}

#[tokio::test]
async fn scans_announce_new_items() {
    let app = app().await;
    let root = app.dir.path().join("media/Filme");
    for name in ["Alpha (2020)/Alpha.2020.mkv", "Beta (2019)/Beta.2019.mkv"] {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"v").unwrap();
    }
    let library = flixnet_db::libraries::create(
        &app.db,
        "Filme",
        "movies",
        &[root.to_string_lossy().into_owned()],
    )
    .await
    .unwrap();
    let bus = EventBus::new();
    let mut events = bus.subscribe();
    let runtime = JobRuntime::new(app.db.clone(), bus.clone())
        .register(flixnet_library::jobs::ScanLibrary)
        .start()
        .await
        .unwrap();
    flixnet_library::jobs::queue_scan(runtime.handle().queue(), library.id, 100)
        .await
        .unwrap();

    let mut added = None;
    let mut finished = None;
    while finished.is_none() {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .unwrap()
            .unwrap();
        match event.notification_kind() {
            Some("library.items_added") => added = Some(event),
            Some("library.scan_finished") => finished = Some(event),
            _ => {}
        }
    }
    runtime.shutdown(Duration::from_secs(2)).await;

    let added = flixnet_api::ops::notify::render(&app.db, &added.expect("items added event"))
        .await
        .unwrap();
    assert_eq!(added.title, "Neu in Filme: 2 Einträge");
    assert_eq!(added.message, "• Alpha (2020)\n• Beta (2019)");
    let finished = flixnet_api::ops::notify::render(&app.db, &finished.unwrap())
        .await
        .unwrap();
    assert_eq!(finished.title, "Scan abgeschlossen: Filme");
    assert_eq!(finished.message, "2 neu, 0 geändert, 0 fehlen");
}

#[tokio::test]
async fn events_are_delivered_by_jobs_with_retries() {
    let server = MockServer::start().await;
    // The first delivery fails; the job system retries.
    Mock::given(method("POST"))
        .and(path("/events"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/events"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let app = app().await;
    let admin = app.setup_admin().await;
    create(
        &app,
        &admin,
        json!({
            "name": "Alles", "kind": "webhook", "url": format!("{}/events", server.uri()),
            "events": ["user.created", "user.new_device"]
        }),
    )
    .await;
    create(
        &app,
        &admin,
        json!({
            "name": "Aus", "kind": "webhook", "url": format!("{}/disabled", server.uri()),
            "events": ["user.created"], "enabled": false
        }),
    )
    .await;

    // Workers for the delivery jobs the app's listener queues.
    let runtime = JobRuntime::new(app.db.clone(), EventBus::new())
        .register(DeliverNotification {
            http: flixnet_library::http::client(),
        })
        .retry_backoff(Duration::from_millis(50))
        .start()
        .await
        .unwrap();

    let created = app
        .post(
            "/api/v1/users",
            json!({ "name": "Anna", "password": common::PASSWORD }),
            Some(&admin),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    let requests = wait_for_requests(&server, 2).await;
    for request in &requests {
        assert_eq!(
            request.url.path(),
            "/events",
            "disabled targets get nothing"
        );
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["event"], "user.created");
        assert_eq!(body["title"], "Neues Nutzerkonto: Anna");
        assert_eq!(body["data"]["type"], "user_created");
        assert_eq!(body["data"]["name"], "Anna");
    }

    // Signing in on a new device notifies; the same device again does not.
    let login = |device: &'static str| {
        app.post(
            "/api/v1/auth/login",
            common::credentials("Anna", common::PASSWORD, device),
            None,
        )
    };
    assert_eq!(login("anna-phone").await.status, StatusCode::OK);
    let requests = wait_for_requests(&server, 3).await;
    let body: Value = serde_json::from_slice(&requests[2].body).unwrap();
    assert_eq!(body["event"], "user.new_device");
    assert_eq!(body["title"], "Neue Anmeldung: Anna");
    assert_eq!(login("anna-phone").await.status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(server.received_requests().await.unwrap().len(), 3);

    runtime.shutdown(Duration::from_secs(2)).await;
}
