mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::app;
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn system_info_returns_server_details() {
    let app = app().await;
    let response = app.get("/api/v1/system/info", None).await;

    assert_eq!(response.status, StatusCode::OK);
    let json = response.json;
    assert_eq!(json["name"], "Flixnet");
    assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(json["api_version"], "v1");
    assert_eq!(json["os"], std::env::consts::OS);
}

#[tokio::test]
async fn openapi_spec_is_served_and_documents_routes() {
    let app = app().await;
    let response = app.get("/api/v1/openapi.json", None).await;
    assert_eq!(response.status, StatusCode::OK);

    let spec = response.json;
    assert!(spec["openapi"].as_str().unwrap().starts_with("3."));
    assert_eq!(spec["info"]["title"], "Flixnet API");
    let op = &spec["paths"]["/api/v1/system/info"]["get"];
    assert_eq!(op["operationId"], "getSystemInfo");
    assert!(spec["components"]["schemas"]["SystemInfo"].is_object());
    assert!(spec["components"]["securitySchemes"]["bearer"].is_object());

    // The served spec is the same document the `openapi` CLI command exports.
    let exported = serde_json::to_value(flixnet_api::openapi()).unwrap();
    assert_eq!(spec, exported);
}

#[tokio::test]
async fn operation_ids_are_unique() {
    let spec = serde_json::to_value(flixnet_api::openapi()).unwrap();
    let mut ids: Vec<String> = spec["paths"]
        .as_object()
        .unwrap()
        .values()
        .flat_map(|item| item.as_object().unwrap().values())
        .filter_map(|op| op["operationId"].as_str().map(str::to_owned))
        .collect();
    let total = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), total, "duplicate operationId");
}

#[tokio::test]
async fn api_docs_page_is_served() {
    let app = app().await;
    let response = app
        .router
        .clone()
        .oneshot(Request::get("/api/v1/docs").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response.headers()[header::CONTENT_TYPE].to_str().unwrap();
    assert!(content_type.starts_with("text/html"), "{content_type}");
    let _ = response.into_body().collect().await.unwrap();
}

#[tokio::test]
async fn unknown_route_is_not_found() {
    let app = app().await;
    let response = app.get("/api/v1/nope", None).await;
    assert_eq!(response.status, StatusCode::NOT_FOUND);
}
