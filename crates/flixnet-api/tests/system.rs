use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use flixnet_api::{AppState, router};
use flixnet_core::Config;
use flixnet_db::Db;
use http_body_util::BodyExt;
use tempfile::TempDir;
use tower::ServiceExt;

/// Router backed by a fresh database; keep the `TempDir` alive for the test's duration.
async fn app() -> (TempDir, axum::Router) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    (dir, router(AppState::new(Config::default(), db)))
}

#[tokio::test]
async fn system_info_returns_server_details() {
    let (_dir, app) = app().await;
    let response = app
        .oneshot(
            Request::get("/api/v1/system/info")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["name"], "Flixnet");
    assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(json["api_version"], "v1");
    assert_eq!(json["os"], std::env::consts::OS);
}

#[tokio::test]
async fn unknown_route_is_not_found() {
    let (_dir, app) = app().await;
    let response = app
        .oneshot(Request::get("/api/v1/nope").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
