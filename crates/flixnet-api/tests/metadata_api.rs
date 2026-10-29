mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::{PASSWORD, TestApp, app, credentials};
use flixnet_api::signing::UrlSigner;
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::{items, libraries, metadata};
use flixnet_library::refresh::refresh_local;
use flixnet_library::scan::scan_library;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

/// A movie folder with a real PNG poster, scanned and refreshed. Returns (library, image id).
async fn movie_with_poster(app: &TestApp, root: &std::path::Path) -> (i64, i64) {
    let folder = root.join("Poster Film (2020)");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("Poster.Film.2020.mkv"), b"video").unwrap();
    image::RgbImage::from_fn(600, 900, |x, y| {
        image::Rgb([(x % 255) as u8, (y % 255) as u8, 128])
    })
    .save(folder.join("poster.png"))
    .unwrap();

    let library = libraries::create(
        &app.db,
        "Filme",
        "movies",
        &[root.to_string_lossy().into_owned()],
    )
    .await
    .unwrap();
    let queue = JobRuntime::new(app.db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    scan_library(&app.db, &queue, library.id).await.unwrap();
    let movie = &items::list_by_kind(&app.db, library.id, "movie")
        .await
        .unwrap()[0];
    refresh_local(&app.db, movie.id).await.unwrap();
    let image = &metadata::images(&app.db, movie.id).await.unwrap()[0];
    (library.id, image.id)
}

async fn raw_get(
    app: &TestApp,
    uri: &str,
    token: Option<&str>,
) -> (StatusCode, Option<String>, Vec<u8>) {
    let mut request = Request::get(uri);
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = app
        .router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, content_type, bytes)
}

#[tokio::test]
async fn images_are_resized_cached_and_access_controlled() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let media = tempfile::tempdir().unwrap();
    let (library_id, image_id) = movie_with_poster(&app, media.path()).await;

    // Signed URL (as the API will hand out): no token needed.
    let signer = UrlSigner::new(b"test-secret".to_vec());
    let url = signer.image_url(image_id, Some(300));
    let (status, content_type, bytes) = raw_get(&app, &url, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("image/jpeg"));
    let decoded = image::load_from_memory(&bytes).unwrap();
    assert_eq!(
        (decoded.width(), decoded.height()),
        (342, 513),
        "snapped to 342px, aspect kept"
    );

    // The first render stored size and blurhash.
    let stored = metadata::image(&app.db, image_id).await.unwrap().unwrap();
    assert_eq!((stored.width, stored.height), (Some(600), Some(900)));
    assert!(stored.blurhash.is_some_and(|h| h.len() > 6));

    // Second request is served from the cache.
    assert_eq!(raw_get(&app, &url, None).await.0, StatusCode::OK);

    // No signature, no token → 401; tampered signature → 401.
    assert_eq!(
        raw_get(&app, &format!("/api/v1/images/{image_id}"), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let tampered = url.replace("sig=", "sig=0");
    assert_eq!(
        raw_get(&app, &tampered, None).await.0,
        StatusCode::UNAUTHORIZED
    );

    // A token works too, if the user may see the library.
    assert_eq!(
        raw_get(
            &app,
            &format!("/api/v1/images/{image_id}?w=92"),
            Some(&admin)
        )
        .await
        .0,
        StatusCode::OK
    );
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
    app.request(
        Method::PUT,
        &format!("/api/v1/users/{kid}/libraries"),
        Some(json!({ "all_libraries": false, "library_ids": [] })),
        Some(&admin),
    )
    .await;
    let kid_token = app
        .post(
            "/api/v1/auth/login",
            credentials("Kind", PASSWORD, "kid"),
            None,
        )
        .await
        .json["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        raw_get(
            &app,
            &format!("/api/v1/images/{image_id}"),
            Some(&kid_token)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let _ = library_id;
}

#[tokio::test]
async fn metadata_settings_and_identify_without_key() {
    let app = app().await;
    let admin = app.setup_admin().await;

    let current = app.get("/api/v1/settings/metadata", Some(&admin)).await;
    assert_eq!(
        current.json,
        json!({ "has_tmdb_api_key": false, "language": "de-DE", "country": "DE" })
    );

    let bad = app
        .request(
            Method::PUT,
            "/api/v1/settings/metadata",
            Some(json!({ "language": "deutsch" })),
            Some(&admin),
        )
        .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    let updated = app
        .request(
            Method::PUT,
            "/api/v1/settings/metadata",
            Some(json!({ "language": "en-US", "country": "US" })),
            Some(&admin),
        )
        .await;
    assert_eq!(updated.json["language"], "en-US");
    assert_eq!(updated.json["country"], "US");

    // A movie to identify.
    let media = tempfile::tempdir().unwrap();
    let (library_id, _) = movie_with_poster(&app, media.path()).await;
    let movie = &items::list_by_kind(&app.db, library_id, "movie")
        .await
        .unwrap()[0];

    let identify = app
        .get(
            &format!("/api/v1/items/{}/identify", movie.id),
            Some(&admin),
        )
        .await;
    assert_eq!(identify.status, StatusCode::CONFLICT);
    assert_eq!(identify.json["code"], "tmdb_not_configured");

    let refresh = app
        .request(
            Method::POST,
            &format!("/api/v1/items/{}/refresh", movie.id),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(refresh.status, StatusCode::ACCEPTED);
    let jobs = app.get("/api/v1/jobs?state=queued", Some(&admin)).await;
    assert!(
        jobs.json
            .as_array()
            .unwrap()
            .iter()
            .any(|j| j["kind"] == "metadata.refresh")
    );

    let missing = app
        .request(
            Method::POST,
            "/api/v1/items/999999/refresh",
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}
