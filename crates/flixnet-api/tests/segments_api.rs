mod common;

use std::path::Path;
use std::process::Command;

use axum::http::{Method, StatusCode};
use common::{PASSWORD, TestApp, app, credentials};
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::{libraries, media, segments};
use flixnet_library::scan::scan_library;
use flixnet_media::segments::SegmentAnalyzer;
use serde_json::{Value, json};

struct Fixture {
    admin: String,
    library: i64,
    series: i64,
    episodes: Vec<i64>,
}

/// Scans `root` as a shows library and signs in an admin.
async fn shows(app: &TestApp, root: &Path) -> Fixture {
    let library = libraries::create(
        &app.db,
        "Serien",
        "shows",
        &[root.to_string_lossy().into_owned()],
    )
    .await
    .unwrap()
    .id;
    let queue = JobRuntime::new(app.db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    scan_library(&app.db, &queue, library).await.unwrap();
    let admin = app.setup_admin().await;
    let series = app
        .get("/api/v1/items?kind=series", Some(&admin))
        .await
        .json["items"][0]["id"]
        .as_i64()
        .unwrap();
    let episodes = app
        .get(
            &format!("/api/v1/items?series_id={series}&kind=episode&sort=index"),
            Some(&admin),
        )
        .await
        .json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_i64().unwrap())
        .collect();
    Fixture {
        admin,
        library,
        series,
        episodes,
    }
}

