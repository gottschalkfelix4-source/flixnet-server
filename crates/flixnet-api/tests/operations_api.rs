//! Backups (snapshot, retention, restore roundtrip), the dashboard (bandwidth of
//! streamed bytes, storage, jobs) and the log view.

mod common;

use axum::http::{Method, StatusCode};
use common::{PASSWORD, app, app_with};
use flixnet_api::ops::logs::{LOGS, LogLevel};
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::{Db, libraries, media, users};
use flixnet_library::scan::scan_library;
use serde_json::json;

#[tokio::test]
async fn backups_are_written_pruned_and_restorable() {
    let config_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(config_file.path(), "[log]\nlevel = \"debug\"\n").unwrap();
    let source = config_file.path().to_path_buf();
    let app = app_with(|c| c.source_file = Some(source)).await;
    let admin = app.setup_admin().await;

    let overview = app.get("/api/v1/backups", Some(&admin)).await;
    assert_eq!(overview.status, StatusCode::OK, "{}", overview.json);
    assert_eq!(overview.json["keep"], 7);
    assert_eq!(overview.json["is_default_directory"], true);
    assert!(
        overview.json["directory"]
            .as_str()
            .unwrap()
            .ends_with("backups")
    );
    assert_eq!(overview.json["files"], json!([]));

    for (body, why) in [
        (json!({ "keep": 0 }), "keep at least one"),
        (json!({ "keep": 1000 }), "too many"),
        (json!({ "directory": "relativ/ordner" }), "relative folder"),
    ] {
        let response = app
            .request(
                Method::PUT,
                "/api/v1/settings/backup",
                Some(body),
                Some(&admin),
            )
            .await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY, "{why}");
    }
    let target = tempfile::tempdir().unwrap();
    let changed = app
        .request(
            Method::PUT,
            "/api/v1/settings/backup",
            Some(json!({ "keep": 2, "directory": target.path().to_string_lossy() })),
            Some(&admin),
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.json);
    assert_eq!(changed.json["keep"], 2);
    assert_eq!(changed.json["is_default_directory"], false);

    users::create(
        &app.db,
        &users::NewUser {
            name: "Vorher",
            password_hash: None,
            pin_hash: None,
            is_admin: false,
            is_hidden: false,
            max_age_rating: None,
        },
    )
    .await
    .unwrap();
    let mut names = Vec::new();
    for _ in 0..3 {
        let backup = app
            .request(Method::POST, "/api/v1/backups", None, Some(&admin))
            .await;
        assert_eq!(backup.status, StatusCode::OK, "{}", backup.json);
        assert_eq!(backup.json["has_config"], true);
        assert!(backup.json["size_bytes"].as_u64().unwrap() > 0);
        names.push(backup.json["name"].as_str().unwrap().to_owned());
    }
    let overview = app.get("/api/v1/backups", Some(&admin)).await;
    let files = overview.json["files"].as_array().unwrap();
    assert_eq!(files.len(), 2, "only the newest two are kept");
    assert_eq!(files[0]["name"], names[2].as_str());
    assert!(!target.path().join(&names[0]).exists());
    assert!(
        !target
            .path()
            .join(names[0].replace(".db", ".toml"))
            .exists()
    );
    assert!(
        overview.json["restore_command"]
            .as_str()
            .unwrap()
            .contains(&names[2])
    );
    let backup = target.path().join(&names[2]);
    assert_eq!(
        std::fs::read_to_string(backup.with_extension("toml")).unwrap(),
        "[log]\nlevel = \"debug\"\n"
    );

    // Restore into another database that has moved on meanwhile.
    let live_dir = tempfile::tempdir().unwrap();
    let live = live_dir.path().join("flixnet.db");
    let db = Db::open(&live).await.unwrap();
    users::create_first_admin(&db, "Nachher", "x")
        .await
        .unwrap();
    db.close().await;
    assert!(flixnet_db::backup::verify(&backup).await.unwrap() > 0);
    let kept = flixnet_db::backup::restore(&backup, &live)
        .await
        .unwrap()
        .unwrap();
    assert!(kept.exists(), "the previous database is kept aside");
    let restored = Db::open(&live).await.unwrap();
    let names: Vec<String> = users::list(&restored)
        .await
        .unwrap()
        .into_iter()
        .map(|u| u.name)
        .collect();
    assert_eq!(names, ["Felix", "Vorher"]);
    restored.close().await;

    // Garbage is refused.
    let junk = live_dir.path().join("junk.db");
    std::fs::write(&junk, b"not a database").unwrap();
    assert!(flixnet_db::backup::restore(&junk, &live).await.is_err());

    // Administrators only.
    app.post(
        "/api/v1/users",
        json!({ "name": "Kind", "password": PASSWORD }),
        Some(&admin),
    )
    .await;
    let kid = app
        .post(
            "/api/v1/auth/login",
            common::credentials("Kind", PASSWORD, "k"),
            None,
        )
        .await;
    let kid = kid.json["token"].as_str().unwrap();
    assert_eq!(
        app.get("/api/v1/backups", Some(kid)).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.request(Method::POST, "/api/v1/backups", None, Some(kid))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn dashboard_counts_streamed_bytes() {
    let app = app().await;
    let root = app.dir.path().join("media/Filme/Alpha (2020)");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("Alpha.2020.mkv"), vec![7u8; 65_536]).unwrap();
    let queue = JobRuntime::new(app.db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    let library = libraries::create(
        &app.db,
        "Filme",
        "movies",
        &[app
            .dir
            .path()
            .join("media/Filme")
            .to_string_lossy()
            .into_owned()],
    )
    .await
    .unwrap();
    scan_library(&app.db, &queue, library.id).await.unwrap();
    let admin = app.setup_admin().await;
    let movies = app.get("/api/v1/items?kind=movie", Some(&admin)).await;
    let item = movies.json["items"][0]["id"].as_i64().unwrap();
    let file = media::files_for_item(&app.db, item).await.unwrap()[0].id;

    let empty = app.get("/api/v1/dashboard", Some(&admin)).await;
    assert_eq!(empty.status, StatusCode::OK, "{}", empty.json);
    assert_eq!(empty.json["bandwidth_total_bytes"], 0);
    let labels: Vec<&str> = empty.json["storage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["Konfiguration", "Transcoding"]);
    assert!(
        empty.json["storage"][0]["disk_total_bytes"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "{}",
        empty.json
    );
    assert!(empty.json["memory_total_bytes"].as_u64().unwrap() > 0);
    assert!(
        empty.json["jobs"]["queued"].as_i64().unwrap() >= 1,
        "probe jobs from the scan"
    );

    let started = app
        .post(
            "/api/v1/playback/sessions",
            json!({ "item_id": item, "position_ms": 0, "duration_ms": 60_000, "play_method": "direct_play" }),
            Some(&admin),
        )
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.json);
    let streamed = app
        .get(&format!("/api/v1/stream/{file}"), Some(&admin))
        .await;
    assert_eq!(streamed.status, StatusCode::OK);

    let dashboard = app.get("/api/v1/dashboard", Some(&admin)).await;
    assert_eq!(
        dashboard.json["bandwidth_total_bytes"], 65_536,
        "{}",
        dashboard.json
    );
    assert!(
        dashboard.json["bandwidth_bytes_per_second"]
            .as_u64()
            .unwrap()
            > 0
    );
    let session = &dashboard.json["sessions"][0];
    assert_eq!(session["title"], "Alpha (2020)");
    assert_eq!(session["user_name"], "Felix");
    assert_eq!(session["bytes"], 65_536);
    assert_eq!(session["play_method"], "direct_play");
    assert!(
        dashboard.json["storage"][0]["used_bytes"].as_u64().unwrap() > 0,
        "the database"
    );
}

#[tokio::test]
async fn logs_can_be_tailed() {
    let app = app().await;
    let admin = app.setup_admin().await;
    LOGS.push(LogLevel::Info, "flixnet_test", "info-zeile-4711".into());
    LOGS.push(LogLevel::Warn, "flixnet_test", "warn-zeile-4711".into());

    let all = app.get("/api/v1/logs?level=debug", Some(&admin)).await;
    assert_eq!(all.status, StatusCode::OK, "{}", all.json);
    let text = all.json["lines"].to_string();
    assert!(text.contains("info-zeile-4711") && text.contains("warn-zeile-4711"));
    let warn = app.get("/api/v1/logs?level=warn", Some(&admin)).await;
    let lines = warn.json["lines"].as_array().unwrap();
    assert!(
        lines
            .iter()
            .all(|l| l["level"] == "warn" || l["level"] == "error")
    );
    assert!(warn.json["lines"].to_string().contains("warn-zeile-4711"));
    let last = all.json["last_seq"].as_u64().unwrap();
    let tail = app
        .get(&format!("/api/v1/logs?after={last}"), Some(&admin))
        .await;
    assert_eq!(tail.json["lines"], json!([]));
    assert_eq!(
        app.get("/api/v1/logs?level=laut", Some(&admin))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}
