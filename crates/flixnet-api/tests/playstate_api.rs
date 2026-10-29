mod common;

use axum::http::StatusCode;
use common::{PASSWORD, TestApp, app, credentials};
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::{libraries, playstate, userdata};
use flixnet_library::scan::scan_library;
use serde_json::{Value, json};

const MIN: i64 = 60_000;

struct Fixture {
    admin: String,
    admin_id: i64,
    movie: i64,
    series: i64,
    e1: i64,
    e2: i64,
}

/// One movie and a series with two episodes, scanned; an admin signed in.
async fn library(app: &TestApp, root: &std::path::Path) -> Fixture {
    for file in [
        "Filme/Alpha (2020)/Alpha.2020.mkv",
        "Serien/Dark/Staffel 1/Dark.S01E01.mkv",
        "Serien/Dark/Staffel 1/Dark.S01E02.mkv",
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
    let id = |json: &Value, n: usize| json["items"][n]["id"].as_i64().unwrap();
    let movie = app.get("/api/v1/items?kind=movie", Some(&admin)).await.json;
    let series = app
        .get("/api/v1/items?kind=series", Some(&admin))
        .await
        .json;
    let episodes = app
        .get(
            &format!("/api/v1/items?series_id={}&kind=episode", id(&series, 0)),
            Some(&admin),
        )
        .await
        .json;
    let me = app.get("/api/v1/users/me", Some(&admin)).await.json;
    Fixture {
        admin_id: me["id"].as_i64().unwrap(),
        movie: id(&movie, 0),
        series: id(&series, 0),
        e1: id(&episodes, 0),
        e2: id(&episodes, 1),
        admin,
    }
}

fn report(item: i64, position_ms: i64, duration_ms: i64) -> Value {
    json!({
        "item_id": item,
        "position_ms": position_ms,
        "duration_ms": duration_ms,
        "play_method": "direct_play",
    })
}

async fn start(app: &TestApp, token: &str, item: i64, duration_ms: i64) -> String {
    let started = app
        .post(
            "/api/v1/playback/sessions",
            report(item, 0, duration_ms),
            Some(token),
        )
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.json);
    started.json["id"].as_str().unwrap().to_owned()
}

fn ids(json: &Value) -> Vec<i64> {
    json.as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_i64().unwrap())
        .collect()
}

#[tokio::test]
async fn progress_resume_and_the_sessions_dashboard() {
    let app = app().await;
    let media = tempfile::tempdir().unwrap();
    let f = library(&app, media.path()).await;
    let token = Some(f.admin.as_str());
    let duration = 100 * MIN;

    let id = start(&app, &f.admin, f.movie, duration).await;
    let active = app.get("/api/v1/sessions/active", token).await;
    assert_eq!(active.status, StatusCode::OK, "{}", active.json);
    let session = &active.json[0];
    assert_eq!(session["id"], id.as_str());
    assert_eq!(session["user_name"], "Felix");
    assert_eq!(session["device_name"], "Testbrowser");
    assert_eq!(session["client"], "Flixnet Tests");
    assert_eq!(session["item"]["title"], "Alpha");
    assert_eq!(session["play_method"], "direct_play");
    assert_eq!(session["transcoding"], false);

    // Progress at 30 % (paused): the resume point and the dashboard follow.
    let progress = app
        .post(
            &format!("/api/v1/playback/sessions/{id}/progress"),
            json!({ "item_id": f.movie, "position_ms": 30 * MIN, "duration_ms": duration,
                    "play_method": "direct_play", "is_paused": true }),
            token,
        )
        .await;
    assert_eq!(progress.status, StatusCode::NO_CONTENT, "{}", progress.json);
    let active = app.get("/api/v1/sessions/active", token).await;
    assert_eq!(active.json[0]["position_ms"], 30 * MIN);
    assert_eq!(active.json[0]["is_paused"], true);
    let resume = app.get("/api/v1/items/resume", token).await;
    assert_eq!(ids(&resume.json), [f.movie]);
    assert_eq!(resume.json[0]["position_ms"], 30 * MIN);
    let detail = app.get(&format!("/api/v1/items/{}", f.movie), token).await;
    assert_eq!(detail.json["position_ms"], 30 * MIN, "item detail resumes");

    // Stop at 40 %: a resume point, no longer an active session.
    let stop = app
        .post(
            &format!("/api/v1/playback/sessions/{id}/stop"),
            report(f.movie, 40 * MIN, duration),
            token,
        )
        .await;
    assert_eq!(stop.status, StatusCode::OK, "{}", stop.json);
    assert_eq!(stop.json["played"], false);
    assert_eq!(stop.json["resume_position_ms"], 40 * MIN);
    assert_eq!(
        app.get("/api/v1/sessions/active", token).await.json,
        json!([])
    );

    // History: start, the 25 % milestone, stop.
    let events = playstate::events(&app.db, f.admin_id).await.unwrap();
    let kinds: Vec<_> = events.iter().map(|e| e.event.as_str()).collect();
    assert_eq!(kinds, ["start", "progress", "stop"]);
    assert_eq!(events[2].position_ms, 40 * MIN);
    assert_eq!(events[2].session_id.as_deref(), Some(id.as_str()));
    assert_eq!(events[2].device_name.as_deref(), Some("Testbrowser"));
    assert!(!events[2].completed);

    // Watching to the end marks it watched and clears the resume point.
    let again = start(&app, &f.admin, f.movie, duration).await;
    let stop = app
        .post(
            &format!("/api/v1/playback/sessions/{again}/stop"),
            report(f.movie, 95 * MIN, duration),
            token,
        )
        .await;
    assert_eq!(stop.json["played"], true);
    assert_eq!(stop.json["resume_position_ms"], 0);
    let data = userdata::get(&app.db, f.admin_id, f.movie).await.unwrap();
    assert!(data.played);
    assert_eq!(data.play_count, 1);
    assert_eq!(app.get("/api/v1/items/resume", token).await.json, json!([]));
}

