//! Milestone 5.2: OpenSubtitles settings, search and download against a mocked API.

mod common;

use axum::http::{Method, StatusCode};
use common::{PASSWORD, TestApp, app, credentials};
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::{items, libraries, settings};
use flixnet_library::refresh::refresh_local;
use flixnet_library::scan::scan_library;
use flixnet_library::subtitles::{self, SubtitleSettings};
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const VIDEO_SIZE: usize = 200_000;

fn write(root: &std::path::Path, relative: &str, content: &[u8]) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// Two movies with TMDB/IMDb ids (Pate, Nosferatu) and a series episode.
async fn library(app: &TestApp, root: &std::path::Path) -> (i64, i64) {
    let movies = root.join("Filme");
    let video = vec![0u8; VIDEO_SIZE];
    for (dir, file, title, tmdb, imdb) in [
        (
            "Der Pate (1972)",
            "Der.Pate.1972.German.1080p.BluRay.x264-GRP.mkv",
            "Der Pate",
            238,
            "tt0068646",
        ),
        (
            "Nosferatu (1922)",
            "Nosferatu.1922.mkv",
            "Nosferatu",
            653,
            "tt0013442",
        ),
    ] {
        write(&movies, &format!("{dir}/{file}"), &video);
        write(
            &movies,
            &format!("{dir}/movie.nfo"),
            format!(
                "<movie><title>{title}</title><uniqueid type=\"tmdb\">{tmdb}</uniqueid>\
                 <uniqueid type=\"imdb\">{imdb}</uniqueid></movie>"
            )
            .as_bytes(),
        );
    }
    let shows = root.join("Serien");
    write(&shows, "Dark (2017)/Staffel 1/Dark.S01E02.mkv", &video);
    write(
        &shows,
        "Dark (2017)/tvshow.nfo",
        b"<tvshow><title>Dark</title><uniqueid type=\"tmdb\">70523</uniqueid></tvshow>",
    );
    let mut ids = Vec::new();
    for (name, kind, root) in [("Filme", "movies", &movies), ("Serien", "shows", &shows)] {
        let id = libraries::create(&app.db, name, kind, &[root.to_string_lossy().into_owned()])
            .await
            .unwrap()
            .id;
        let queue = JobRuntime::new(app.db.clone(), EventBus::new())
            .handle()
            .queue()
            .clone();
        scan_library(&app.db, &queue, id).await.unwrap();
        for kind in ["movie", "series", "season", "episode"] {
            for item in items::list_by_kind(&app.db, id, kind).await.unwrap() {
                refresh_local(&app.db, item.id).await.unwrap();
            }
        }
        ids.push(id);
    }
    (ids[0], ids[1])
}

async fn item(app: &TestApp, library: i64, kind: &str, title: &str) -> i64 {
    items::list_by_kind(&app.db, library, kind)
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.title == title)
        .unwrap_or_else(|| panic!("no {kind} {title}"))
        .id
}

/// Settings pointing at the mock server (the base URL is not settable via the API).
async fn configure(app: &TestApp, server: &MockServer, extra: SubtitleSettings) {
    let s = SubtitleSettings {
        api_key: Some("test-key".into()),
        base_url: Some(format!("{}/api/v1", server.uri())),
        ..extra
    };
    settings::set(&app.db, subtitles::SETTINGS_KEY, &s)
        .await
        .unwrap();
}

fn result(file_id: i64, language: &str, release: &str, downloads: i64, hash: bool) -> Value {
    json!({ "id": file_id.to_string(), "type": "subtitle", "attributes": {
        "language": language, "download_count": downloads, "hearing_impaired": false,
        "fps": 25.0, "ratings": 7.5, "votes": 2, "release": release, "moviehash_match": hash,
        "files": [{ "file_id": file_id, "file_name": format!("{release}.srt") }]
    }})
}

async fn mock_download(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/v1/download"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "link": format!("{}/dl/sub.srt", server.uri()), "file_name": "sub.srt", "remaining": 4
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/dl/sub.srt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("1\n00:00:01,000 --> 00:00:02,000\nHallo Welt\n"),
        )
        .mount(server)
        .await;
}

