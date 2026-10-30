//! tvheadend (tuner + EPG) and Schedules Direct (account, lineups, guide source)
//! through the API and the real job runtime, against wiremock.

mod common;

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use common::TestApp;
use flixnet_api::{AppState, Services, router};
use flixnet_core::Config;
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_core::scheduler::TaskRegistry;
use flixnet_db::Db;
use flixnet_library::images::ImageCache;
use flixnet_livetv::schedules_direct::{self, SdCredentials};
use flixnet_livetv::stream::iso8601;
use flixnet_livetv::{LiveSettings, LiveTvManager, jobs as livetv_jobs};
use flixnet_media::transcode::{TranscodeManager, TranscodeSettings};
use serde_json::{Value, json};
use wiremock::matchers::{basic_auth, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
            LiveSettings::default(),
        ),
    };
    let mut config = Config::default();
    config.paths.config_dir = dir.path().to_path_buf();
    let state = AppState::new(config, db.clone(), services);
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

async fn eventually(
    app: &TestApp,
    token: &str,
    uri: &str,
    check: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let response = app.get(uri, Some(token)).await;
        if response.status == StatusCode::OK && check(&response.json) {
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

#[tokio::test]
async fn tvheadend_tuner_brings_its_epg_and_matches_exactly() {
    let tvh = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/channel/grid"))
        .and(basic_auth("felix", "geheim"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "entries": [
                { "uuid": "aaa111", "name": "Das Erste HD", "number": 1, "enabled": true },
                { "uuid": "bbb222", "name": "Sender ohne Namen im EPG", "number": 2, "enabled": true }
            ]
        })))
        .mount(&tvh)
        .await;
    let now = flixnet_db::now_ms() / 1000;
    Mock::given(method("GET"))
        .and(path("/api/epg/events/grid"))
        .and(basic_auth("felix", "geheim"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "entries": [
                { "channelUuid": "aaa111", "start": now - 600, "stop": now + 1200, "title": "Tagesschau", "ageRating": 6 },
                { "channelUuid": "bbb222", "start": now - 600, "stop": now + 1200, "title": "Etwas anderes" }
            ],
            "totalCount": 2
        })))
        .mount(&tvh)
        .await;

    let (app, _jobs) = app().await;
    let admin = app.setup_admin().await;
    let created = app
        .post(
            "/api/v1/livetv/tuners",
            json!({ "kind": "tvheadend", "name": "Keller", "url": tvh.uri(), "username": "felix", "password": "geheim" }),
            Some(&admin),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    assert_eq!(created.json["kind"], "tvheadend");
    let sources = eventually(&app, &admin, "/api/v1/livetv/epg-sources", |s| {
        s[0]["program_count"] == 2
    })
    .await;
    assert_eq!(sources[0]["kind"], "tvheadend");
    assert_eq!(sources[0]["tuner_id"], created.json["id"]);
    assert_eq!(sources[0]["name"], "Keller (EPG)");

    let channels = eventually(&app, &admin, "/api/v1/livetv/channels", |c| {
        c.as_array().unwrap().len() == 2
            && c.as_array().unwrap().iter().all(|c| !c["epg"].is_null())
    })
    .await;
    for channel in channels.as_array().unwrap() {
        assert_eq!(channel["epg"]["score"], 1.0, "matched by uuid: {channel}");
    }
    assert_eq!(channels[0]["now"]["title"], "Tagesschau");
    assert_eq!(channels[0]["now"]["rating"], "FSK 6");
    assert_eq!(channels[1]["now"]["title"], "Etwas anderes");

    // Removing the tuner removes its guide.
    let removed = app
        .request(
            axum::http::Method::DELETE,
            &format!("/api/v1/livetv/tuners/{}", created.json["id"]),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.get("/api/v1/livetv/epg-sources", Some(&admin))
            .await
            .json,
        json!([])
    );
}

