mod common;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use common::{TestApp, app};
use flixnet_api::signing::UrlSigner;
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::{libraries, media, users};
use flixnet_library::scan::scan_library;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

/// Raw request (no JSON decoding) with optional extra headers.
async fn raw(
    app: &TestApp,
    uri: &str,
    headers: &[(header::HeaderName, &str)],
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().uri(uri);
    for (name, value) in headers {
        builder = builder.header(name, *value);
    }
    let response = app
        .router
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes.to_vec())
}

struct Fixture {
    token: String,
    movie: i64,
    series: i64,
    content: Vec<u8>,
}

/// One movie (1000 known bytes) and one series, scanned; an admin signed in.
async fn library(app: &TestApp, root: &std::path::Path) -> Fixture {
    let content: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    let movie = root.join("Filme/Alpha (2020)/Alpha.2020.mkv");
    std::fs::create_dir_all(movie.parent().unwrap()).unwrap();
    std::fs::write(&movie, &content).unwrap();
    let episode = root.join("Serien/Dark/Staffel 1/Dark.S01E01.mkv");
    std::fs::create_dir_all(episode.parent().unwrap()).unwrap();
    std::fs::write(&episode, b"e").unwrap();

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
    let token = app.setup_admin().await;
    let movie = app.get("/api/v1/items?kind=movie", Some(&token)).await;
    let series = app.get("/api/v1/items?kind=series", Some(&token)).await;
    Fixture {
        movie: movie.json["items"][0]["id"].as_i64().unwrap(),
        series: series.json["items"][0]["id"].as_i64().unwrap(),
        token,
        content,
    }
}

