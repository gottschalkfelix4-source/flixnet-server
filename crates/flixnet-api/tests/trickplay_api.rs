//! Trickplay sheets and chapter images: made by the background jobs (real ffmpeg),
//! served by the API with signed URLs.

mod common;

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use common::{TestApp, app};
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::{libraries, media, trickplay};
use flixnet_library::scan::scan_library;
use flixnet_library::trickplay::{GenerateChapterImages, GenerateTrickplay, queue_thumbnails};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn raw(app: &TestApp, uri: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app
        .router
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes.to_vec())
}

/// 35 s of 640×360 test picture with three chapters, scanned and probed.
async fn chaptered_movie(app: &TestApp, root: &std::path::Path) -> i64 {
    let folder = root.join("Filme/Kapitel (2020)");
    std::fs::create_dir_all(&folder).unwrap();
    let metadata = root.join("chapters.txt");
    std::fs::write(
        &metadata,
        ";FFMETADATA1\n\
         [CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=10000\ntitle=Anfang\n\
         [CHAPTER]\nTIMEBASE=1/1000\nSTART=10000\nEND=25000\ntitle=Mitte\n\
         [CHAPTER]\nTIMEBASE=1/1000\nSTART=25000\nEND=35000\ntitle=Ende\n",
    )
    .unwrap();
    let status = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc2=s=640x360:r=25:d=35"])
        .arg("-i")
        .arg(&metadata)
        .args(["-map", "0", "-map_metadata", "1", "-map_chapters", "1"])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-g", "50"])
        .arg(folder.join("Kapitel.2020.mkv"))
        .status()
        .expect("ffmpeg on PATH");
    assert!(status.success());
    let library = libraries::create(
        &app.db,
        "Filme",
        "movies",
        &[root.join("Filme").to_string_lossy().into_owned()],
    )
    .await
    .unwrap()
    .id;
    let queue = JobRuntime::new(app.db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    scan_library(&app.db, &queue, library).await.unwrap();
    let file_id = media::file_states(&app.db, library).await.unwrap()[0].id;
    flixnet_library::jobs::probe_file(&app.db, &flixnet_media::Ffprobe::new("ffprobe"), file_id)
        .await
        .unwrap();
    file_id
}

fn jpeg_size(bytes: &[u8]) -> (u32, u32) {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).unwrap();
    (image.width(), image.height())
}