#[tokio::test]
async fn schedules_direct_account_lineups_and_guide() {
    let sd = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(wiremock::matchers::body_partial_json(json!({
            "username": "felix", "password": schedules_direct::password_hash("geheim")
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "code": 0, "token": "t" })))
        .mount(&sd)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({ "code": 4003, "message": "Invalid user" })),
        )
        .mount(&sd)
        .await;
    Mock::given(method("GET"))
        .and(path("/lineups"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0, "lineups": [{ "lineup": "DEU-1000097-DEFAULT", "name": "Kabel Berlin" }]
        })))
        .mount(&sd)
        .await;
    Mock::given(method("GET"))
        .and(path("/lineups/DEU-1000097-DEFAULT"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "map": [{ "stationID": "1", "channel": "1" }],
            "stations": [{ "stationID": "1", "name": "Das Erste", "callsign": "ARD" }]
        })))
        .mount(&sd)
        .await;
    let start = iso8601(flixnet_db::now_ms() - 600_000);
    Mock::given(method("POST"))
        .and(path("/schedules"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "stationID": "1", "programs": [{ "programID": "EP000000010001", "airDateTime": start, "duration": 3600 }] }
        ])))
        .mount(&sd)
        .await;
    Mock::given(method("POST"))
        .and(path("/programs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "programID": "EP000000010001", "titles": [{ "title120": "Sportschau" }] }
        ])))
        .mount(&sd)
        .await;

    let (app, _jobs) = app().await;
    let admin = app.setup_admin().await;
    // The API base of the test server (not settable through the API).
    flixnet_db::settings::set(
        &app.db,
        schedules_direct::SETTINGS_KEY,
        &SdCredentials {
            base_url: Some(sd.uri()),
            ..SdCredentials::default()
        },
    )
    .await
    .unwrap();

    let wrong = app
        .request(
            axum::http::Method::PUT,
            "/api/v1/livetv/schedules-direct",
            Some(json!({ "username": "felix", "password": "falsch" })),
            Some(&admin),
        )
        .await;
    assert_eq!(
        wrong.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        wrong.json
    );
    let saved = app
        .request(
            axum::http::Method::PUT,
            "/api/v1/livetv/schedules-direct",
            Some(json!({ "username": "felix", "password": "geheim" })),
            Some(&admin),
        )
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.json);
    let account = app
        .get("/api/v1/livetv/schedules-direct", Some(&admin))
        .await
        .json;
    assert_eq!(
        account,
        json!({ "username": "felix", "has_password": true })
    );
    let stored: SdCredentials = flixnet_db::settings::get(&app.db, schedules_direct::SETTINGS_KEY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.password_sha1,
        schedules_direct::password_hash("geheim")
    );

    let lineups = app
        .get("/api/v1/livetv/schedules-direct/lineups", Some(&admin))
        .await;
    assert_eq!(lineups.json[0]["lineup"], "DEU-1000097-DEFAULT");

    let invalid = app
        .post(
            "/api/v1/livetv/epg-sources",
            json!({ "name": "SD", "url": "../etc", "kind": "schedules_direct" }),
            Some(&admin),
        )
        .await;
    assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);
    let source = app
        .post(
            "/api/v1/livetv/epg-sources",
            json!({ "name": "Kabel Berlin", "url": "DEU-1000097-DEFAULT", "kind": "schedules_direct" }),
            Some(&admin),
        )
        .await;
    assert_eq!(source.status, StatusCode::CREATED, "{}", source.json);
    let sources = eventually(&app, &admin, "/api/v1/livetv/epg-sources", |s| {
        s[0]["last_refresh_at"].is_i64()
    })
    .await;
    assert!(sources[0]["last_error"].is_null(), "{sources}");
    assert_eq!(sources[0]["kind"], "schedules_direct");
    assert_eq!(sources[0]["program_count"], 1);
    assert_eq!(sources[0]["channel_count"], 1);
    let channels = app
        .get("/api/v1/livetv/epg-channels", Some(&admin))
        .await
        .json;
    assert_eq!(channels[0]["names"], json!(["Das Erste", "ARD", "1"]));
}
