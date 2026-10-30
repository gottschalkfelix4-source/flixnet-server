//! Live TV API against the fake source: tuners and guide sources (through the real
//! job runtime), channels, guide, matching, search and live HLS playback.

mod common;
#[path = "../../flixnet-livetv/tests/fake/mod.rs"]
mod fake;

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use common::{PASSWORD, Response, TestApp, credentials};
use flixnet_api::{AppState, Services, router};
use flixnet_core::Config;
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_core::scheduler::TaskRegistry;
use flixnet_db::Db;
use flixnet_library::images::ImageCache;
use flixnet_livetv::{LiveSettings, LiveTvManager, jobs as livetv_jobs};
use flixnet_media::transcode::{TranscodeManager, TranscodeSettings};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Like `common::app`, but with running job workers for imports and refreshes and
/// quick live settings.
async fn app() -> (TestApp, flixnet_core::jobs::RunningJobs) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    let events = EventBus::new();
    let runtime = JobRuntime::new(db.clone(), events.clone())
        .register(livetv_jobs::ImportChannels {
            http: flixnet_livetv::http::client(),
        })
        .register(livetv_jobs::RefreshGuide {
            http: flixnet_livetv::http::client(),
            cache_dir: dir.path().join("cache/epg"),
        });
    let jobs = runtime.handle();
    let running = runtime.start().await.unwrap();
    let services = Services {
        events,
        jobs,
        tasks: TaskRegistry::new(livetv_jobs::tasks()),
        images: ImageCache::new(dir.path().join("cache/images")),
        url_secret: b"test-secret".to_vec(),
        watcher: None,
        transcoder: TranscodeManager::new(
            "ffmpeg",
            dir.path().join("transcode"),
            TranscodeSettings::default(),
        ),
        ffprobe: flixnet_media::Ffprobe::new("ffprobe"),
        subtitles: flixnet_media::subtitles::SubtitleCache::new(
            "ffmpeg",
            dir.path().join("cache/subtitles"),
            Duration::from_secs(60),
            2,
        ),
        livetv: LiveTvManager::new(
            "ffmpeg",
            "ffprobe",
            dir.path().join("transcode/live"),
            LiveSettings {
                segment_secs: 1,
                window: 4,
                min_segments: 1,
                ..LiveSettings::default()
            },
        ),
    };
    let state = AppState::new(Config::default(), db.clone(), services);
    let router = router(state.clone());
    (
        TestApp {
            dir,
            router,
            db,
            state,
        },
        running,
    )
}

/// Raw GET without a token (HLS URLs), returning status and body text.
async fn raw_get(app: &TestApp, uri: &str) -> (StatusCode, String) {
    let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn eventually(
    app: &TestApp,
    token: &str,
    uri: &str,
    check: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let response = app.get(uri, Some(token)).await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.json);
        if check(&response.json) {
            return response.json;
        }
        assert!(
            Instant::now() < deadline,
            "timed out on {uri}: {}",
            response.json
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn send(app: &TestApp, method: Method, uri: &str, body: Value, token: &str) -> Response {
    app.request(method, uri, Some(body), Some(token)).await
}

fn channel<'a>(channels: &'a Value, name: &str) -> &'a Value {
    channels
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no channel {name} in {channels}"))
}

/// A browser like Chromium: H.264 and AAC, no MPEG-2.
fn profile() -> Value {
    json!({
        "containers": ["mp4", "webm"],
        "video_codecs": [{ "codec": "h264", "max_width": 3840, "max_height": 2160 }],
        "audio_codecs": [{ "codec": "aac", "max_channels": 6 }, { "codec": "opus" }],
    })
}

/// Sets up the fake source's playlist and guide and waits for both jobs.
async fn with_channels_and_guide(
    app: &TestApp,
    admin: &str,
    fake: &fake::FakeSource,
    tuners: Option<i64>,
) {
    let mut body = json!({ "kind": "m3u", "name": "Fake-IPTV", "url": fake.url("/playlist.m3u") });
    if let Some(count) = tuners {
        body["tuner_count"] = json!(count);
    }
    let created = app.post("/api/v1/livetv/tuners", body, Some(admin)).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    eventually(app, admin, "/api/v1/livetv/tuners", |t| {
        t[0]["channel_count"] == 4
    })
    .await;

    let source = app
        .post(
            "/api/v1/livetv/epg-sources",
            json!({ "name": "Fake-EPG", "url": fake.url("/guide.xml.gz") }),
            Some(admin),
        )
        .await;
    assert_eq!(source.status, StatusCode::CREATED, "{}", source.json);
    eventually(app, admin, "/api/v1/livetv/epg-sources", |s| {
        s[0]["program_count"] == 48
    })
    .await;
    // Matching runs after the refresh job's writes.
    eventually(app, admin, "/api/v1/livetv/channels", |c| {
        c.as_array()
            .unwrap()
            .iter()
            .filter(|c| !c["epg"].is_null())
            .count()
            == 3
    })
    .await;
}

