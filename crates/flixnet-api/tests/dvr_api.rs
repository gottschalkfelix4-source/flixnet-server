//! DVR and Live TV parental controls through the API, against the fake source:
//! recording programs and series, conflicts against the tuner count and priorities,
//! cancelling, a real recording that becomes a library item, channel logos through
//! the image cache, and profiles barred from Live TV or restricted by age.

mod common;
#[path = "../../flixnet-livetv/tests/fake/mod.rs"]
mod fake;

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use common::{PASSWORD, TestApp, credentials};
use flixnet_api::{AppState, Services, router};
use flixnet_core::Config;
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_core::scheduler::TaskRegistry;
use flixnet_db::Db;
use flixnet_library::images::ImageCache;
use flixnet_library::jobs as library_jobs;
use flixnet_livetv::{LiveSettings, LiveTvManager, jobs as livetv_jobs};
use flixnet_media::transcode::{TranscodeManager, TranscodeSettings};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Running job workers (imports, guide, library scans) and quick live settings; the
/// config dir (and with it the recordings folder) is the temp dir.
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
        })
        .register(library_jobs::ScanLibrary)
        .register(library_jobs::RefreshMetadata)
        .register(library_jobs::ProbeFile {
            ffprobe: flixnet_media::Ffprobe::new("ffprobe"),
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
    let mut config = Config::default();
    config.paths.config_dir = dir.path().to_path_buf();
    let router = router(AppState::new(config, db.clone(), services));
    (TestApp { dir, router, db }, running)
}

async fn raw_get(app: &TestApp, uri: &str) -> (StatusCode, Option<String>, usize) {
    let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, content_type, bytes.len())
}

