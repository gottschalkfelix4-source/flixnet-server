mod common;

use axum::http::{Method, StatusCode};
use common::{PASSWORD, app, credentials};
use serde_json::json;

#[tokio::test]
async fn admins_create_scan_and_delete_libraries() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let media = tempfile::tempdir().unwrap();
    let movies_dir = media.path().join("Filme");
    std::fs::create_dir_all(movies_dir.join("Some Movie (2020)")).unwrap();
    let movies_path = movies_dir.to_string_lossy().into_owned();

    let missing = app
        .post(
            "/api/v1/libraries",
            json!({ "name": "Filme", "kind": "movies", "paths": [media.path().join("nope").to_string_lossy()] }),
            Some(&admin),
        )
        .await;
    assert_eq!(missing.status, StatusCode::UNPROCESSABLE_ENTITY);
    let relative = app
        .post(
            "/api/v1/libraries",
            json!({ "name": "Filme", "kind": "movies", "paths": ["Filme"] }),
            Some(&admin),
        )
        .await;
    assert_eq!(relative.status, StatusCode::UNPROCESSABLE_ENTITY);

    let created = app
        .post(
            "/api/v1/libraries",
            json!({ "name": " Filme ", "kind": "movies", "paths": [movies_path, movies_path] }),
            Some(&admin),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    assert_eq!(created.json["name"], "Filme");
    assert_eq!(
        created.json["paths"],
        json!([movies_path]),
        "duplicates dropped"
    );
    let id = created.json["id"].as_i64().unwrap();

    // Creating queued a scan; asking again while it is pending is deduplicated.
    let jobs = app.get("/api/v1/jobs?state=queued", Some(&admin)).await;
    assert_eq!(jobs.json[0]["kind"], "library.scan");
    let scan = app
        .request(
            Method::POST,
            &format!("/api/v1/libraries/{id}/scan"),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(scan.status, StatusCode::ACCEPTED);
    assert_eq!(scan.json["job_id"], serde_json::Value::Null);

    let renamed = app
        .request(
            Method::PATCH,
            &format!("/api/v1/libraries/{id}"),
            Some(json!({ "name": "Kino" })),
            Some(&admin),
        )
        .await;
    assert_eq!(renamed.json["name"], "Kino");

    let list = app.get("/api/v1/libraries", Some(&admin)).await;
    assert_eq!(list.json.as_array().unwrap().len(), 1);
    assert_eq!(list.json[0]["counts"]["files"], 0);

    let deleted = app
        .request(
            Method::DELETE,
            &format!("/api/v1/libraries/{id}"),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.get("/api/v1/libraries", Some(&admin)).await.json,
        json!([])
    );
}

#[tokio::test]
async fn members_see_only_granted_libraries_without_paths() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let media = tempfile::tempdir().unwrap();
    let path = media.path().to_string_lossy().into_owned();
    let movies = app
        .post(
            "/api/v1/libraries",
            json!({ "name": "Filme", "kind": "movies", "paths": [path] }),
            Some(&admin),
        )
        .await
        .json["id"]
        .as_i64()
        .unwrap();
    let shows = app
        .post(
            "/api/v1/libraries",
            json!({ "name": "Serien", "kind": "shows", "paths": [path] }),
            Some(&admin),
        )
        .await
        .json["id"]
        .as_i64()
        .unwrap();

    let kid = app
        .post(
            "/api/v1/users",
            json!({ "name": "Kind", "password": PASSWORD }),
            Some(&admin),
        )
        .await
        .json["id"]
        .as_i64()
        .unwrap();
    let token = app
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

    // Default: all libraries, but no server paths for non-admins.
    let visible = app.get("/api/v1/libraries", Some(&token)).await;
    assert_eq!(visible.json.as_array().unwrap().len(), 2);
    assert_eq!(visible.json[0]["paths"], serde_json::Value::Null);

    let access = app
        .request(
            Method::PUT,
            &format!("/api/v1/users/{kid}/libraries"),
            Some(json!({ "all_libraries": false, "library_ids": [shows, 999] })),
            Some(&admin),
        )
        .await;
    assert_eq!(access.status, StatusCode::OK, "{}", access.json);
    assert_eq!(
        access.json["library_ids"],
        json!([shows]),
        "unknown ids are ignored"
    );

    let visible = app.get("/api/v1/libraries", Some(&token)).await;
    let names: Vec<_> = visible
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Serien"]);
    assert_eq!(
        app.get(&format!("/api/v1/libraries/{movies}"), Some(&token))
            .await
            .status,
        StatusCode::NOT_FOUND,
        "hidden libraries look like they do not exist"
    );
    assert_eq!(
        app.get(&format!("/api/v1/libraries/{shows}"), Some(&token))
            .await
            .status,
        StatusCode::OK
    );

    let read_back = app
        .get(&format!("/api/v1/users/{kid}/libraries"), Some(&admin))
        .await;
    assert_eq!(
        read_back.json,
        json!({ "all_libraries": false, "library_ids": [shows] })
    );
}

#[tokio::test]
async fn folder_browser_lists_directories() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let media = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(media.path().join("Serien")).unwrap();
    std::fs::create_dir_all(media.path().join("Filme")).unwrap();
    std::fs::create_dir_all(media.path().join(".versteckt")).unwrap();
    std::fs::write(media.path().join("datei.txt"), "x").unwrap();

    let roots = app.get("/api/v1/fs/directories", Some(&admin)).await;
    assert_eq!(roots.status, StatusCode::OK);
    assert!(!roots.json["directories"].as_array().unwrap().is_empty());

    let path = media.path().to_string_lossy();
    let listing = app
        .get(
            &format!("/api/v1/fs/directories?path={}", urlencode(&path)),
            Some(&admin),
        )
        .await;
    assert_eq!(listing.status, StatusCode::OK, "{}", listing.json);
    let names: Vec<_> = listing.json["directories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Filme", "Serien"]);
    assert!(listing.json["parent"].is_string());

    let missing = app
        .get(
            &format!(
                "/api/v1/fs/directories?path={}",
                urlencode(&format!("{path}/gibtsnicht"))
            ),
            Some(&admin),
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}