#[tokio::test]
async fn settings_mask_secrets_and_validate() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/login"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "token": "jwt", "status": 200 })),
        )
        .mount(&server)
        .await;
    configure(&app, &server, SubtitleSettings::default()).await;

    let initial = app.get("/api/v1/settings/subtitles", Some(&admin)).await;
    assert_eq!(initial.status, StatusCode::OK);
    assert_eq!(initial.json["has_api_key"], true);
    assert_eq!(initial.json["auto_download"], false);

    let put = |body: Value| {
        app.request(
            Method::PUT,
            "/api/v1/settings/subtitles",
            Some(body),
            Some(&admin),
        )
    };
    let saved = put(json!({
        "api_key": " neuer-schluessel ", "username": "felix", "password": "os-passwort",
        "languages": ["de", "EN", "de"], "auto_download": true
    }))
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.json);
    assert_eq!(saved.json["has_password"], true);
    assert_eq!(saved.json["username"], "felix");
    assert_eq!(saved.json["languages"], json!(["de", "en"]));
    assert_eq!(saved.json["auto_download"], true);
    let text = saved.json.to_string();
    assert!(
        !text.contains("neuer-schluessel") && !text.contains("os-passwort"),
        "{text}"
    );
    let stored: SubtitleSettings = settings::get(&app.db, subtitles::SETTINGS_KEY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.api_key.as_deref(), Some("neuer-schluessel"));
    assert!(
        stored.auto_since_ms.is_some(),
        "switching on marks the start"
    );
    assert!(stored.base_url.is_some(), "kept across updates");

    for bad in [
        json!({ "languages": ["deu"] }),
        json!({ "languages": [], "auto_download": true }),
    ] {
        let response = put(bad.clone()).await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }

    // A key OpenSubtitles rejects is not saved.
    let rejecting = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/infos/formats"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&rejecting)
        .await;
    configure(&app, &rejecting, SubtitleSettings::default()).await;
    let invalid = put(json!({ "api_key": "falsch" })).await;
    assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        invalid.json["message"]
            .as_str()
            .unwrap()
            .contains("ungültig")
    );

    // Removing everything.
    let cleared = put(json!({ "api_key": null, "username": null, "password": null })).await;
    assert_eq!(cleared.json["has_api_key"], false);
    assert_eq!(cleared.json["username"], Value::Null);

    // Members may not touch it.
    let member = app
        .post(
            "/api/v1/users",
            json!({ "name": "Anna", "password": PASSWORD }),
            Some(&admin),
        )
        .await;
    assert_eq!(member.status, StatusCode::CREATED);
    let token = app
        .post(
            "/api/v1/auth/login",
            credentials("Anna", PASSWORD, "anna"),
            None,
        )
        .await
        .json["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        app.get("/api/v1/settings/subtitles", Some(&token))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.get("/api/v1/items/1/subtitles/search?language=de", Some(&token))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn search_ranks_results_and_download_adds_a_track() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let media = tempfile::tempdir().unwrap();
    let (movies, shows) = library(&app, media.path()).await;
    let pate = item(&app, movies, "movie", "Der Pate").await;
    let nosferatu = item(&app, movies, "movie", "Nosferatu").await;
    let episode = items::list_by_kind(&app.db, shows, "episode")
        .await
        .unwrap()[0]
        .id;

    // Not configured yet.
    let none = app
        .get(
            &format!("/api/v1/items/{pate}/subtitles/search?language=de"),
            Some(&admin),
        )
        .await;
    assert_eq!(none.status, StatusCode::CONFLICT, "{}", none.json);
    assert_eq!(none.json["code"], "opensubtitles_not_configured");

    let server = MockServer::start().await;
    configure(&app, &server, SubtitleSettings::default()).await;
    let hash = format!("{:016x}", VIDEO_SIZE);
    Mock::given(method("GET"))
        .and(path("/api/v1/subtitles"))
        .and(query_param("tmdb_id", "238"))
        .and(query_param("imdb_id", "68646"))
        .and(query_param("languages", "de"))
        .and(query_param("moviehash", hash.as_str()))
        .and(query_param("type", "movie"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [
            result(11, "de", "The.Godfather.1972.720p.WEB-DL.x265-XYZ", 5000, false),
            result(12, "de", "Der.Pate.1972.German.1080p.BluRay.x264-GRP", 10, false),
            result(13, "de", "irgendwas", 1, true),
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/subtitles"))
        .and(query_param("parent_tmdb_id", "70523"))
        .and(query_param("season_number", "1"))
        .and(query_param("episode_number", "2"))
        .and(query_param("type", "episode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [
            result(21, "en", "Dark.S01E02.720p", 3, false),
        ]})))
        .mount(&server)
        .await;
    mock_download(&server).await;

    let search = app
        .get(
            &format!("/api/v1/items/{pate}/subtitles/search?language=de"),
            Some(&admin),
        )
        .await;
    assert_eq!(search.status, StatusCode::OK, "{}", search.json);
    assert_eq!(search.json["moviehash"], hash);
    assert_eq!(
        search.json["file_name"],
        "Der.Pate.1972.German.1080p.BluRay.x264-GRP.mkv"
    );
    let ids: Vec<i64> = search.json["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["subtitle_file_id"].as_i64().unwrap())
        .collect();
    assert_eq!(
        ids,
        [13, 12, 11],
        "hash match, then release fit, then downloads"
    );
    assert_eq!(search.json["results"][0]["match_score"], 100);
    assert_eq!(search.json["results"][1]["match_score"], 95);
    assert_eq!(search.json["results"][1]["fps"], 25.0);

    let episode_search = app
        .get(
            &format!("/api/v1/items/{episode}/subtitles/search?language=en"),
            Some(&admin),
        )
        .await;
    assert_eq!(
        episode_search.status,
        StatusCode::OK,
        "{}",
        episode_search.json
    );
    assert_eq!(episode_search.json["results"][0]["subtitle_file_id"], 21);

    let bad = app
        .get(
            &format!("/api/v1/items/{pate}/subtitles/search?language=Deutsch"),
            Some(&admin),
        )
        .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    let series = items::list_by_kind(&app.db, shows, "series").await.unwrap()[0].id;
    let not_playable = app
        .get(
            &format!("/api/v1/items/{series}/subtitles/search?language=de"),
            Some(&admin),
        )
        .await;
    assert_eq!(not_playable.status, StatusCode::NOT_FOUND);

    // Download next to the file: a new track right away.
    let downloaded = app
        .post(
            &format!("/api/v1/items/{pate}/subtitles/download"),
            json!({ "subtitle_file_id": 12, "language": "de", "release": "Der.Pate.1972.German.1080p.BluRay.x264-GRP" }),
            Some(&admin),
        )
        .await;
    assert_eq!(
        downloaded.status,
        StatusCode::CREATED,
        "{}",
        downloaded.json
    );
    assert_eq!(downloaded.json["saved_in_library"], true);
    assert_eq!(downloaded.json["language"], "deu");
    assert_eq!(downloaded.json["remaining_downloads"], 4);
    let index = downloaded.json["stream_index"].as_i64().unwrap();
    assert_eq!(index, 1000);
    let sidecar = media
        .path()
        .join("Filme/Der Pate (1972)/Der.Pate.1972.German.1080p.BluRay.x264-GRP.de.srt");
    assert!(
        std::fs::read_to_string(&sidecar)
            .unwrap()
            .contains("Hallo Welt")
    );

    let info = app
        .post(
            &format!("/api/v1/items/{pate}/playback-info"),
            json!({}),
            Some(&admin),
        )
        .await;
    assert_eq!(info.status, StatusCode::OK, "{}", info.json);
    let tracks = info.json["sources"][0]["subtitle_tracks"]
        .as_array()
        .unwrap();
    let track = tracks
        .iter()
        .find(|t| t["index"] == index)
        .expect("new track");
    assert_eq!(track["language"], "deu");
    assert_eq!(track["is_external"], true);
    assert!(
        track["vtt_url"]
            .as_str()
            .unwrap()
            .contains(&format!("/{index}.vtt"))
    );

    // A second download of the same language does not overwrite the first.
    let again = app
        .post(
            &format!("/api/v1/items/{pate}/subtitles/download"),
            json!({ "subtitle_file_id": 11, "language": "de", "hearing_impaired": true }),
            Some(&admin),
        )
        .await;
    assert_eq!(again.json["stream_index"], 1001, "{}", again.json);
    assert_eq!(
        again.json["file_name"],
        "Der.Pate.1972.German.1080p.BluRay.x264-GRP.de.sdh.srt"
    );

    // A library folder that cannot be written: the subtitle goes to the config store.
    std::fs::remove_dir_all(media.path().join("Filme/Nosferatu (1922)")).unwrap();
    let stored = app
        .post(
            &format!("/api/v1/items/{nosferatu}/subtitles/download"),
            json!({ "subtitle_file_id": 12, "language": "de" }),
            Some(&admin),
        )
        .await;
    assert_eq!(stored.status, StatusCode::CREATED, "{}", stored.json);
    assert_eq!(stored.json["saved_in_library"], false);
    let file_id = stored.json["file_id"].as_i64().unwrap();
    assert!(
        app.dir
            .path()
            .join(format!("subtitles/{file_id}/Nosferatu.1922.de.srt"))
            .is_file()
    );
    let info = app
        .post(
            &format!("/api/v1/items/{nosferatu}/playback-info"),
            json!({}),
            Some(&admin),
        )
        .await;
    assert_eq!(
        info.json["sources"][0]["subtitle_tracks"][0]["index"], 1000,
        "{}",
        info.json
    );
}

#[tokio::test]
async fn provider_failures_map_to_clear_errors() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let media = tempfile::tempdir().unwrap();
    let (movies, _) = library(&app, media.path()).await;
    let pate = item(&app, movies, "movie", "Der Pate").await;
    let server = MockServer::start().await;
    configure(&app, &server, SubtitleSettings::default()).await;
    Mock::given(method("POST"))
        .and(path("/api/v1/download"))
        .respond_with(
            ResponseTemplate::new(406).set_body_json(json!({ "message": "limit", "remaining": 0 })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/subtitles"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let quota = app
        .post(
            &format!("/api/v1/items/{pate}/subtitles/download"),
            json!({ "subtitle_file_id": 1, "language": "de" }),
            Some(&admin),
        )
        .await;
    assert_eq!(quota.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(quota.json["code"], "quota_exhausted");
    let down = app
        .get(
            &format!("/api/v1/items/{pate}/subtitles/search?language=de"),
            Some(&admin),
        )
        .await;
    assert_eq!(down.status, StatusCode::BAD_GATEWAY);
    assert_eq!(down.json["code"], "opensubtitles_unavailable");
}

#[tokio::test]
async fn automatic_downloads_fill_missing_languages_once() {
    let app = app().await;
    app.setup_admin().await;
    let media = tempfile::tempdir().unwrap();
    library(&app, media.path()).await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/subtitles"))
        .and(query_param("languages", "de"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [
            result(31, "de", "irgendein.release", 10, false),
            result(32, "en", "falsche.sprache", 99, true),
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/subtitles"))
        .and(query_param("languages", "en"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [] })))
        .mount(&server)
        .await;
    mock_download(&server).await;
    let store = app.dir.path().join("subtitles");

    // Off by default.
    configure(
        &app,
        &server,
        SubtitleSettings {
            languages: vec!["de".into()],
            ..Default::default()
        },
    )
    .await;
    assert_eq!(subtitles::auto_download(&app.db, &store).await.unwrap(), 0);

    configure(
        &app,
        &server,
        SubtitleSettings {
            auto_download: true,
            languages: vec!["de".into(), "en".into()],
            auto_since_ms: Some(0),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        subtitles::auto_download(&app.db, &store).await.unwrap(),
        3,
        "two movies and the episode, German"
    );
    let searches = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/v1/subtitles")
        .count();
    assert_eq!(searches, 6, "German and English for three files");

    // Nothing is asked twice: German exists now, English was searched without result.
    assert_eq!(subtitles::auto_download(&app.db, &store).await.unwrap(), 0);
    let again = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/v1/subtitles")
        .count();
    assert_eq!(again, searches);

    // Items added before switching on are not new.
    configure(
        &app,
        &server,
        SubtitleSettings {
            auto_download: true,
            languages: vec!["fr".into()],
            auto_since_ms: Some(flixnet_db::now_ms() + 1),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(subtitles::auto_download(&app.db, &store).await.unwrap(), 0);
}
