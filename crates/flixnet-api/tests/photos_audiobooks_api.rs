//! Photo and audiobook libraries: scan, analysis jobs and the API (needs ffmpeg).

mod common;

use std::path::Path;
use std::process::Command;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::{PASSWORD, TestApp, app, credentials};
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::{items, libraries};
use flixnet_library::scan::scan_library;
use http_body_util::BodyExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

fn ffmpeg(args: &[&str], output: &Path) {
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .arg(output)
        .status()
        .expect("ffmpeg on PATH");
    assert!(status.success(), "ffmpeg failed for {}", output.display());
}

async fn scan(app: &TestApp, name: &str, kind: &str, path: &Path) -> i64 {
    let library = libraries::create(&app.db, name, kind, &[path.to_string_lossy().into_owned()])
        .await
        .unwrap()
        .id;
    let queue = JobRuntime::new(app.db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    let report = scan_library(&app.db, &queue, library).await.unwrap();
    assert!(report.added > 0, "{report:?}");
    library
}

/// Status, content type and body of a (signed) URL fetched without a token.
async fn fetch(app: &TestApp, url: &str) -> (StatusCode, String, Vec<u8>) {
    let response = app
        .router
        .clone()
        .oneshot(Request::get(url).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let body = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, content_type, body)
}

async fn login(app: &TestApp, admin: &str, body: serde_json::Value) -> String {
    let name = body["name"].as_str().unwrap().to_owned();
    assert_eq!(
        app.post("/api/v1/users", body, Some(admin)).await.status,
        StatusCode::CREATED
    );
    app.post(
        "/api/v1/auth/login",
        credentials(&name, PASSWORD, &name),
        None,
    )
    .await
    .json["token"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn photo_libraries_have_a_timeline_albums_and_clips() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let root = tempfile::tempdir().unwrap();
    let photos = root.path().join("Fotos");
    std::fs::create_dir_all(photos.join("2023/Urlaub")).unwrap();
    image::RgbImage::from_pixel(60, 40, image::Rgb([200, 30, 30]))
        .save(photos.join("2023/Urlaub/Strand.png"))
        .unwrap();
    image::RgbImage::new(40, 60)
        .save(photos.join("2023/Urlaub/Berg.jpg"))
        .unwrap();
    image::RgbImage::new(20, 20)
        .save(photos.join("Wohnung.webp"))
        .unwrap();
    std::fs::write(photos.join("kaputt.jpg"), b"no picture").unwrap();
    std::fs::write(photos.join("notizen.txt"), b"ignored").unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=s=320x240:r=25:d=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
        &photos.join("2023/Urlaub/Clip.mp4"),
    );
    let library = scan(&app, "Fotos", "photos", &photos).await;

    // The clip job: probe, recording date, thumbnail.
    let clip = items::list_by_kind(&app.db, library, "video")
        .await
        .unwrap();
    assert_eq!(clip.len(), 1);
    let clip_file = flixnet_db::media::files_for_item(&app.db, clip[0].id)
        .await
        .unwrap()[0]
        .id;
    flixnet_library::photos::analyze_clip(
        &app.db,
        &flixnet_media::Ffprobe::new("ffprobe"),
        Path::new("ffmpeg"),
        &app.dir.path().join("cache"),
        clip_file,
        &CancellationToken::new(),
    )
    .await
    .unwrap();

    let page = app.get("/api/v1/photos", Some(&admin)).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.json);
    assert_eq!(page.json["total"], 5, "{}", page.json);
    let items = page.json["items"].as_array().unwrap();
    let by_title = |title: &str| items.iter().find(|p| p["title"] == title).unwrap().clone();
    let strand = by_title("Strand");
    assert_eq!(strand["kind"], "photo");
    assert_eq!(
        (strand["width"].as_i64(), strand["height"].as_i64()),
        (Some(60), Some(40))
    );
    assert!(strand["album_id"].is_i64());
    assert_eq!(by_title("Wohnung")["album_id"], json!(null));
    assert_eq!(
        by_title("kaputt")["thumb_url"],
        json!(null),
        "unreadable: no picture"
    );
    let clip = by_title("Clip");
    assert_eq!(clip["kind"], "video");
    assert_eq!(clip["width"], 320);
    assert!(
        (1900..=2100).contains(&clip["duration_ms"].as_i64().unwrap()),
        "{clip}"
    );
    // Newest first.
    let times: Vec<i64> = items
        .iter()
        .map(|p| p["taken_at"].as_i64().unwrap())
        .collect();
    assert!(times.windows(2).all(|w| w[0] >= w[1]), "{times:?}");

    // Thumbnails and the large view come from the image cache; clips stream.
    let (status, content_type, body) = fetch(&app, strand["thumb_url"].as_str().unwrap()).await;
    assert_eq!(
        (status, content_type.as_str()),
        (StatusCode::OK, "image/jpeg")
    );
    let decoded = image::load_from_memory(&body).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (60, 40));
    assert_eq!(
        fetch(&app, clip["thumb_url"].as_str().unwrap()).await.0,
        StatusCode::OK
    );
    assert_eq!(
        fetch(&app, clip["original_url"].as_str().unwrap()).await.0,
        StatusCode::OK
    );

    // Albums, months, one picture, one album.
    let albums = app.get("/api/v1/photos/albums", Some(&admin)).await;
    assert_eq!(albums.json.as_array().unwrap().len(), 1, "{}", albums.json);
    let album = &albums.json[0];
    assert_eq!(album["title"], "Urlaub");
    assert_eq!(album["photo_count"], 3);
    assert!(album["cover_url"].is_string());
    let in_album = app
        .get(
            &format!("/api/v1/photos?album_id={}&order=asc", album["id"]),
            Some(&admin),
        )
        .await;
    assert_eq!(in_album.json["total"], 3);
    let months = app.get("/api/v1/photos/months", Some(&admin)).await;
    assert_eq!(
        months
            .json
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["count"].as_i64().unwrap())
            .sum::<i64>(),
        5
    );
    let one = app
        .get(&format!("/api/v1/photos/{}", strand["id"]), Some(&admin))
        .await;
    assert_eq!(one.json["title"], "Strand");
    assert_eq!(
        app.get("/api/v1/photos/999", Some(&admin)).await.status,
        StatusCode::NOT_FOUND
    );
    // Photo items also show up in the generic browse with their kinds.
    let generic = app
        .get("/api/v1/items?kind=photo_album", Some(&admin))
        .await;
    assert_eq!(generic.json["total"], 1);

    // Parental controls: unrated pictures are hidden from a restricted profile unless
    // it allows unrated content; library access applies too.
    let kid = login(
        &app,
        &admin,
        json!({ "name": "Kind", "password": PASSWORD, "max_age_rating": 12 }),
    )
    .await;
    assert_eq!(app.get("/api/v1/photos", Some(&kid)).await.json["total"], 0);
    assert_eq!(
        app.get("/api/v1/photos/albums", Some(&kid)).await.json,
        json!([])
    );
    assert_eq!(
        app.get(&format!("/api/v1/photos/{}", strand["id"]), Some(&kid))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let teen = login(
        &app,
        &admin,
        json!({ "name": "Teen", "password": PASSWORD, "max_age_rating": 12, "allow_unrated": true }),
    )
    .await;
    assert_eq!(
        app.get("/api/v1/photos", Some(&teen)).await.json["total"],
        5
    );
    let guest = login(
        &app,
        &admin,
        json!({ "name": "Gast", "password": PASSWORD }),
    )
    .await;
    let guest_id = app.get("/api/v1/users/me", Some(&guest)).await.json["id"].clone();
    let access = app
        .request(
            Method::PUT,
            &format!("/api/v1/users/{guest_id}/libraries"),
            Some(json!({ "all_libraries": false, "library_ids": [] })),
            Some(&admin),
        )
        .await;
    assert!(access.status.is_success(), "{}", access.json);
    assert_eq!(
        app.get("/api/v1/photos", Some(&guest)).await.json["total"],
        0
    );

    // A rescan without changes writes nothing.
    let queue = JobRuntime::new(app.db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    let again = scan_library(&app.db, &queue, library).await.unwrap();
    assert_eq!((again.added, again.changed, again.unchanged), (0, 0, 5));
    // A deleted picture is missing, its album stays while others are left.
    std::fs::remove_file(photos.join("Wohnung.webp")).unwrap();
    assert_eq!(
        scan_library(&app.db, &queue, library)
            .await
            .unwrap()
            .missing,
        1
    );
}

#[tokio::test]
async fn audiobooks_have_parts_chapters_tags_and_resume() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let root = tempfile::tempdir().unwrap();
    let books = root.path().join("Hörbücher");
    let book = books.join("Ordner Autor/Reihe/01 - Ordnertitel");
    for (file, title) in [("01.mp3", "Teil Eins"), ("02.mp3", "Teil Zwei")] {
        ffmpeg(
            &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=40",
                "-c:a",
                "libmp3lame",
                "-b:a",
                "32k",
                "-metadata",
                "album=Das echte Buch",
                "-metadata",
                "artist=Echte Autorin",
                "-metadata",
                "composer=Der Sprecher",
                "-metadata",
                &format!("title={title}"),
                "-metadata",
                "date=2019",
            ],
            &book.join(file),
        );
    }
    image::RgbImage::new(30, 30)
        .save(book.join("cover.jpg"))
        .unwrap();
    // An m4b with chapters.
    let meta = root.path().join("chapters.txt");
    std::fs::write(
        &meta,
        ";FFMETADATA1\ntitle=Einzelbuch\nartist=Solo Autor\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=20000\ntitle=Anfang\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=20000\nEND=50000\ntitle=Ende\n",
    )
    .unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=330:duration=50",
            "-i",
            &meta.to_string_lossy(),
            "-map_metadata",
            "1",
            "-map_chapters",
            "1",
            "-map",
            "0",
            "-c:a",
            "aac",
            "-b:a",
            "32k",
            "-f",
            "mp4",
        ],
        &books.join("Einzelbuch.m4b"),
    );
    let library = scan(&app, "Hörbücher", "audiobooks", &books).await;
    let ffprobe = flixnet_media::Ffprobe::new("ffprobe");
    for item in items::list_by_kind(&app.db, library, "audiobook")
        .await
        .unwrap()
    {
        flixnet_library::audiobooks::analyze_book(
            &app.db,
            &ffprobe,
            Path::new("ffmpeg"),
            &app.dir.path().join("cache"),
            item.id,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    }

    let list = app.get("/api/v1/audiobooks", Some(&admin)).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.json);
    assert_eq!(list.json["total"], 2, "{}", list.json);
    let echte = list.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["title"] == "Das echte Buch")
        .unwrap()
        .clone();
    assert_eq!(echte["author"], "Echte Autorin");
    assert_eq!(echte["narrator"], "Der Sprecher");
    assert_eq!(echte["series_name"], "Reihe");
    assert_eq!(echte["series_index"], 1.0);
    assert_eq!(echte["year"], 2019);
    assert!(echte["cover_url"].is_string());
    let search = app
        .get("/api/v1/audiobooks?search=sprecher", Some(&admin))
        .await;
    assert_eq!(search.json["total"], 1);

    let id = echte["id"].as_i64().unwrap();
    let detail = app
        .get(&format!("/api/v1/audiobooks/{id}"), Some(&admin))
        .await;
    let parts = detail.json["parts"].as_array().unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0]["start_ms"], 0);
    let second_start = parts[1]["start_ms"].as_i64().unwrap();
    assert!((39_000..=41_000).contains(&second_start), "{second_start}");
    assert_eq!(detail.json["chapters"].as_array().unwrap().len(), 2);
    assert_eq!(detail.json["chapters"][1]["start_ms"], second_start);
    let total = detail.json["duration_ms"].as_i64().unwrap();
    assert!((79_000..=81_000).contains(&total));
    let (status, content_type, _) = fetch(&app, parts[1]["url"].as_str().unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("audio/"), "{content_type}");

    let single = list.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["author"] == "Solo Autor")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let single = app
        .get(&format!("/api/v1/audiobooks/{single}"), Some(&admin))
        .await;
    assert_eq!(single.json["title"], "Einzelbuch");
    let chapters: Vec<&str> = single.json["chapters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["title"].as_str().unwrap())
        .collect();
    assert_eq!(chapters, ["Anfang", "Ende"]);

    // Resume per book, across parts; finished near the end.
    let progress_url = format!("/api/v1/audiobooks/{id}/progress");
    let put = |position: i64| {
        app.request(
            Method::PUT,
            &progress_url,
            Some(json!({ "position_ms": position })),
            Some(&admin),
        )
    };
    assert_eq!(
        put(5_000).await.json["position_ms"],
        0,
        "too early to remember"
    );
    let saved = put(45_000).await;
    assert_eq!(
        saved.json,
        json!({ "position_ms": 45_000, "finished": false })
    );
    let again = app
        .get(&format!("/api/v1/audiobooks/{id}"), Some(&admin))
        .await;
    assert_eq!(again.json["position_ms"], 45_000);
    assert_eq!(put(-1).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        put(total - 5_000).await.json,
        json!({ "position_ms": 0, "finished": true })
    );
    let done = app
        .get(&format!("/api/v1/audiobooks/{id}"), Some(&admin))
        .await;
    assert_eq!(done.json["finished"], true);

    // Restricted profiles see no unrated books (as with any unrated content).
    let kid = login(
        &app,
        &admin,
        json!({ "name": "Kind", "password": PASSWORD, "max_age_rating": 6 }),
    )
    .await;
    assert_eq!(
        app.get("/api/v1/audiobooks", Some(&kid)).await.json["total"],
        0
    );
    assert_eq!(
        app.get(&format!("/api/v1/audiobooks/{id}"), Some(&kid))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.request(
            Method::PUT,
            &format!("/api/v1/audiobooks/{id}/progress"),
            Some(json!({ "position_ms": 45_000 })),
            Some(&kid),
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
}