#[tokio::test]
async fn direct_play_serves_ranges_with_signed_urls() {
    let app = app().await;
    let root = tempfile::tempdir().unwrap();
    let f = library(&app, root.path()).await;

    let info = app
        .post(
            &format!("/api/v1/items/{}/playback-info", f.movie),
            json!({}),
            Some(&f.token),
        )
        .await;
    assert_eq!(info.status, StatusCode::OK, "{}", info.json);
    let sources = info.json["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0]["file_name"], "Alpha.2020.mkv");
    assert_eq!(sources[0]["size"], 1000);
    let url = sources[0]["direct_url"].as_str().unwrap().to_owned();

    // Whole file, without any token: the signature is enough.
    let (status, headers, body) = raw(&app, &url, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, f.content);
    assert_eq!(headers[header::ACCEPT_RANGES], "bytes");
    assert_eq!(headers[header::CONTENT_TYPE], "video/x-matroska");

    // A byte range, as browsers request when seeking.
    let (status, headers, body) = raw(&app, &url, &[(header::RANGE, "bytes=100-199")]).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers[header::CONTENT_RANGE], "bytes 100-199/1000");
    assert_eq!(body, f.content[100..200]);

    // Open-ended range.
    let (status, _, body) = raw(&app, &url, &[(header::RANGE, "bytes=990-")]).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, f.content[990..]);

    let (status, _, _) = raw(&app, &url, &[(header::RANGE, "bytes=5000-6000")]).await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);

    // Series have nothing to play themselves.
    let none = app
        .post(
            &format!("/api/v1/items/{}/playback-info", f.series),
            json!({}),
            Some(&f.token),
        )
        .await;
    assert_eq!(none.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stream_urls_are_bound_to_user_and_access() {
    let app = app().await;
    let root = tempfile::tempdir().unwrap();
    let f = library(&app, root.path()).await;
    let info = app
        .post(
            &format!("/api/v1/items/{}/playback-info", f.movie),
            json!({}),
            Some(&f.token),
        )
        .await;
    let url = info.json["sources"][0]["direct_url"]
        .as_str()
        .unwrap()
        .to_owned();
    let file_id = info.json["sources"][0]["file_id"].as_i64().unwrap();

    // No signature, no token.
    let (status, _, _) = raw(&app, &format!("/api/v1/stream/{file_id}"), &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = raw(&app, "/api/v1/stream/9999", &[]).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "no hint whether a file exists"
    );
    // A token works instead of a signature.
    let bearer = format!("Bearer {}", f.token);
    let (status, _, _) = raw(
        &app,
        &format!("/api/v1/stream/{file_id}"),
        &[(header::AUTHORIZATION, &bearer)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Tampering: another user id or another file with the same signature.
    let other_user = url.replace("u=1&", "u=2&");
    assert_ne!(other_user, url);
    assert_eq!(
        raw(&app, &other_user, &[]).await.0,
        StatusCode::UNAUTHORIZED
    );
    let other_file = url.replace(&format!("/stream/{file_id}?"), "/stream/9999?");
    assert_eq!(
        raw(&app, &other_file, &[]).await.0,
        StatusCode::UNAUTHORIZED
    );

    // A user without access to the library: a correctly signed URL still fails.
    let user = users::create(
        &app.db,
        &users::NewUser {
            name: "Kind",
            password_hash: None,
            pin_hash: None,
            is_admin: false,
            is_hidden: false,
            max_age_rating: None,
        },
    )
    .await
    .unwrap();
    libraries::set_access(&app.db, user.id, false, &[])
        .await
        .unwrap();
    let signed = UrlSigner::new(b"test-secret".to_vec()).stream_url(file_id, user.id);
    assert_eq!(raw(&app, &signed, &[]).await.0, StatusCode::NOT_FOUND);
    // Granting access makes the same URL work.
    let library_id = libraries::list(&app.db).await.unwrap()[0].id;
    libraries::set_access(&app.db, user.id, false, &[library_id])
        .await
        .unwrap();
    assert_eq!(raw(&app, &signed, &[]).await.0, StatusCode::OK);
    // A deleted user's URLs stop working.
    users::delete(&app.db, user.id).await.unwrap();
    assert_eq!(raw(&app, &signed, &[]).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn playback_info_decides_per_device_profile() {
    let app = app().await;
    let root = tempfile::tempdir().unwrap();
    let f = library(&app, root.path()).await;
    let uri = format!("/api/v1/items/{}/playback-info", f.movie);
    let first = app.post(&uri, json!({}), Some(&f.token)).await;
    let file_id = first.json["sources"][0]["file_id"].as_i64().unwrap();

    // What a probe would have found: H.264 1080p, AC-3 5.1 (default) and AAC stereo.
    let stream = |idx, kind: &str, codec: &str| media::Stream {
        idx,
        kind: kind.into(),
        codec: Some(codec.into()),
        ..Default::default()
    };
    let streams = vec![
        media::Stream {
            width: Some(1920),
            height: Some(1080),
            bit_depth: Some(8),
            hdr: Some("sdr".into()),
            ..stream(0, "video", "h264")
        },
        media::Stream {
            channels: Some(6),
            is_default: true,
            ..stream(1, "audio", "ac3")
        },
        media::Stream {
            channels: Some(2),
            ..stream(2, "audio", "aac")
        },
        stream(3, "subtitle", "hdmv_pgs_subtitle"),
    ];
    media::store_probe(
        &app.db,
        file_id,
        &media::ProbeData {
            container: Some("matroska,webm".into()),
            duration_ms: Some(1000),
            bitrate: Some(5_000_000),
            streams,
            chapters: vec![],
        },
    )
    .await
    .unwrap();

    let browser = json!({
        "containers": ["mp4", "webm"],
        "video_codecs": [{ "codec": "h264", "max_height": 2160 }],
        "audio_codecs": [{ "codec": "aac", "max_channels": 6 }],
    });
    let plan = |body| async {
        let response = app.post(&uri, body, Some(&f.token)).await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.json);
        response.json["sources"][0]["plan"].clone()
    };

    let remux = plan(json!({ "device_profile": browser })).await;
    assert_eq!(remux["method"], "remux");
    assert_eq!(
        remux["reasons"],
        json!(["audio_codec_not_supported", "container_not_supported"])
    );
    assert_eq!(remux["video"]["action"], "copy");
    assert_eq!(remux["audio"]["action"], "transcode");
    assert_eq!(remux["audio"]["stream_index"], 1);

    // Picking the AAC track: no audio conversion, but browsers cannot switch tracks.
    let aac = plan(json!({ "device_profile": browser, "audio_stream_index": 2 })).await;
    assert_eq!(aac["method"], "remux");
    assert_eq!(aac["audio"]["action"], "copy");
    assert_eq!(
        aac["reasons"],
        json!(["audio_track_not_default", "container_not_supported"])
    );

    // PGS subtitles and a quality limit force a transcode.
    let burn = plan(
        json!({ "device_profile": browser, "subtitle_stream_index": 3, "max_bitrate": 3_000_000 }),
    )
    .await;
    assert_eq!(burn["method"], "transcode");
    assert_eq!(burn["subtitle"]["delivery"], "burn_in");
    assert_eq!(burn["video"]["codec"], "h264");
    // 3 Mbit/s minus the audio budget lands on the 720p step.
    assert_eq!(burn["video"]["height"], 720);

    // A device that plays Matroska with AC-3.
    let native = plan(json!({ "device_profile": {
        "containers": ["mkv"],
        "video_codecs": [{ "codec": "h264" }],
        "audio_codecs": [{ "codec": "ac3", "max_channels": 6 }],
    }}))
    .await;
    assert_eq!(native["method"], "direct_play");
    assert_eq!(native["reasons"], json!([]));
}
