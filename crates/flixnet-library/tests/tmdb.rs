//! Milestone 2.5: TMDB matching and metadata against a mocked TMDB API.

use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::metadata;
use flixnet_db::{Db, items, libraries};
use flixnet_library::providers::{self, Tmdb, TmdbConfig};
use flixnet_library::refresh::refresh_local;
use flixnet_library::scan::scan_library;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn tmdb_mock() -> MockServer {
    let server = MockServer::start().await;
    let ok = |body: serde_json::Value| ResponseTemplate::new(200).set_body_json(body);

    Mock::given(method("GET"))
        .and(path("/search/movie"))
        .and(query_param("query", "Testfilm Alpha"))
        .respond_with(ok(json!({ "results": [
            { "id": 900_002, "title": "Testfilm Alpha 2", "original_title": "Test Movie Alpha 2", "release_date": "2023-05-01", "popularity": 30.0 },
            { "id": 900_001, "title": "Testfilm Alpha", "original_title": "Test Movie Alpha", "release_date": "2021-03-04", "popularity": 12.5 }
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/search/movie"))
        .and(query_param("query", "Voellig Unbekannt"))
        .respond_with(ok(json!({ "results": [
            { "id": 1, "title": "Etwas ganz anderes", "original_title": "Something else", "release_date": "1999-01-01", "popularity": 1.0 }
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/movie/900001"))
        .and(query_param("language", "de-DE"))
        .respond_with(ok(json!({
            "id": 900_001, "title": "Testfilm Alpha", "original_title": "Test Movie Alpha",
            "overview": "Ein deutscher Text.", "tagline": "", "runtime": 101,
            "release_date": "2021-03-04", "vote_average": 7.4, "imdb_id": "tt9000001",
            "genres": [{ "name": "Abenteuer" }, { "name": "Komödie" }],
            "production_companies": [{ "name": "Flixnet Studios" }],
            "credits": {
                "cast": [{ "id": 11, "name": "Erika Muster", "character": "Heldin", "order": 0, "profile_path": "/erika.jpg" }],
                "crew": [{ "id": 12, "name": "Max Regie", "job": "Director" }, { "id": 13, "name": "Kamera Kai", "job": "Director of Photography" }]
            },
            "images": {
                "posters": [
                    { "file_path": "/poster-en.jpg", "iso_639_1": "en", "vote_average": 9.0, "width": 1000, "height": 1500 },
                    { "file_path": "/poster-de.jpg", "iso_639_1": "de", "vote_average": 5.0, "width": 1000, "height": 1500 }
                ],
                "backdrops": [{ "file_path": "/backdrop.jpg", "iso_639_1": null, "vote_average": 5.0 }],
                "logos": [{ "file_path": "/logo.png", "iso_639_1": "de", "vote_average": 5.0 }]
            },
            "release_dates": { "results": [
                { "iso_3166_1": "US", "release_dates": [{ "certification": "PG-13" }] },
                { "iso_3166_1": "DE", "release_dates": [{ "certification": "" }, { "certification": "12" }] }
            ]},
            "external_ids": { "imdb_id": "tt9000001" },
            "keywords": { "keywords": [{ "id": 1, "name": "time travel" }, { "id": 2, "name": "dinosaur" }] }
        })))
        .mount(&server)
        .await;
    // English fallback for the empty German tagline.
    Mock::given(method("GET"))
        .and(path("/movie/900001"))
        .and(query_param("language", "en-US"))
        .respond_with(ok(json!({ "id": 900_001, "title": "Test Movie Alpha", "overview": "English text.", "tagline": "An English tagline." })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/find/990100"))
        .and(query_param("external_source", "tvdb_id"))
        .respond_with(ok(json!({ "movie_results": [], "tv_results": [{ "id": 700_001, "name": "Testserie", "original_name": "Test Series" }] })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/tv/700001"))
        .respond_with(ok(json!({
            "id": 700_001, "name": "Testserie", "original_name": "Test Series",
            "overview": "Eine Serie.", "first_air_date": "2022-01-07", "episode_run_time": [45],
            "vote_average": 8.1, "genres": [{ "name": "Drama" }], "networks": [{ "name": "Flixnet+" }],
            "created_by": [{ "id": 21, "name": "Sarah Schöpfer" }],
            "aggregate_credits": { "cast": [{ "id": 22, "name": "Tom Test", "roles": [{ "character": "Kommissar" }], "order": 0 }] },
            "images": { "posters": [{ "file_path": "/series.jpg", "iso_639_1": "de", "vote_average": 5.0 }] },
            "content_ratings": { "results": [{ "iso_3166_1": "DE", "rating": "16" }] },
            "external_ids": { "imdb_id": "tt7000001", "tvdb_id": 990_100 },
            "keywords": { "results": [{ "id": 3, "name": "police" }] },
            "seasons": [{ "season_number": 1 }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/tv/700001/season/1"))
        .respond_with(ok(json!({
            "season_number": 1, "name": "Staffel 1", "overview": "Alles beginnt.", "air_date": "2022-01-07",
            "poster_path": "/s1.jpg",
            "episodes": [
                { "episode_number": 1, "name": "Der Anfang", "overview": "Folge eins.", "air_date": "2022-01-07", "runtime": 47, "still_path": "/e1.jpg" },
                { "episode_number": 2, "name": "Die Wendung", "overview": "Folge zwei.", "air_date": "2022-01-14", "runtime": 44 }
            ]
        })))
        .mount(&server)
        .await;
    server
}

fn write(root: &std::path::Path, relative: &str, content: &[u8]) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[tokio::test]
async fn movies_and_series_are_matched_and_enriched() {
    let server = tmdb_mock().await;
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    let queue = JobRuntime::new(db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    let mut config = TmdbConfig::new("test-key");
    config.base_url = server.uri();
    let tmdb = Tmdb::new(config, db.clone());

    let movies_root = dir.path().join("Filme");
    write(
        &movies_root,
        "Testfilm Alpha (2021)/Testfilm.Alpha.2021.German.DL.1080p.mkv",
        b"v",
    );
    write(
        &movies_root,
        "Voellig Unbekannt (2005)/Voellig.Unbekannt.2005.mkv",
        b"v",
    );
    let shows_root = dir.path().join("Serien");
    write(
        &shows_root,
        "Testserie (2022) [tvdbid-990100]/Staffel 01/Testserie.S01E01.mkv",
        b"v",
    );
    write(
        &shows_root,
        "Testserie (2022) [tvdbid-990100]/Staffel 01/Testserie.S01E02.mkv",
        b"v",
    );
    // An NFO-locked episode title must survive provider data.
    write(
        &shows_root,
        "Testserie (2022) [tvdbid-990100]/Staffel 01/Testserie.S01E02.nfo",
        b"<episodedetails><title>Mein eigener Titel</title></episodedetails>",
    );

    let movies = libraries::create(
        &db,
        "Filme",
        "movies",
        &[movies_root.to_string_lossy().into_owned()],
    )
    .await
    .unwrap();
    let shows = libraries::create(
        &db,
        "Serien",
        "shows",
        &[shows_root.to_string_lossy().into_owned()],
    )
    .await
    .unwrap();
    scan_library(&db, &queue, movies.id).await.unwrap();
    scan_library(&db, &queue, shows.id).await.unwrap();

    // --- Movie: best candidate by title+year, not the more popular sequel ----------
    let alpha = items::list_by_kind(&db, movies.id, "movie")
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.title == "Testfilm Alpha")
        .unwrap();
    let outcome = providers::refresh_remote(&db, &tmdb, alpha.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((outcome.tmdb_id, outcome.state), (Some(900_001), "matched"));

    let row = items::get(&db, alpha.id).await.unwrap().unwrap();
    assert_eq!(row.match_state, "matched");
    let ids = items::provider_ids(&db, alpha.id).await.unwrap();
    assert!(
        ids.contains(&("tmdb".into(), "900001".into()))
            && ids.contains(&("imdb".into(), "tt9000001".into()))
    );
    assert_eq!(
        metadata::genres(&db, alpha.id).await.unwrap(),
        ["Abenteuer", "Komödie"]
    );
    assert_eq!(
        flixnet_db::ai::keywords(&db, alpha.id).await.unwrap(),
        ["time travel", "dinosaur"]
    );
    let credits = metadata::credits(&db, alpha.id).await.unwrap();
    let names: Vec<_> = credits
        .iter()
        .map(|c| (c.role.as_str(), c.name.as_str()))
        .collect();
    assert_eq!(
        names,
        [("actor", "Erika Muster"), ("director", "Max Regie")],
        "camera crew is not listed"
    );
    let images = metadata::images(&db, alpha.id).await.unwrap();
    let posters: Vec<_> = images
        .iter()
        .filter(|i| i.kind == "poster")
        .map(|i| i.url.as_deref().unwrap())
        .collect();
    assert!(
        posters[0].ends_with("/poster-de.jpg"),
        "German poster preferred: {posters:?}"
    );

    // Overview from German, tagline from the English fallback, FSK from DE.
    let (overview, tagline, rating, age): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
    ) = sqlx_row(&db, alpha.id).await;
    assert_eq!(overview.as_deref(), Some("Ein deutscher Text."));
    assert_eq!(tagline.as_deref(), Some("An English tagline."));
    assert_eq!(rating.as_deref(), Some("FSK 12"));
    assert_eq!(age, Some(12));

    // --- An unknown movie is not force-matched ----------------------------------------
    let unknown = items::list_by_kind(&db, movies.id, "movie")
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.title == "Voellig Unbekannt")
        .unwrap();
    let outcome = providers::refresh_remote(&db, &tmdb, unknown.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((outcome.tmdb_id, outcome.state), (None, "failed"));
    assert_eq!(
        items::get(&db, unknown.id).await.unwrap().unwrap().title,
        "Voellig Unbekannt"
    );

    // Manual identification fixes it.
    providers::identify(&db, &tmdb, unknown.id, 900_001)
        .await
        .unwrap();
    assert_eq!(
        items::get(&db, unknown.id)
            .await
            .unwrap()
            .unwrap()
            .match_state,
        "manual"
    );

    // --- Series via the TVDB id hint, with seasons and episodes ------------------------
    for item in items::list_by_kind(&db, shows.id, "episode").await.unwrap() {
        refresh_local(&db, item.id).await.unwrap();
    }
    let series = &items::list_by_kind(&db, shows.id, "series").await.unwrap()[0];
    providers::refresh_remote(&db, &tmdb, series.id)
        .await
        .unwrap();
    assert_eq!(metadata::genres(&db, series.id).await.unwrap(), ["Drama"]);
    assert_eq!(
        flixnet_db::ai::keywords(&db, series.id).await.unwrap(),
        ["police"]
    );
    let roles: Vec<_> = metadata::credits(&db, series.id)
        .await
        .unwrap()
        .iter()
        .map(|c| (c.role.clone(), c.character.clone()))
        .collect();
    assert_eq!(
        roles,
        [
            ("actor".into(), Some("Kommissar".into())),
            ("creator".into(), None)
        ]
    );

    let episodes = items::list_by_kind(&db, shows.id, "episode").await.unwrap();
    assert_eq!(episodes[0].title, "Der Anfang");
    assert_eq!(
        episodes[1].title, "Mein eigener Titel",
        "NFO title stays locked"
    );
    assert_eq!(
        metadata::images(&db, episodes[0].id).await.unwrap()[0].kind,
        "thumb"
    );
    let season = &items::list_by_kind(&db, shows.id, "season").await.unwrap()[0];
    assert_eq!(metadata::images(&db, season.id).await.unwrap().len(), 1);
}

/// Reads a few descriptive columns not exposed on `Item`.
async fn sqlx_row(
    db: &Db,
    id: i64,
) -> (Option<String>, Option<String>, Option<String>, Option<i64>) {
    let item = items::details(db, id).await.unwrap().unwrap();
    (
        item.overview,
        item.tagline,
        item.official_rating,
        item.age_rating,
    )
}

#[tokio::test]
async fn invalid_api_key_is_reported() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "status_code": 7 })))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    let mut config = TmdbConfig::new("wrong");
    config.base_url = server.uri();
    let err = Tmdb::new(config, db)
        .search_movie("X", None)
        .await
        .unwrap_err();
    assert!(matches!(err, providers::TmdbError::InvalidKey), "{err}");
}