#[tokio::test]
async fn next_up_after_the_first_episode() {
    let app = app().await;
    let media = tempfile::tempdir().unwrap();
    let f = library(&app, media.path()).await;
    let token = Some(f.admin.as_str());

    // Before watching: the series starts with episode 1; nothing is "next up".
    let first = app
        .get(&format!("/api/v1/items/{}/next-up", f.series), token)
        .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.json);
    assert_eq!(first.json["id"], f.e1);
    assert_eq!(first.json["series_title"], "Dark");
    assert_eq!(
        app.get("/api/v1/items/next-up", token).await.json,
        json!([])
    );

    let id = start(&app, &f.admin, f.e1, 40 * MIN).await;
    let stop = app
        .post(
            &format!("/api/v1/playback/sessions/{id}/stop"),
            report(f.e1, 39 * MIN, 40 * MIN),
            token,
        )
        .await;
    assert_eq!(stop.json["played"], true);

    let next_up = app.get("/api/v1/items/next-up", token).await;
    assert_eq!(ids(&next_up.json), [f.e2]);
    assert_eq!(next_up.json[0]["series_title"], "Dark");
    let series_next = app
        .get(&format!("/api/v1/items/{}/next-up", f.series), token)
        .await;
    assert_eq!(series_next.json["id"], f.e2);

    // The next episode for autoplay; none after the last.
    let next = app
        .get(&format!("/api/v1/items/{}/next-episode", f.e1), token)
        .await;
    assert_eq!(next.json["id"], f.e2);
    let last = app
        .get(&format!("/api/v1/items/{}/next-episode", f.e2), token)
        .await;
    assert_eq!(last.status, StatusCode::NO_CONTENT);

    // Movies have no series next-up.
    let movie = app
        .get(&format!("/api/v1/items/{}/next-up", f.movie), token)
        .await;
    assert_eq!(movie.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn reports_are_validated_and_scoped_to_the_user() {
    let app = app().await;
    let media = tempfile::tempdir().unwrap();
    let f = library(&app, media.path()).await;
    let token = Some(f.admin.as_str());

    // Only movies and episodes play; positions are not negative.
    let series = app
        .post("/api/v1/playback/sessions", report(f.series, 0, MIN), token)
        .await;
    assert_eq!(series.status, StatusCode::UNPROCESSABLE_ENTITY);
    let negative = app
        .post("/api/v1/playback/sessions", report(f.movie, -1, MIN), token)
        .await;
    assert_eq!(negative.status, StatusCode::UNPROCESSABLE_ENTITY);
    let missing = app
        .post("/api/v1/playback/sessions", report(999_999, 0, MIN), token)
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);

    // A profile with an age limit cannot see the unrated movie.
    app.post(
        "/api/v1/users",
        json!({ "name": "Kind", "password": PASSWORD, "max_age_rating": 12 }),
        token,
    )
    .await;
    let kid = app
        .post(
            "/api/v1/auth/login",
            credentials("Kind", PASSWORD, "tablet"),
            None,
        )
        .await
        .json["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let hidden = app
        .post(
            "/api/v1/playback/sessions",
            report(f.movie, 0, MIN),
            Some(&kid),
        )
        .await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    assert_eq!(
        app.get("/api/v1/sessions/active", Some(&kid)).await.status,
        StatusCode::FORBIDDEN
    );

    // Someone else's session.
    let id = start(&app, &f.admin, f.movie, 100 * MIN).await;
    let foreign = app
        .post(
            &format!("/api/v1/playback/sessions/{id}/progress"),
            report(f.movie, 30 * MIN, 100 * MIN),
            Some(&kid),
        )
        .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    let garbage = app
        .post(
            "/api/v1/playback/sessions/not-an-id/progress",
            report(f.movie, 30 * MIN, 100 * MIN),
            token,
        )
        .await;
    assert_eq!(garbage.status, StatusCode::NOT_FOUND);

    // An unknown id (server restarted, session expired) is picked up again.
    let lost = "0123456789abcdef0123456789abcdef";
    let resumed = app
        .post(
            &format!("/api/v1/playback/sessions/{lost}/progress"),
            report(f.movie, 30 * MIN, 100 * MIN),
            token,
        )
        .await;
    assert_eq!(resumed.status, StatusCode::NO_CONTENT, "{}", resumed.json);
    let active = app.get("/api/v1/sessions/active", token).await;
    assert_eq!(active.json.as_array().unwrap().len(), 2);
    assert!(
        active
            .json
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == lost)
    );
}