async fn eventually(
    app: &TestApp,
    token: &str,
    uri: &str,
    check: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let response = app.get(uri, Some(token)).await;
        if response.status == StatusCode::OK && check(&response.json) {
            return response.json;
        }
        assert!(
            Instant::now() < deadline,
            "timed out on {uri}: {} {}",
            response.status,
            response.json
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn setup(app: &TestApp, admin: &str, fake: &fake::FakeSource, tuners: i64) {
    let created = app
        .post(
            "/api/v1/livetv/tuners",
            json!({ "kind": "m3u", "name": "Fake-IPTV", "url": fake.url("/playlist.m3u"), "tuner_count": tuners }),
            Some(admin),
        )
        .await;
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

/// The program `title` (search).
async fn program(app: &TestApp, token: &str, title: &str) -> Value {
    let found = app
        .get(
            &format!("/api/v1/livetv/programs?q={}", title.replace(' ', "+")),
            Some(token),
        )
        .await;
    found
        .json
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["title"] == title)
        .unwrap_or_else(|| panic!("no program {title}: {}", found.json))
        .clone()
}

async fn member(app: &TestApp, admin: &str, name: &str, extra: Value) -> (i64, String) {
    let mut body = json!({ "name": name, "password": PASSWORD });
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let created = app.post("/api/v1/users", body, Some(admin)).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    let login = app
        .post(
            "/api/v1/auth/login",
            credentials(name, PASSWORD, &format!("{name}-tv")),
            None,
        )
        .await;
    (
        created.json["id"].as_i64().unwrap(),
        login.json["token"].as_str().unwrap().to_owned(),
    )
}

#[tokio::test]
async fn rules_conflicts_priorities_and_cancelling() {
    let fake = fake::start().await;
    let (app, _jobs) = app().await;
    let admin = app.setup_admin().await;
    setup(&app, &admin, &fake, 1).await;

    // One program: scheduled without conflict, with the default padding.
    let zdf = program(&app, &admin, "Zweite Sendung 10").await;
    let first = app
        .post(
            "/api/v1/livetv/recordings",
            json!({ "program_id": zdf["id"], "utc_offset_min": 60 }),
            Some(&admin),
        )
        .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.json);
    assert_eq!(first.json["status"], "scheduled");
    assert_eq!(first.json["conflict"], false);
    assert_eq!(first.json["padding_before_s"], 60);
    assert_eq!(first.json["padding_after_s"], 300);
    assert_eq!(first.json["rule_kind"], "program");
    let again = app
        .post(
            "/api/v1/livetv/recordings",
            json!({ "program_id": zdf["id"] }),
            Some(&admin),
        )
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT);

    // The same slot on another channel needs a second tuner: conflict.
    let ard = program(&app, &admin, "Erste Sendung 10").await;
    let second = app
        .post(
            "/api/v1/livetv/recordings",
            json!({ "program_id": ard["id"] }),
            Some(&admin),
        )
        .await;
    assert_eq!(second.status, StatusCode::CREATED, "{}", second.json);
    assert_eq!(second.json["conflict"], true);

    // The guide marks both.
    let guide_program = program(&app, &admin, "Zweite Sendung 10").await;
    assert_eq!(guide_program["recording"]["status"], "scheduled");
    assert_eq!(guide_program["recording"]["series"], false);

    // A higher priority moves the conflict to the other one.
    let rule = second.json["rule_id"].as_i64().unwrap();
    let raised = app
        .request(
            Method::PATCH,
            &format!("/api/v1/livetv/recording-rules/{rule}"),
            Some(json!({ "priority": 5 })),
            Some(&admin),
        )
        .await;
    assert_eq!(raised.status, StatusCode::OK, "{}", raised.json);
    let recordings = app
        .get("/api/v1/livetv/recordings", Some(&admin))
        .await
        .json;
    let by_title = |title: &str| {
        recordings
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["title"] == title)
            .unwrap()
            .clone()
    };
    assert_eq!(by_title("Erste Sendung 10")["conflict"], false);
    assert_eq!(by_title("Zweite Sendung 10")["conflict"], true);

    // A series rule by title, only new episodes, on one channel.
    let arte = program(&app, &admin, "Arte Sendung 12").await;
    let series = app
        .post(
            "/api/v1/livetv/recording-rules",
            json!({ "program_id": arte["id"], "channel_id": arte["channel_id"], "new_only": false, "keep_last": 3 }),
            Some(&admin),
        )
        .await;
    assert_eq!(series.status, StatusCode::CREATED, "{}", series.json);
    assert_eq!(series.json["kind"], "series");
    assert_eq!(series.json["scheduled"], 1);
    assert_eq!(series.json["keep_last"], 3);
    let rules = app
        .get("/api/v1/livetv/recording-rules", Some(&admin))
        .await
        .json;
    assert_eq!(rules.as_array().unwrap().len(), 3);
    let invalid = app
        .post(
            "/api/v1/livetv/recording-rules",
            json!({ "title": "X", "window_start_min": 1200 }),
            Some(&admin),
        )
        .await;
    assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Cancelling the one-off removes its rule; deleting the series rule its schedule.
    let cancel = app
        .request(
            Method::DELETE,
            &format!("/api/v1/livetv/recordings/{}", first.json["id"]),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(cancel.status, StatusCode::NO_CONTENT);
    let deleted = app
        .request(
            Method::DELETE,
            &format!("/api/v1/livetv/recording-rules/{}", series.json["id"]),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    let left = app
        .get("/api/v1/livetv/recordings", Some(&admin))
        .await
        .json;
    let titles: Vec<_> = left
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["title"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(titles, ["Erste Sendung 10"]);
    assert_eq!(left[0]["conflict"], false, "nothing competes any more");

    // Members record, but cannot touch others' rules.
    let (_, anna) = member(&app, &admin, "Anna", json!({})).await;
    let forbidden = app
        .request(
            Method::DELETE,
            &format!("/api/v1/livetv/recordings/{}", left[0]["id"]),
            None,
            Some(&anna),
        )
        .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_short_real_recording_becomes_a_library_item() {
    let fake = fake::start().await;
    let (app, _jobs) = app().await;
    let admin = app.setup_admin().await;
    setup(&app, &admin, &fake, 2).await;

    // Record what runs now (no padding) and stop after a few seconds.
    let running = program(&app, &admin, "Tagesschau").await;
    let created = app
        .post(
            "/api/v1/livetv/recordings",
            json!({ "program_id": running["id"], "padding_before_s": 0, "padding_after_s": 0 }),
            Some(&admin),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    let id = created.json["id"].as_i64().unwrap();
    let uri = format!("/api/v1/livetv/recordings/{id}");
    eventually(&app, &admin, &uri, |r| {
        r["status"] == "recording" && r["bytes"].as_i64().unwrap() > 200_000
    })
    .await;
    let streams = app.get("/api/v1/livetv/streams", Some(&admin)).await.json;
    assert_eq!(streams[0]["recordings"], 1);
    let deleting_running = app.request(Method::DELETE, &uri, None, Some(&admin)).await;
    assert_eq!(deleting_running.status, StatusCode::CONFLICT);
    let stopped = app
        .post(&format!("{uri}/stop"), json!({}), Some(&admin))
        .await;
    assert_eq!(stopped.status, StatusCode::OK, "{}", stopped.json);
    assert_eq!(stopped.json["status"], "completed");

    // The recordings folder became a library and was scanned.
    let done = eventually(&app, &admin, &uri, |r| !r["item_id"].is_null()).await;
    let item = app
        .get(&format!("/api/v1/items/{}", done["item_id"]), Some(&admin))
        .await;
    assert_eq!(item.status, StatusCode::OK, "{}", item.json);
    assert_eq!(item.json["title"], "Tagesschau");
    let libraries = app.get("/api/v1/libraries", Some(&admin)).await.json;
    let names: Vec<_> = libraries
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        names.contains(&"Aufnahmen – Filme".to_owned())
            && names.contains(&"Aufnahmen – Serien".to_owned()),
        "{names:?}"
    );
    let file = app
        .dir
        .path()
        .join("recordings/Filme")
        .read_dir()
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(file.join("movie.nfo").is_file(), "{file:?}");

    // Deleting removes the files.
    let delete = app.request(Method::DELETE, &uri, None, Some(&admin)).await;
    assert_eq!(delete.status, StatusCode::NO_CONTENT);
    assert!(!file.exists(), "{file:?}");
    assert_eq!(
        app.get(&uri, Some(&admin)).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn logos_parental_controls_and_live_tv_access() {
    let fake = fake::start().await;
    let (app, _jobs) = app().await;
    let admin = app.setup_admin().await;
    setup(&app, &admin, &fake, 0).await;

    // Logos are served from the cache through signed URLs.
    let channels = app.get("/api/v1/livetv/channels", Some(&admin)).await.json;
    let ard = channels
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Das Erste HD")
        .unwrap()
        .clone();
    let logo = ard["logo_url"].as_str().unwrap().to_owned();
    assert!(logo.starts_with("/api/v1/livetv/channels/"), "{logo}");
    assert!(!logo.contains("127.0.0.1"), "never the provider URL");
    for _ in 0..2 {
        let (status, content_type, len) = raw_get(&app, &logo).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type.as_deref(), Some("image/png"));
        assert!(len > 0);
    }
    assert_eq!(
        fake.logo_downloads(),
        1,
        "the second request came from the cache"
    );
    let unsigned = logo.split('?').next().unwrap();
    assert_eq!(raw_get(&app, unsigned).await.0, StatusCode::UNAUTHORIZED);
    let forged = format!("{unsigned}?exp=9999999999&sig=00000000000000000000000000000000");
    assert_eq!(raw_get(&app, &forged).await.0, StatusCode::UNAUTHORIZED);

    // Something to hide: a recording of a program.
    let zdf = program(&app, &admin, "Zweite Sendung 12").await;
    let scheduled = app
        .post(
            "/api/v1/livetv/recordings",
            json!({ "program_id": zdf["id"] }),
            Some(&admin),
        )
        .await;
    assert_eq!(scheduled.status, StatusCode::CREATED);

    // A child profile (age 12, unrated hidden): the guide keeps slots but hides
    // titles (the fake guide has no ratings), search finds nothing, watching and
    // recordings are refused.
    let (_, kid) = member(&app, &admin, "Kind", json!({ "max_age_rating": 12 })).await;
    let now = flixnet_db::now_ms();
    let guide = app
        .get(
            &format!(
                "/api/v1/livetv/guide?start_ms={}&end_ms={}",
                now,
                now + 3_600_000
            ),
            Some(&kid),
        )
        .await;
    assert_eq!(guide.status, StatusCode::OK);
    let text = guide.json.to_string();
    assert!(!text.contains("Sendung 1"), "{text}");
    assert!(text.contains("Gesperrte Sendung"));
    assert!(guide.json["rows"][0]["programs"][0]["restricted"] == true);
    let search = app
        .get("/api/v1/livetv/programs?q=Sendung", Some(&kid))
        .await;
    assert_eq!(search.json, json!([]));
    let detail = app
        .get(
            &format!("/api/v1/livetv/programs/{}", zdf["id"]),
            Some(&kid),
        )
        .await;
    assert_eq!(detail.status, StatusCode::NOT_FOUND);
    let watch = app
        .post(
            &format!("/api/v1/livetv/channels/{}/watch", ard["id"]),
            json!({ "device_profile": { "containers": ["mp4"], "video_codecs": [{ "codec": "h264" }], "audio_codecs": [{ "codec": "aac" }] } }),
            Some(&kid),
        )
        .await;
    assert_eq!(watch.status, StatusCode::FORBIDDEN, "{}", watch.json);
    assert_eq!(watch.json["code"], "program_restricted");
    let recordings = app.get("/api/v1/livetv/recordings", Some(&kid)).await;
    assert_eq!(recordings.json, json!([]));
    let record = app
        .post(
            "/api/v1/livetv/recordings",
            json!({ "program_id": zdf["id"] }),
            Some(&kid),
        )
        .await;
    assert_eq!(record.status, StatusCode::NOT_FOUND);

    // A profile without Live TV: refused everywhere; the administrator sees it.
    let (gast_id, gast) = member(&app, &admin, "Gast", json!({})).await;
    let allowed = app.get("/api/v1/livetv/channels", Some(&gast)).await;
    assert_eq!(allowed.status, StatusCode::OK);
    let bar = app
        .request(
            Method::PUT,
            &format!("/api/v1/livetv/profiles/{gast_id}"),
            Some(json!({ "live_tv_allowed": false })),
            Some(&admin),
        )
        .await;
    assert_eq!(bar.status, StatusCode::NO_CONTENT);
    for uri in [
        "/api/v1/livetv/channels".to_owned(),
        format!(
            "/api/v1/livetv/guide?start_ms={now}&end_ms={}",
            now + 60_000
        ),
        "/api/v1/livetv/programs?q=Sendung".to_owned(),
        "/api/v1/livetv/recordings".to_owned(),
        "/api/v1/livetv/recording-rules".to_owned(),
    ] {
        let response = app.get(&uri, Some(&gast)).await;
        assert_eq!(response.status, StatusCode::FORBIDDEN, "{uri}");
        assert_eq!(response.json["code"], "live_tv_forbidden");
    }
    let profiles = app.get("/api/v1/livetv/profiles", Some(&admin)).await.json;
    let gast_row = profiles
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "Gast")
        .unwrap();
    assert_eq!(gast_row["live_tv_allowed"], false);
    assert_eq!(
        app.get("/api/v1/livetv/profiles", Some(&gast)).await.status,
        StatusCode::FORBIDDEN
    );
}