#[tokio::test]
async fn tuners_guide_sources_matching_and_search() {
    let fake = fake::start().await;
    let (app, _jobs) = app().await;
    let admin = app.setup_admin().await;

    // Validation.
    for body in [
        json!({ "kind": "xtream", "name": "X", "url": fake.base, "username": "max" }),
        json!({ "kind": "m3u", "name": "X", "url": "relative/list.m3u" }),
        json!({ "kind": "m3u", "name": "X", "url": fake.url("/playlist.m3u"), "tuner_count": 100 }),
        json!({ "kind": "m3u", "name": " ", "url": fake.url("/playlist.m3u") }),
    ] {
        let response = app
            .post("/api/v1/livetv/tuners", body.clone(), Some(&admin))
            .await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }

    with_channels_and_guide(&app, &admin, &fake, None).await;
    let tuners = app.get("/api/v1/livetv/tuners", Some(&admin)).await.json;
    assert_eq!(tuners[0]["kind"], "m3u");
    assert_eq!(tuners[0]["tuner_count"], 0, "unlimited for playlists");
    assert!(tuners[0]["last_error"].is_null());

    // Channels in guide order with now/next from the guide.
    let channels = app.get("/api/v1/livetv/channels", Some(&admin)).await.json;
    let names: Vec<_> = channels
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].clone())
        .collect();
    assert_eq!(names, ["Das Erste HD", "ZDF HD", "DE: arte", "Kaputt TV"]);
    let ard = channel(&channels, "Das Erste HD");
    assert_eq!(ard["now"]["title"], "Tagesschau");
    assert!(ard["next"]["start_ms"].as_i64().unwrap() >= ard["now"]["end_ms"].as_i64().unwrap());
    assert_eq!(ard["epg"]["kind"], "auto");
    assert_eq!(ard["epg"]["epg_channel_id"], "daserste.de");
    assert_eq!(ard["epg"]["score"], 1.0);
    assert_eq!(
        channel(&channels, "DE: arte")["epg"]["epg_channel_id"],
        "arte.de"
    );
    assert!(channel(&channels, "Kaputt TV")["epg"].is_null());

    // The guide pages through channels within a time window.
    let now = flixnet_db::now_ms();
    let guide = app
        .get(
            &format!(
                "/api/v1/livetv/guide?start_ms={}&end_ms={}&offset=1&limit=2",
                now - 3_600_000,
                now + 3 * 3_600_000
            ),
            Some(&admin),
        )
        .await;
    assert_eq!(guide.status, StatusCode::OK, "{}", guide.json);
    assert_eq!(guide.json["total"], 4);
    let rows = guide.json["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["channel_id"], channel(&channels, "ZDF HD")["id"]);
    // 1 h back + 3 h ahead in half-hour slots: 8 or 9 programs depending on the clock.
    let count = rows[0]["programs"].as_array().unwrap().len();
    assert!((8..=9).contains(&count), "{count}");
    let too_long = app
        .get(
            &format!("/api/v1/livetv/guide?start_ms=0&end_ms={}", 49 * 3_600_000),
            Some(&admin),
        )
        .await;
    assert_eq!(too_long.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Search and program details.
    let found = app
        .get("/api/v1/livetv/programs?q=tagessch", Some(&admin))
        .await
        .json;
    assert_eq!(found.as_array().unwrap().len(), 1);
    assert_eq!(found[0]["channel_id"], ard["id"]);
    let id = found[0]["id"].as_i64().unwrap();
    let program = app
        .get(&format!("/api/v1/livetv/programs/{id}"), Some(&admin))
        .await;
    assert_eq!(program.json["title"], "Tagesschau");
    assert_eq!(program.json["categories"], json!(["Unterhaltung"]));

    // Manual matching: assign, disable, hand back.
    let epg_channels = app
        .get("/api/v1/livetv/epg-channels", Some(&admin))
        .await
        .json;
    assert_eq!(epg_channels.as_array().unwrap().len(), 3);
    let source_id = epg_channels[0]["epg_source_id"].as_i64().unwrap();
    let kaputt = channel(&channels, "Kaputt TV")["id"].as_i64().unwrap();
    let uri = format!("/api/v1/livetv/channels/{kaputt}/epg");
    let manual = send(
        &app,
        Method::PUT,
        &uri,
        json!({ "mode": "manual", "epg_source_id": source_id, "epg_channel_id": "arte.de" }),
        &admin,
    )
    .await;
    assert_eq!(manual.status, StatusCode::OK, "{}", manual.json);
    assert_eq!(manual.json["epg"]["kind"], "manual");
    assert_eq!(manual.json["epg"]["epg_channel_id"], "arte.de");
    let unknown = send(
        &app,
        Method::PUT,
        &uri,
        json!({ "mode": "manual", "epg_source_id": source_id, "epg_channel_id": "gibts.nicht" }),
        &admin,
    )
    .await;
    assert_eq!(unknown.status, StatusCode::UNPROCESSABLE_ENTITY);
    // The matcher leaves manual choices alone.
    let rematch = app
        .post("/api/v1/livetv/channels/match", json!({}), Some(&admin))
        .await;
    assert_eq!(rematch.json["matched"], 3, "manual ones are not counted");
    let channels = app.get("/api/v1/livetv/channels", Some(&admin)).await.json;
    let shown = channel(&channels, "Kaputt TV")["now"]["title"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(shown.starts_with("Arte"), "{shown}");
    let none = send(&app, Method::PUT, &uri, json!({ "mode": "none" }), &admin).await;
    assert!(none.json["epg"].is_null());
    assert_eq!(none.json["epg_disabled"], true);
    let auto = send(&app, Method::PUT, &uri, json!({ "mode": "auto" }), &admin).await;
    assert!(
        auto.json["epg"].is_null(),
        "no automatic match for this name"
    );
    assert_eq!(auto.json["epg_disabled"], false);

    // Hiding a channel.
    let hide = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/livetv/channels/{kaputt}"),
        json!({ "enabled": false }),
        &admin,
    )
    .await;
    assert_eq!(hide.status, StatusCode::NO_CONTENT);
    let visible = app.get("/api/v1/livetv/channels", Some(&admin)).await.json;
    assert_eq!(visible.as_array().unwrap().len(), 3);
    let all = app
        .get(
            "/api/v1/livetv/channels?include_disabled=true",
            Some(&admin),
        )
        .await
        .json;
    assert_eq!(all.as_array().unwrap().len(), 4);

    // HDHomeRun by address.
    let found = app
        .post(
            "/api/v1/livetv/tuners/discover",
            json!({ "address": fake.base }),
            Some(&admin),
        )
        .await;
    assert_eq!(found.status, StatusCode::OK, "{}", found.json);
    assert_eq!(found.json[0]["device_id"], "1013ABCD");
    assert_eq!(found.json[0]["tuner_count"], 2);
    assert_eq!(found.json[0]["model"], "HDTC-2US");
    assert_eq!(found.json[0]["configured"], false);

    // Members watch, but do not manage.
    let member = app
        .post(
            "/api/v1/users",
            json!({ "name": "Anna", "password": PASSWORD }),
            Some(&admin),
        )
        .await;
    assert_eq!(member.status, StatusCode::CREATED, "{}", member.json);
    let login = app
        .post(
            "/api/v1/auth/login",
            credentials("Anna", PASSWORD, "anna-tv"),
            None,
        )
        .await;
    let anna = login.json["token"].as_str().unwrap().to_owned();
    assert_eq!(
        app.get("/api/v1/livetv/tuners", Some(&anna)).await.status,
        StatusCode::FORBIDDEN
    );
    let theirs = app
        .get("/api/v1/livetv/channels?include_disabled=true", Some(&anna))
        .await
        .json;
    assert_eq!(
        theirs.as_array().unwrap().len(),
        3,
        "members never see hidden channels"
    );
    assert!(theirs[0]["tvg_id"].is_null());

    // Removing the guide source unmatches the channels.
    let removed = app
        .request(
            Method::DELETE,
            &format!("/api/v1/livetv/epg-sources/{source_id}"),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    let channels = app.get("/api/v1/livetv/channels", Some(&admin)).await.json;
    assert!(
        channels
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["epg"].is_null() && c["now"].is_null())
    );
}

#[tokio::test]
async fn watching_streams_live_hls_shared_per_channel() {
    let fake = fake::start().await;
    let (app, _jobs) = app().await;
    let admin = app.setup_admin().await;
    with_channels_and_guide(&app, &admin, &fake, Some(1)).await;
    let channels = app.get("/api/v1/livetv/channels", Some(&admin)).await.json;
    let id = |name: &str| channel(&channels, name)["id"].as_i64().unwrap();
    let watch = |channel: i64| {
        let app = &app;
        let admin = admin.clone();
        async move {
            app.post(
                &format!("/api/v1/livetv/channels/{channel}/watch"),
                json!({ "device_profile": profile() }),
                Some(&admin),
            )
            .await
        }
    };

    // H.264/AAC: copied.
    let first = watch(id("Das Erste HD")).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.json);
    assert_eq!(first.json["plan"]["method"], "remux");
    assert_eq!(first.json["plan"]["video"]["action"], "copy");
    assert_eq!(first.json["plan"]["audio"]["action"], "copy");
    assert_eq!(first.json["shared"], false);
    let playlist_url = first.json["playlist_url"].as_str().unwrap().to_owned();

    // The playlist and segments need no token: the session id is the capability.
    let (status, playlist) = raw_get(&app, &playlist_url).await;
    assert_eq!(status, StatusCode::OK);
    assert!(playlist.starts_with("#EXTM3U"), "{playlist}");
    let segment = playlist.lines().find(|l| l.ends_with(".ts")).unwrap();
    let segment_url = playlist_url.replace("live.m3u8", segment);
    let (status, bytes) = raw_get(&app, &segment_url).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!bytes.is_empty());
    assert_eq!(
        raw_get(&app, &playlist_url.replace("live.m3u8", "ffmpeg.log"))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        raw_get(
            &app,
            "/api/v1/livetv/sessions/00000000000000000000000000000000/live.m3u8"
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    // A second viewer shares the stream; another channel finds the only tuner taken.
    let second = watch(id("Das Erste HD")).await;
    assert_eq!(second.json["shared"], true);
    assert_eq!(fake.open_streams(), 1, "one upstream for both viewers");
    let busy = watch(id("ZDF HD")).await;
    assert_eq!(
        busy.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        busy.json
    );
    assert_eq!(busy.json["code"], "tuners_busy");
    let tuners = app.get("/api/v1/livetv/tuners", Some(&admin)).await.json;
    assert_eq!(tuners[0]["tuners_in_use"], 1);
    let streams = app.get("/api/v1/livetv/streams", Some(&admin)).await.json;
    assert_eq!(streams.as_array().unwrap().len(), 1);
    assert_eq!(streams[0]["channel_name"], "Das Erste HD");
    assert_eq!(streams[0]["viewers"].as_array().unwrap().len(), 2);

    // Both viewers leave: the stream stops.
    for session in [&first.json["session_id"], &second.json["session_id"]] {
        let stop = app
            .request(
                Method::DELETE,
                &format!("/api/v1/livetv/sessions/{}", session.as_str().unwrap()),
                None,
                Some(&admin),
            )
            .await;
        assert_eq!(stop.status, StatusCode::NO_CONTENT);
    }
    assert_eq!(
        app.get("/api/v1/livetv/streams", Some(&admin)).await.json,
        json!([])
    );
    assert_eq!(raw_get(&app, &playlist_url).await.0, StatusCode::NOT_FOUND);

    // MPEG-2/MP2 (probed on first use): transcoded to H.264/AAC.
    let arte = watch(id("DE: arte")).await;
    assert_eq!(arte.status, StatusCode::OK, "{}", arte.json);
    assert_eq!(arte.json["plan"]["method"], "transcode");
    assert_eq!(arte.json["plan"]["video"]["action"], "transcode");
    assert_eq!(arte.json["plan"]["video"]["codec"], "h264");
    assert_eq!(arte.json["plan"]["audio"]["action"], "transcode");
    assert!(
        arte.json["plan"]["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("video_codec_not_supported"))
    );
    let stop = app
        .request(
            Method::DELETE,
            &format!(
                "/api/v1/livetv/sessions/{}",
                arte.json["session_id"].as_str().unwrap()
            ),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(stop.status, StatusCode::NO_CONTENT);

    // A dead stream: a clean 502.
    let broken = watch(id("Kaputt TV")).await;
    assert_eq!(broken.status, StatusCode::BAD_GATEWAY, "{}", broken.json);
    assert_eq!(broken.json["code"], "stream_failed");
    assert_eq!(
        app.get("/api/v1/livetv/streams", Some(&admin)).await.json,
        json!([])
    );
}
