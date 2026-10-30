mod common;

use axum::http::{Method, StatusCode};
use common::{PASSWORD, app, credentials};
use serde_json::json;

#[tokio::test]
async fn admins_manage_jobs_and_tasks() {
    let app = app().await;
    let admin = app.setup_admin().await;

    let tasks = app.get("/api/v1/tasks", Some(&admin)).await;
    assert_eq!(tasks.status, StatusCode::OK);
    assert_eq!(tasks.json[0]["name"], "cleanup_jobs");
    assert_eq!(tasks.json[0]["interval_ms"], 24 * 60 * 60 * 1000);

    assert_eq!(app.get("/api/v1/jobs", Some(&admin)).await.json, json!([]));

    // Run now → a queued job (no workers run in API tests).
    let run = app
        .request(
            Method::POST,
            "/api/v1/tasks/cleanup_jobs/run",
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(run.status, StatusCode::ACCEPTED);
    let jobs = app.get("/api/v1/jobs?state=queued", Some(&admin)).await;
    let job = &jobs.json[0];
    assert_eq!(job["kind"], "maintenance.cleanup_jobs");
    assert_eq!(job["pool"], "io");
    let id = job["id"].as_i64().unwrap();

    // Running it twice while pending is deduplicated.
    app.request(
        Method::POST,
        "/api/v1/tasks/cleanup_jobs/run",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(
        app.get("/api/v1/jobs", Some(&admin))
            .await
            .json
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let summary = app.get("/api/v1/jobs/summary", Some(&admin)).await;
    let io = summary
        .json
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["pool"] == "io")
        .unwrap();
    assert_eq!(io["queued"], 1);

    let cancel = app
        .request(
            Method::POST,
            &format!("/api/v1/jobs/{id}/cancel"),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(cancel.status, StatusCode::NO_CONTENT);
    let again = app
        .request(
            Method::POST,
            &format!("/api/v1/jobs/{id}/cancel"),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);

    let retry = app
        .request(
            Method::POST,
            &format!("/api/v1/jobs/{id}/retry"),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(retry.status, StatusCode::NO_CONTENT);
    let queued = app.get("/api/v1/jobs?state=queued", Some(&admin)).await;
    assert_eq!(queued.json[0]["id"], id);

    let bad = app
        .request(
            Method::PATCH,
            "/api/v1/tasks/cleanup_jobs",
            Some(json!({ "interval_ms": 1000 })),
            Some(&admin),
        )
        .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    let updated = app
        .request(
            Method::PATCH,
            "/api/v1/tasks/cleanup_jobs",
            Some(json!({ "interval_ms": 3_600_000, "enabled": false })),
            Some(&admin),
        )
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.json);
    assert_eq!(updated.json["interval_ms"], 3_600_000);
    assert_eq!(updated.json["enabled"], false);

    let unknown = app
        .request(Method::POST, "/api/v1/tasks/nope/run", None, Some(&admin))
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn members_cannot_see_jobs() {
    let app = app().await;
    let admin = app.setup_admin().await;
    app.post(
        "/api/v1/users",
        json!({ "name": "Anna", "password": PASSWORD }),
        Some(&admin),
    )
    .await;
    let anna = app
        .post(
            "/api/v1/auth/login",
            credentials("Anna", PASSWORD, "d"),
            None,
        )
        .await
        .json["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        app.get("/api/v1/jobs", Some(&anna)).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.get("/api/v1/tasks", Some(&anna)).await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn activity_lists_scans_per_library_and_batches_with_progress() {
    use flixnet_db::jobs::{self, NewJob};

    let app = app().await;
    let admin = app.setup_admin().await;
    assert_eq!(
        app.get("/api/v1/jobs/activity", None).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.get("/api/v1/jobs/activity", Some(&admin)).await.json,
        json!([]),
        "idle server"
    );

    // Creating a library queues its scan.
    let media = tempfile::tempdir().unwrap();
    let created = app
        .post(
            "/api/v1/libraries",
            json!({ "name": "Filme", "kind": "movies", "paths": [media.path().to_string_lossy()] }),
            Some(&admin),
        )
        .await;
    let library = created.json["id"].as_i64().unwrap();

    let job = |kind: &'static str, pool: &'static str, run_after: Option<i64>| NewJob {
        kind,
        pool,
        payload: "{}",
        priority: 50,
        max_attempts: 3,
        dedupe_key: None,
        run_after,
    };
    // A batch of three probes: one done, one running, one waiting.
    for _ in 0..3 {
        jobs::enqueue(&app.db, &job("media.probe", "probe", None))
            .await
            .unwrap();
    }
    let first = jobs::claim(&app.db, "probe").await.unwrap().unwrap();
    jobs::complete(&app.db, first.id).await.unwrap();
    jobs::claim(&app.db, "probe").await.unwrap().unwrap();
    // Bookkeeping and retries waiting for their back-off are not activity.
    jobs::enqueue(&app.db, &job("notify.deliver", "io", None))
        .await
        .unwrap();
    let later = flixnet_db::now_ms() + 60_000;
    jobs::enqueue(&app.db, &job("media.trickplay", "cpu", Some(later)))
        .await
        .unwrap();

    let activity = app.get("/api/v1/jobs/activity", Some(&admin)).await;
    assert_eq!(activity.status, StatusCode::OK);
    assert_eq!(
        activity.json,
        json!([
            {
                "kind": "library.scan", "library_id": library, "library_name": "Filme",
                "running": 0, "queued": 1, "done": 0, "progress": null
            },
            {
                "kind": "media.probe", "library_id": null, "library_name": null,
                "running": 1, "queued": 1, "done": 1, "progress": 1.0 / 3.0
            }
        ])
    );
}