async fn user(app: &TestApp, admin: &str) -> String {
    app.post(
        "/api/v1/users",
        json!({ "name": "Anna", "password": PASSWORD }),
        Some(admin),
    )
    .await;
    app.post(
        "/api/v1/auth/login",
        credentials("Anna", PASSWORD, "anna"),
        None,
    )
    .await
    .json["token"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn placeholder_season(root: &Path) {
    let season = root.join("Dark/Staffel 1");
    std::fs::create_dir_all(&season).unwrap();
    for n in 1..=2 {
        std::fs::write(season.join(format!("Dark.S01E0{n}.mkv")), b"v").unwrap();
    }
}

#[tokio::test]
async fn segments_can_be_set_read_and_cleared_by_hand() {
    let app = app().await;
    let media_dir = tempfile::tempdir().unwrap();
    placeholder_season(media_dir.path());
    let f = shows(&app, media_dir.path()).await;
    let admin = Some(f.admin.as_str());
    let e1 = f.episodes[0];
    let url = format!("/api/v1/items/{e1}/segments");

    let empty = app.get(&url, admin).await;
    assert_eq!(empty.status, StatusCode::OK);
    assert_eq!(empty.json, json!([]));

    let set = app
        .request(
            Method::PUT,
            &format!("{url}/intro"),
            Some(json!({ "start_ms": 5_000, "end_ms": 41_000 })),
            admin,
        )
        .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.json);
    assert_eq!(set.json[0]["kind"], "intro");
    assert_eq!(set.json[0]["start_ms"], 5_000);
    assert_eq!(set.json[0]["end_ms"], 41_000);
    assert_eq!(set.json[0]["source"], "manual");
    assert_eq!(set.json[0]["confidence"], 1.0);

    // Playback info carries the segments of each source.
    let info = app
        .post(
            &format!("/api/v1/items/{e1}/playback-info"),
            json!({}),
            admin,
        )
        .await;
    assert_eq!(info.status, StatusCode::OK, "{}", info.json);
    assert_eq!(info.json["sources"][0]["segments"][0]["kind"], "intro");
    assert_eq!(info.json["sources"][0]["segments"][0]["end_ms"], 41_000);

    // Invalid times, unknown kinds and foreign files are rejected.
    for body in [
        json!({ "start_ms": 9_000, "end_ms": 9_000 }),
        json!({ "start_ms": -1, "end_ms": 9_000 }),
    ] {
        let bad = app
            .request(Method::PUT, &format!("{url}/intro"), Some(body), admin)
            .await;
        assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", bad.json);
    }
    let unknown = app
        .request(
            Method::PUT,
            &format!("{url}/outro"),
            Some(json!({ "start_ms": 0, "end_ms": 1 })),
            admin,
        )
        .await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    let other_file = segments::for_item(&app.db, f.episodes[1]).await.unwrap();
    assert!(other_file.is_empty());
    let foreign = media::files_for_item(&app.db, f.episodes[1]).await.unwrap()[0].id;
    let wrong = app
        .request(
            Method::PUT,
            &format!("{url}/credits"),
            Some(json!({ "start_ms": 0, "end_ms": 1_000, "file_id": foreign })),
            admin,
        )
        .await;
    assert_eq!(wrong.status, StatusCode::NOT_FOUND);

    // Profiles see segments but cannot change them.
    let anna = user(&app, &f.admin).await;
    assert_eq!(app.get(&url, Some(&anna)).await.json[0]["kind"], "intro");
    let denied = app
        .request(
            Method::PUT,
            &format!("{url}/intro"),
            Some(json!({ "start_ms": 0, "end_ms": 1 })),
            Some(&anna),
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let denied = app
        .request(Method::DELETE, &format!("{url}/intro"), None, Some(&anna))
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);

    let cleared = app
        .request(Method::DELETE, &format!("{url}/intro"), None, admin)
        .await;
    assert_eq!(cleared.status, StatusCode::NO_CONTENT);
    let again = app
        .request(Method::DELETE, &format!("{url}/intro"), None, admin)
        .await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
    assert_eq!(app.get(&url, admin).await.json, json!([]));
}

#[tokio::test]
async fn a_re_probe_invalidates_segments_and_fingerprints() {
    let app = app().await;
    let media_dir = tempfile::tempdir().unwrap();
    placeholder_season(media_dir.path());
    let f = shows(&app, media_dir.path()).await;
    let file = media::files_for_item(&app.db, f.episodes[0]).await.unwrap()[0].id;
    segments::set_manual(&app.db, file, "intro", 1_000, 30_000)
        .await
        .unwrap();
    let print = segments::StoredFingerprint {
        stream_index: 1,
        start_ms: 0,
        items: vec![1, 2, 3],
    };
    segments::store_fingerprint(&app.db, file, "head", &print)
        .await
        .unwrap();
    assert_eq!(
        segments::fingerprint(&app.db, file, "head").await.unwrap(),
        Some(print)
    );

    media::store_probe(&app.db, file, &media::ProbeData::default())
        .await
        .unwrap();
    assert!(segments::for_file(&app.db, file).await.unwrap().is_empty());
    assert_eq!(
        segments::fingerprint(&app.db, file, "head").await.unwrap(),
        None
    );
}

#[tokio::test]
async fn detection_settings_and_triggers() {
    let app = app().await;
    let media_dir = tempfile::tempdir().unwrap();
    placeholder_season(media_dir.path());
    let f = shows(&app, media_dir.path()).await;
    let admin = Some(f.admin.as_str());
    let library_url = format!("/api/v1/libraries/{}", f.library);

    // Intro detection is on by default and can be switched per library.
    assert_eq!(
        app.get(&library_url, admin).await.json["intro_detection"],
        true
    );
    let analyze = app
        .post(&format!("{library_url}/segments/analyze"), json!({}), admin)
        .await;
    assert_eq!(analyze.status, StatusCode::ACCEPTED, "{}", analyze.json);
    assert_eq!(analyze.json["seasons"], 1);
    // Already queued: not queued twice.
    let item = app
        .post(
            &format!("/api/v1/items/{}/segments/analyze", f.series),
            json!({}),
            admin,
        )
        .await;
    assert_eq!(item.status, StatusCode::ACCEPTED);
    assert_eq!(item.json["seasons"], 0);

    let off = app
        .request(
            Method::PATCH,
            &library_url,
            Some(json!({ "intro_detection": false })),
            admin,
        )
        .await;
    assert_eq!(off.status, StatusCode::OK, "{}", off.json);
    assert_eq!(off.json["intro_detection"], false);
    assert_eq!(
        segments::library_seasons(&app.db, f.library).await.unwrap(),
        Vec::<i64>::new()
    );
    let file = media::files_for_item(&app.db, f.episodes[0]).await.unwrap()[0].id;
    assert_eq!(segments::season_of_file(&app.db, file).await.unwrap(), None);

    let missing = app
        .post("/api/v1/libraries/999/segments/analyze", json!({}), admin)
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    let missing = app
        .post("/api/v1/items/999/segments/analyze", json!({}), admin)
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);

    let anna = user(&app, &f.admin).await;
    let denied = app
        .post(
            &format!("{library_url}/segments/analyze"),
            json!({}),
            Some(&anna),
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn skipping_intros_is_a_personal_preference() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let anna = user(&app, &admin).await;
    let url = "/api/v1/users/me/preferences";
    assert_eq!(
        app.get(url, Some(&admin)).await.json,
        json!({ "skip_intro_automatically": false })
    );

    let on = app
        .request(
            Method::PATCH,
            url,
            Some(json!({ "skip_intro_automatically": true })),
            Some(&admin),
        )
        .await;
    assert_eq!(on.status, StatusCode::OK, "{}", on.json);
    assert_eq!(on.json["skip_intro_automatically"], true);
    // Empty updates change nothing; other profiles keep their own value.
    let same = app
        .request(Method::PATCH, url, Some(json!({})), Some(&admin))
        .await;
    assert_eq!(same.json["skip_intro_automatically"], true);
    assert_eq!(
        app.get(url, Some(&anna)).await.json["skip_intro_automatically"],
        false
    );
}

// ---- Real detection (ffmpeg with Chromaprint) ---------------------------------------

const INTRO_S: f64 = 30.0;

/// Episode: own random tones, the shared intro tune, more own tones.
fn episode(path: &Path, seed: u32, cold_open_s: f64) {
    let own = |seed: u32, seconds: f64| {
        let hash = format!("sin(floor(t*3)*12.9898+{seed}*78.233)*43758.5453");
        format!("aevalsrc=0.25*sin(2*PI*t*(150+700*({hash}-floor({hash})))):s=48000:d={seconds}")
    };
    let intro = format!(
        "aevalsrc=0.3*sin(2*PI*t*220*pow(2\\,mod(floor(t/0.75)*7\\,12)/12)):s=48000:d={INTRO_S}"
    );
    let parts = [own(seed, cold_open_s), intro, own(seed + 500, 60.0)];
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    let total: f64 = cold_open_s + INTRO_S + 60.0;
    cmd.args([
        "-f",
        "lavfi",
        "-i",
        &format!("testsrc2=s=160x90:r=5:d={total}"),
    ]);
    for part in &parts {
        cmd.args(["-f", "lavfi", "-i", part]);
    }
    cmd.args([
        "-filter_complex",
        "[1:a][2:a][3:a]concat=n=3:v=0:a=1[a]",
        "-map",
        "0:v",
        "-map",
        "[a]",
    ]);
    cmd.args([
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-c:a",
        "aac",
        "-shortest",
    ]);
    assert!(cmd.arg(path).status().expect("ffmpeg on PATH").success());
}

#[tokio::test]
async fn analysis_of_a_scanned_season_feeds_the_player() {
    let analyzer = SegmentAnalyzer::new("ffmpeg");
    if !analyzer.chromaprint_available().await {
        eprintln!("skipped: ffmpeg was built without Chromaprint");
        return;
    }
    let app = app().await;
    let media_dir = tempfile::tempdir().unwrap();
    let season = media_dir.path().join("Intro Serie/Staffel 1");
    std::fs::create_dir_all(&season).unwrap();
    let cold_opens = [8.4, 21.7, 3.2];
    for (i, cold_open) in cold_opens.iter().enumerate() {
        episode(
            &season.join(format!("Intro.Serie.S01E0{}.mkv", i + 1)),
            i as u32 * 7 + 1,
            *cold_open,
        );
    }
    let f = shows(&app, media_dir.path()).await;
    let ffprobe = flixnet_media::Ffprobe::new("ffprobe");
    for file in media::file_states(&app.db, f.library).await.unwrap() {
        flixnet_library::jobs::probe_file(&app.db, &ffprobe, file.id)
            .await
            .unwrap();
    }
    let file = media::files_for_item(&app.db, f.episodes[0]).await.unwrap()[0].id;
    let season_id = segments::season_of_file(&app.db, file)
        .await
        .unwrap()
        .expect("a season");

    let report = flixnet_library::segments::analyze_season(&app.db, &analyzer, season_id)
        .await
        .unwrap();
    assert_eq!(
        (report.files, report.fingerprinted, report.intros),
        (3, 6, 3),
        "{report:?}"
    );
    // Everything is cached now: a second run fingerprints nothing.
    let again = flixnet_library::segments::analyze_season(&app.db, &analyzer, season_id)
        .await
        .unwrap();
    assert_eq!(again.fingerprinted, 0);
    assert_eq!(again.intros, 3);

    for (episode, cold_open) in f.episodes.iter().zip(cold_opens) {
        let found = app
            .get(&format!("/api/v1/items/{episode}/segments"), Some(&f.admin))
            .await
            .json;
        let intro = found
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["kind"] == "intro")
            .unwrap_or_else(|| panic!("no intro: {found}"))
            .clone();
        let at = |v: &Value| v.as_i64().unwrap() as f64 / 1000.0;
        assert!((at(&intro["start_ms"]) - cold_open).abs() <= 1.0, "{intro}");
        assert!(
            (at(&intro["end_ms"]) - cold_open - INTRO_S).abs() <= 1.0,
            "{intro}"
        );
        assert_eq!(intro["source"], "fingerprint");
    }

    // A manual correction survives the next analysis.
    let e1 = f.episodes[0];
    app.request(
        Method::PUT,
        &format!("/api/v1/items/{e1}/segments/intro"),
        Some(json!({ "start_ms": 1_000, "end_ms": 20_000 })),
        Some(&f.admin),
    )
    .await;
    flixnet_library::segments::analyze_season(&app.db, &analyzer, season_id)
        .await
        .unwrap();
    let kept = app
        .get(&format!("/api/v1/items/{e1}/segments"), Some(&f.admin))
        .await
        .json;
    assert_eq!(kept[0]["source"], "manual", "{kept}");
    assert_eq!(kept[0]["end_ms"], 20_000);
}