#[tokio::test]
async fn sheets_and_chapter_images_are_made_and_served_signed() {
    let app = app().await;
    let root = tempfile::tempdir().unwrap();
    let file_id = chaptered_movie(&app, root.path()).await;
    let token = app.setup_admin().await;

    // Nothing made yet: no manifest, chapters without images.
    let manifest = format!("/api/v1/files/{file_id}/trickplay");
    let chapters_url = format!("/api/v1/files/{file_id}/chapters");
    assert_eq!(
        app.get(&manifest, Some(&token)).await.status,
        StatusCode::NOT_FOUND
    );
    let chapters = app.get(&chapters_url, Some(&token)).await;
    assert_eq!(chapters.status, StatusCode::OK);
    assert_eq!(
        chapters.json,
        json!([
            { "index": 0, "start_ms": 0, "end_ms": 10000, "title": "Anfang", "image_url": null },
            { "index": 1, "start_ms": 10000, "end_ms": 25000, "title": "Mitte", "image_url": null },
            { "index": 2, "start_ms": 25000, "end_ms": 35000, "title": "Ende", "image_url": null },
        ])
    );

    // The jobs, as the server runs them after a probe (cpu pool).
    let cache = app.dir.path().join("cache");
    let jobs = JobRuntime::new(app.db.clone(), EventBus::new())
        .register(GenerateTrickplay {
            ffmpeg: "ffmpeg".into(),
            cache_dir: cache.clone(),
        })
        .register(GenerateChapterImages {
            ffmpeg: "ffmpeg".into(),
            cache_dir: cache.clone(),
        })
        .start()
        .await
        .unwrap();
    queue_thumbnails(jobs.handle().queue(), file_id, 100)
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let sheets = trickplay::get(&app.db, file_id).await.unwrap();
        let images = trickplay::chapters(&app.db, file_id).await.unwrap();
        if sheets.is_some() && images.iter().all(|c| c.image_at.is_some()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "thumbnails were not made in time"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    jobs.shutdown(Duration::from_secs(5)).await;
    assert!(
        !cache.join("trickplay/1.partial").exists(),
        "scratch dir cleaned up"
    );

    // 35 s → 4 thumbnails of 320×180 on one 10×10 sheet.
    let response = app.get(&manifest, Some(&token)).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.json);
    let json = &response.json;
    assert_eq!(
        (
            &json["interval_ms"],
            &json["tile_width"],
            &json["tile_height"],
            &json["columns"],
            &json["rows"],
            &json["thumbnails"]
        ),
        (
            &json!(10000),
            &json!(320),
            &json!(180),
            &json!(10),
            &json!(10),
            &json!(4)
        )
    );
    let sheets: Vec<String> = serde_json::from_value(json["sheets"].clone()).unwrap();
    assert_eq!(sheets.len(), 1);
    let sheet = &sheets[0];
    assert!(
        sheet.starts_with(&format!("/api/v1/files/{file_id}/trickplay/0.jpg?u=1&")),
        "{sheet}"
    );

    // The signed URL works without a token (CSS backgrounds cannot send one).
    let (status, headers, body) = raw(&app, sheet).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/jpeg");
    assert_eq!(jpeg_size(&body), (3200, 1800));

    // Tampering, other users, missing sheets and missing auth are rejected.
    let tampered = sheet.replace("sig=", "sig=00");
    assert_eq!(raw(&app, &tampered).await.0, StatusCode::UNAUTHORIZED);
    let other_user = sheet.replace("?u=1&", "?u=2&");
    assert_eq!(raw(&app, &other_user).await.0, StatusCode::UNAUTHORIZED);
    let chapter_signature = sheet.replace("/trickplay/0.jpg", "/chapters/0.jpg");
    assert_eq!(
        raw(&app, &chapter_signature).await.0,
        StatusCode::UNAUTHORIZED,
        "a trickplay signature is no chapter signature"
    );
    assert_eq!(
        raw(&app, &sheet.replace("/0.jpg", "/1.jpg")).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        raw(&app, &sheet.replace("/0.jpg", "/..%2F0.jpg")).await.0,
        StatusCode::NOT_FOUND
    );
    let unsigned = format!("/api/v1/files/{file_id}/trickplay/0.jpg");
    assert_eq!(raw(&app, &unsigned).await.0, StatusCode::UNAUTHORIZED);
    // With a token instead of a signature.
    let with_token = app.get(&format!("{unsigned}?api_key={token}"), None).await;
    assert_eq!(with_token.status, StatusCode::OK);

    // Every chapter got an image, 320 px wide, served signed as well.
    let chapters = app.get(&chapters_url, Some(&token)).await.json;
    let urls: Vec<&str> = chapters
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["image_url"].as_str().unwrap())
        .collect();
    assert_eq!(urls.len(), 3);
    for (index, url) in urls.iter().enumerate() {
        assert!(url.starts_with(&format!(
            "/api/v1/files/{file_id}/chapters/{index}.jpg?u=1&"
        )));
        let (status, _, body) = raw(&app, url).await;
        assert_eq!(status, StatusCode::OK, "{url}");
        assert_eq!(jpeg_size(&body), (320, 180));
    }
    assert_eq!(
        raw(&app, &urls[0].replace("/0.jpg", "/7.jpg")).await.0,
        StatusCode::NOT_FOUND
    );

    // A new probe (the file changed) drops the sheets and the chapter images.
    flixnet_library::jobs::probe_file(&app.db, &flixnet_media::Ffprobe::new("ffprobe"), file_id)
        .await
        .unwrap();
    assert_eq!(
        app.get(&manifest, Some(&token)).await.status,
        StatusCode::NOT_FOUND
    );
    let after: Value = app.get(&chapters_url, Some(&token)).await.json;
    assert!(
        after
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["image_url"].is_null())
    );
}

#[tokio::test]
async fn backfill_queues_what_is_missing() {
    let app = app().await;
    let root = tempfile::tempdir().unwrap();
    let file_id = chaptered_movie(&app, root.path()).await;
    assert_eq!(
        trickplay::files_without_trickplay(&app.db, 10)
            .await
            .unwrap(),
        [file_id]
    );
    assert_eq!(
        trickplay::files_without_chapter_images(&app.db, 10)
            .await
            .unwrap(),
        [file_id]
    );
    trickplay::mark_chapter_images(&app.db, file_id, &[0, 1, 2])
        .await
        .unwrap();
    assert!(
        trickplay::files_without_chapter_images(&app.db, 10)
            .await
            .unwrap()
            .is_empty()
    );
}
