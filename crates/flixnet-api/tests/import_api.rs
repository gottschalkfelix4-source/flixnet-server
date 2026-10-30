//! History import from Plex and Jellyfin against mocked servers with realistic
//! payloads: matching statistics in the preview, writes on apply, idempotent repeats.

mod common;

use axum::http::StatusCode;
use common::{PASSWORD, TestApp, app};
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::{libraries, playstate, userdata};
use flixnet_library::scan::scan_library;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Fixture {
    admin: String,
    admin_id: i64,
    kid_id: i64,
    matrix: i64,
    alpha: i64,
    dark_e1: i64,
    dark_e2: i64,
    babylon_e1: i64,
}

async fn library(app: &TestApp) -> Fixture {
    let root = app.dir.path().join("media");
    for file in [
        "Filme/The Matrix (1999) {tmdb-603}/The Matrix (1999).mkv",
        "Filme/Alpha (2020)/Alpha.2020.mkv",
        "Serien/Dark {tvdb-334824}/Staffel 1/Dark.S01E01.mkv",
        "Serien/Dark {tvdb-334824}/Staffel 1/Dark.S01E02.mkv",
        "Serien/Babylon Berlin (2017)/Staffel 1/Babylon.Berlin.S01E01.mkv",
    ] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"v").unwrap();
    }
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
    let admin = app.setup_admin().await;
    let kid = app
        .post(
            "/api/v1/users",
            json!({ "name": "Kind", "password": PASSWORD }),
            Some(&admin),
        )
        .await;
    let find = |list: &Value, title: &str| {
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["title"].as_str().unwrap().contains(title))
            .unwrap_or_else(|| panic!("{title} in {list}"))["id"]
            .as_i64()
            .unwrap()
    };
    let movies = app.get("/api/v1/items?kind=movie", Some(&admin)).await.json;
    let series = app
        .get("/api/v1/items?kind=series", Some(&admin))
        .await
        .json;
    let episodes = |series: i64| {
        let app = &app;
        let admin = admin.clone();
        async move {
            app.get(
                &format!("/api/v1/items?series_id={series}&kind=episode"),
                Some(&admin),
            )
            .await
            .json
        }
    };
    let dark = episodes(find(&series, "Dark")).await;
    let babylon = episodes(find(&series, "Babylon")).await;
    let me = app.get("/api/v1/users/me", Some(&admin)).await.json;
    Fixture {
        admin_id: me["id"].as_i64().unwrap(),
        kid_id: kid.json["id"].as_i64().unwrap(),
        matrix: find(&movies, "Matrix"),
        alpha: find(&movies, "Alpha"),
        dark_e1: dark["items"][0]["id"].as_i64().unwrap(),
        dark_e2: dark["items"][1]["id"].as_i64().unwrap(),
        babylon_e1: babylon["items"][0]["id"].as_i64().unwrap(),
        admin,
    }
}

async fn plex_server() -> MockServer {
    let server = MockServer::start().await;
    let get = |route: &'static str| {
        Mock::given(method("GET"))
            .and(path(route))
            .and(header("X-Plex-Token", "plex-token"))
    };
    get("/library/sections")
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "MediaContainer": { "size": 3, "allowSync": false, "title1": "Plex Library", "Directory": [
                { "allowSync": true, "key": "1", "type": "movie", "title": "Filme", "agent": "tv.plex.agents.movie",
                  "scanner": "Plex Movie", "language": "de-DE", "uuid": "a1", "Location": [{ "id": 1, "path": "/data/filme" }] },
                { "allowSync": true, "key": "2", "type": "show", "title": "Serien", "agent": "tv.plex.agents.series",
                  "scanner": "Plex TV Series", "language": "de-DE", "uuid": "a2", "Location": [{ "id": 2, "path": "/data/serien" }] },
                { "allowSync": true, "key": "3", "type": "artist", "title": "Musik", "agent": "tv.plex.agents.music" }
            ] }
        })))
        .mount(&server)
        .await;
    get("/library/sections/1/all")
        .and(query_param("type", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "MediaContainer": { "size": 4, "totalSize": 4, "offset": 0, "librarySectionID": 1, "librarySectionTitle": "Filme", "Metadata": [
                { "ratingKey": "101", "key": "/library/metadata/101", "guid": "plex://movie/5d776825880197001ec967c6",
                  "type": "movie", "title": "Matrix", "originalTitle": "The Matrix", "year": 1999, "duration": 8160000,
                  "viewCount": 3, "lastViewedAt": 1705350660, "userRating": 9.0,
                  "Guid": [{ "id": "imdb://tt0133093" }, { "id": "tmdb://603" }, { "id": "tvdb://169" }] },
                { "ratingKey": "102", "guid": "plex://movie/5d9f35063c3d6b001fa8f6a1", "type": "movie", "title": "Alpha",
                  "year": 2020, "duration": 6000000, "viewOffset": 1800000, "lastViewedAt": 1706000000,
                  "Guid": [{ "id": "tmdb://999001" }] },
                { "ratingKey": "103", "guid": "com.plexapp.agents.imdb://tt0000042?lang=de", "type": "movie",
                  "title": "Nicht da", "year": 2001, "viewCount": 1, "lastViewedAt": 1600000000 },
                { "ratingKey": "104", "guid": "plex://movie/x", "type": "movie", "title": "Unberührt", "year": 2005 }
            ] }
        })))
        .mount(&server)
        .await;
    get("/library/sections/2/all")
        .and(query_param("type", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "MediaContainer": { "size": 2, "totalSize": 2, "Metadata": [
                { "ratingKey": "200", "type": "show", "title": "Dark", "year": 2017,
                  "Guid": [{ "id": "imdb://tt5753856" }, { "id": "tmdb://70523" }, { "id": "tvdb://334824" }] },
                { "ratingKey": "300", "type": "show", "title": "Babylon Berlin", "year": 2017,
                  "Guid": [{ "id": "tvdb://328634" }] }
            ] }
        })))
        .mount(&server)
        .await;
    get("/library/sections/2/all")
        .and(query_param("type", "4"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "MediaContainer": { "size": 3, "totalSize": 3, "Metadata": [
                { "ratingKey": "201", "type": "episode", "title": "Geheimnisse", "grandparentRatingKey": "200",
                  "grandparentTitle": "Dark", "parentIndex": 1, "index": 1, "viewCount": 1, "lastViewedAt": 1706100000,
                  "Guid": [{ "id": "tvdb://6327751" }] },
                { "ratingKey": "202", "type": "episode", "title": "Lügen", "grandparentRatingKey": "200",
                  "grandparentTitle": "Dark", "parentIndex": 1, "index": 2, "viewOffset": 900000, "lastViewedAt": 1706200000 },
                { "ratingKey": "301", "type": "episode", "title": "Folge 1", "grandparentRatingKey": "300",
                  "grandparentTitle": "Babylon Berlin", "parentIndex": 1, "index": 1, "viewCount": 2, "lastViewedAt": 1706300000 }
            ] }
        })))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn plex_preview_then_apply() {
    let server = plex_server().await;
    let app = app().await;
    let f = library(&app).await;
    let request = json!({
        "source": "plex", "server_url": format!("{}/", server.uri()), "token": "plex-token", "user_id": f.admin_id
    });

    let preview = app
        .post("/api/v1/import/preview", request.clone(), Some(&f.admin))
        .await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.json);
    assert_eq!(preview.json["applied"], false);
    let user = &preview.json["users"][0];
    assert_eq!(user["user_id"], f.admin_id);
    assert_eq!(user["user_name"], "Felix");
    assert_eq!(
        user["stats"],
        json!({
            "entries": 6, "matched": 5, "unmatched": 1, "by_provider_id": 1, "by_title_year": 1,
            "by_episode": 3, "played": 3, "in_progress": 2, "favorites": 0, "rated": 1
        })
    );
    assert_eq!(user["unmatched_samples"], json!(["Nicht da (2001)"]));
    assert!(user["written_items"].is_null());
    // Nothing was written by the preview.
    assert!(
        !userdata::get(&app.db, f.admin_id, f.matrix)
            .await
            .unwrap()
            .played
    );

    let applied = app
        .post("/api/v1/import/apply", request.clone(), Some(&f.admin))
        .await;
    assert_eq!(applied.status, StatusCode::OK, "{}", applied.json);
    assert_eq!(applied.json["applied"], true);
    assert_eq!(applied.json["users"][0]["written_items"], 5);
    assert_eq!(applied.json["users"][0]["written_history"], 3);

    let matrix = userdata::get(&app.db, f.admin_id, f.matrix).await.unwrap();
    assert!(matrix.played);
    assert_eq!(matrix.play_count, 3);
    assert_eq!(matrix.rating, Some(9.0));
    assert_eq!(matrix.last_played_at, Some(1_705_350_660_000));
    let alpha = userdata::get(&app.db, f.admin_id, f.alpha).await.unwrap();
    assert!(!alpha.played);
    assert_eq!(alpha.position_ms, 1_800_000);
    assert!(
        userdata::get(&app.db, f.admin_id, f.dark_e1)
            .await
            .unwrap()
            .played
    );
    assert_eq!(
        userdata::get(&app.db, f.admin_id, f.dark_e2)
            .await
            .unwrap()
            .position_ms,
        900_000
    );
    assert_eq!(
        userdata::get(&app.db, f.admin_id, f.babylon_e1)
            .await
            .unwrap()
            .play_count,
        2
    );
    let history = playstate::events(&app.db, f.admin_id).await.unwrap();
    assert_eq!(history.len(), 3);
    assert!(
        history
            .iter()
            .all(|e| e.completed && e.client.as_deref() == Some("Plex-Import"))
    );
    assert!(history.iter().any(|e| e.created_at == 1_705_350_660_000));

    // Repeating the import adds no history twice.
    let again = app
        .post("/api/v1/import/apply", request, Some(&f.admin))
        .await;
    assert_eq!(again.json["users"][0]["written_history"], 0);
    assert_eq!(
        playstate::events(&app.db, f.admin_id).await.unwrap().len(),
        3
    );
    // Imported items show up in "Weiterschauen".
    let resume = app.get("/api/v1/items/resume", Some(&f.admin)).await;
    let ids: Vec<i64> = resume
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_i64().unwrap())
        .collect();
    assert!(
        ids.contains(&f.alpha) && ids.contains(&f.dark_e2),
        "{ids:?}"
    );
}

#[tokio::test]
async fn plex_errors_and_validation() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/library/sections"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let app = app().await;
    let admin = app.setup_admin().await;
    let me = app.get("/api/v1/users/me", Some(&admin)).await.json["id"]
        .as_i64()
        .unwrap();

    let rejected = app
        .post("/api/v1/import/preview", json!({ "source": "plex", "server_url": server.uri(), "token": "falsch", "user_id": me }), Some(&admin))
        .await;
    assert_eq!(rejected.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        rejected.json["message"]
            .as_str()
            .unwrap()
            .contains("abgelehnt")
    );
    for body in [
        json!({ "source": "plex", "server_url": "plex.local", "token": "t", "user_id": me }),
        json!({ "source": "plex", "server_url": server.uri(), "user_id": me }),
        json!({ "source": "plex", "server_url": server.uri(), "token": "t" }),
        json!({ "source": "plex", "server_url": server.uri(), "token": "t", "user_id": 999 }),
        json!({ "source": "jellyfin", "server_url": server.uri() }),
    ] {
        let response = app
            .post("/api/v1/import/preview", body.clone(), Some(&admin))
            .await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    let unreachable = app
        .post("/api/v1/import/preview", json!({ "source": "plex", "server_url": "http://127.0.0.1:9", "token": "t", "user_id": me }), Some(&admin))
        .await;
    assert_eq!(unreachable.status, StatusCode::BAD_GATEWAY);
}

fn jellyfin_items(items: Value) -> ResponseTemplate {
    let count = items.as_array().unwrap().len();
    ResponseTemplate::new(200)
        .set_body_json(json!({ "Items": items, "TotalRecordCount": count, "StartIndex": 0 }))
}

async fn jellyfin_user_items(server: &MockServer, user: &str, token: &str) {
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("userId", user))
        .and(query_param("IncludeItemTypes", "Series"))
        .and(header("X-Emby-Token", token))
        .respond_with(jellyfin_items(json!([
            { "Name": "Dark", "ServerId": "srv", "Id": "s-dark", "Type": "Series", "ProductionYear": 2017,
              "ProviderIds": { "Tvdb": "334824", "Imdb": "tt5753856", "Tmdb": "70523" }, "IsFolder": true }
        ])))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("userId", user))
        .and(query_param("IncludeItemTypes", "Movie,Episode"))
        .and(header("X-Emby-Token", token))
        .respond_with(jellyfin_items(json!([
            { "Name": "Matrix", "ServerId": "srv", "Id": "m1", "Type": "Movie", "ProductionYear": 1999,
              "RunTimeTicks": 81_960_000_000i64, "ProviderIds": { "Tmdb": "603", "Imdb": "tt0133093" },
              "UserData": { "PlaybackPositionTicks": 0, "PlayCount": 2, "IsFavorite": true, "Played": true,
                            "LastPlayedDate": "2024-03-01T19:00:00.0000000Z", "Key": "603" } },
            { "Name": "Alpha", "ServerId": "srv", "Id": "m2", "Type": "Movie", "ProductionYear": 2020,
              "RunTimeTicks": 60_000_000_000i64, "ProviderIds": {},
              "UserData": { "PlaybackPositionTicks": 0, "PlayedPercentage": 25.0, "PlayCount": 0, "IsFavorite": false, "Played": false } },
            { "Name": "Lügen", "ServerId": "srv", "Id": "e2", "Type": "Episode", "SeriesName": "Dark", "SeriesId": "s-dark",
              "ParentIndexNumber": 1, "IndexNumber": 2, "ProviderIds": { "Tvdb": "6327752" },
              "UserData": { "PlaybackPositionTicks": 0, "PlayCount": 1, "IsFavorite": false, "Played": true,
                            "LastPlayedDate": "2024-03-02T20:00:00.0000000Z" } },
            { "Name": "Ungesehen", "ServerId": "srv", "Id": "m3", "Type": "Movie", "ProductionYear": 2010,
              "ProviderIds": { "Tmdb": "5" },
              "UserData": { "PlaybackPositionTicks": 0, "PlayCount": 0, "IsFavorite": false, "Played": false } }
        ])))
        .mount(server)
        .await;
}

#[tokio::test]
async fn jellyfin_users_by_api_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/Users"))
        .and(header("X-Emby-Token", "api-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "Name": "Felix", "ServerId": "srv", "Id": "u-felix", "HasPassword": true, "Policy": { "IsAdministrator": true } },
            { "Name": "Gast", "ServerId": "srv", "Id": "u-gast", "HasPassword": false },
            { "Name": "Oma", "ServerId": "srv", "Id": "u-oma", "HasPassword": false }
        ])))
        .mount(&server)
        .await;
    for user in ["u-felix", "u-gast", "u-oma"] {
        jellyfin_user_items(&server, user, "api-key").await;
    }
    let app = app().await;
    let f = library(&app).await;
    let request = json!({
        "source": "jellyfin", "server_url": server.uri(), "token": "api-key",
        "user_map": [{ "source_user_id": "u-gast", "user_id": f.kid_id }]
    });

    let preview = app
        .post("/api/v1/import/preview", request.clone(), Some(&f.admin))
        .await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.json);
    let users = preview.json["users"].as_array().unwrap();
    assert_eq!(users.len(), 3);
    assert_eq!(users[0]["source_user_name"], "Felix");
    assert_eq!(users[0]["user_id"], f.admin_id, "mapped by name");
    assert_eq!(users[1]["user_id"], f.kid_id, "mapped explicitly");
    assert!(users[2]["user_id"].is_null(), "no Flixnet user called Oma");
    assert_eq!(
        users[0]["stats"],
        json!({
            "entries": 3, "matched": 3, "unmatched": 0, "by_provider_id": 1, "by_title_year": 1,
            "by_episode": 1, "played": 2, "in_progress": 1, "favorites": 1, "rated": 0
        })
    );

    let applied = app
        .post("/api/v1/import/apply", request, Some(&f.admin))
        .await;
    assert_eq!(applied.status, StatusCode::OK, "{}", applied.json);
    assert_eq!(applied.json["users"][0]["written_items"], 3);
    assert!(applied.json["users"][2]["written_items"].is_null());
    for user in [f.admin_id, f.kid_id] {
        let matrix = userdata::get(&app.db, user, f.matrix).await.unwrap();
        assert!(matrix.played && matrix.favorite);
        assert_eq!(matrix.play_count, 2);
        // 25 % of a 100-minute movie.
        assert_eq!(
            userdata::get(&app.db, user, f.alpha)
                .await
                .unwrap()
                .position_ms,
            1_500_000
        );
        assert!(
            userdata::get(&app.db, user, f.dark_e2)
                .await
                .unwrap()
                .played
        );
        assert!(
            !userdata::get(&app.db, user, f.dark_e1)
                .await
                .unwrap()
                .played
        );
    }
    let history = playstate::events(&app.db, f.kid_id).await.unwrap();
    assert_eq!(history.len(), 2);
    assert!(
        history
            .iter()
            .all(|e| e.client.as_deref() == Some("Jellyfin-Import"))
    );
}

#[tokio::test]
async fn jellyfin_sign_in_imports_that_user() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "User": { "Name": "Felix", "ServerId": "srv", "Id": "u-felix" },
            "SessionInfo": { "Id": "sess" }, "AccessToken": "user-token", "ServerId": "srv"
        })))
        .mount(&server)
        .await;
    jellyfin_user_items(&server, "u-felix", "user-token").await;
    let app = app().await;
    let f = library(&app).await;

    let preview = app
        .post(
            "/api/v1/import/preview",
            json!({ "source": "jellyfin", "server_url": server.uri(), "username": "Felix", "password": "pw", "user_id": f.kid_id }),
            Some(&f.admin),
        )
        .await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.json);
    let users = preview.json["users"].as_array().unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0]["user_id"], f.kid_id);
    assert_eq!(users[0]["stats"]["matched"], 3);
    let login = &server.received_requests().await.unwrap()[0];
    let body: Value = serde_json::from_slice(&login.body).unwrap();
    assert_eq!(body, json!({ "Username": "Felix", "Pw": "pw" }));
    assert!(
        login
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("MediaBrowser Client=\"Flixnet\"")
    );

    // Non-administrators cannot import.
    let login = app
        .post(
            "/api/v1/auth/login",
            common::credentials("Kind", PASSWORD, "kid"),
            None,
        )
        .await;
    let kid = login.json["token"].as_str().unwrap();
    let denied = app
        .post(
            "/api/v1/import/preview",
            json!({ "source": "jellyfin", "server_url": server.uri() }),
            Some(kid),
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
}

// ---- Real Jellyfin payloads -----------------------------------------------------------
//
// `fixtures/jellyfin_import/*.json` are trimmed responses of a real Jellyfin 12.4
// (`jellyfin/jellyfin:12.4.20261026-211555`) for the library built below: movies with
// NFO ids, one movie without any metadata (Jellyfin names it after its folder,
// "Ein Film ohne IDs (2015)"), a series with three episodes without episode ids, a
// favorite series, and watch state set through Jellyfin's API.

const JF_FELIX: &str = "35354f0f467b4fe18d4b8334e889599a";
const JF_GAST: &str = "2c1341e56530432f977d461289d8bc92";

fn fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/jellyfin_import")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

struct RealLibrary {
    admin: String,
    admin_id: i64,
    gast_id: i64,
    ids: std::collections::HashMap<&'static str, i64>,
}

async fn real_library(app: &TestApp) -> RealLibrary {
    let root = app.dir.path().join("media");
    for file in [
        "movies/The Matrix (1999) {tmdb-603}/The Matrix (1999).mkv",
        "movies/Inception (2010) {tmdb-27205}/Inception (2010).mkv",
        "movies/Alien (1979) {imdb-tt0078748}/Alien (1979).mkv",
        "movies/Ein Film ohne IDs (2015)/Ein Film ohne IDs (2015).mkv",
        "shows/Dark (2017) {tvdb-334824}/Season 01/Dark S01E01.mkv",
        "shows/Dark (2017) {tvdb-334824}/Season 01/Dark S01E02.mkv",
        "shows/Dark (2017) {tvdb-334824}/Season 01/Dark S01E03.mkv",
    ] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"v").unwrap();
    }
    let queue = JobRuntime::new(app.db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    for (name, kind, dir) in [("Filme", "movies", "movies"), ("Serien", "shows", "shows")] {
        let path = root.join(dir).to_string_lossy().into_owned();
        let id = libraries::create(&app.db, name, kind, &[path])
            .await
            .unwrap()
            .id;
        scan_library(&app.db, &queue, id).await.unwrap();
    }
    let admin = app.setup_admin().await;
    let gast = app
        .post(
            "/api/v1/users",
            json!({ "name": "Gast", "password": PASSWORD }),
            Some(&admin),
        )
        .await;
    let mut ids = std::collections::HashMap::new();
    for kind in ["movie", "series", "episode"] {
        let list = app
            .get(&format!("/api/v1/items?kind={kind}"), Some(&admin))
            .await
            .json;
        for item in list["items"].as_array().unwrap() {
            let title = item["title"].as_str().unwrap();
            let key = match (kind, item["index_number"].as_i64(), title) {
                ("episode", Some(1), _) => "e1",
                ("episode", Some(2), _) => "e2",
                ("episode", Some(3), _) => "e3",
                (_, _, "The Matrix") => "matrix",
                (_, _, "Inception") => "inception",
                (_, _, "Alien") => "alien",
                (_, _, "Ein Film ohne IDs") => "ohne",
                (_, _, "Dark") => "dark",
                _ => panic!("unexpected {kind} {title}"),
            };
            ids.insert(key, item["id"].as_i64().unwrap());
        }
    }
    assert_eq!(ids.len(), 8, "{ids:?}");
    let me = app.get("/api/v1/users/me", Some(&admin)).await.json;
    RealLibrary {
        admin_id: me["id"].as_i64().unwrap(),
        gast_id: gast.json["id"].as_i64().unwrap(),
        admin,
        ids,
    }
}

/// Serves the captured responses: `/Users`, `/Items?userId=…` and the sign-in.
async fn real_jellyfin() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/Users"))
        .and(header("X-Emby-Token", "real-api-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("users.json")))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("authenticate.json")))
        .mount(&server)
        .await;
    for (user, label) in [(JF_FELIX, "felix"), (JF_GAST, "gast")] {
        for (types, name) in [("Series", "series"), ("Movie,Episode", "videos")] {
            Mock::given(method("GET"))
                .and(path("/Items"))
                .and(query_param("userId", user))
                .and(query_param("IncludeItemTypes", types))
                .and(query_param("EnableUserData", "true"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(fixture(&format!("items_{label}_{name}.json"))),
                )
                .mount(&server)
                .await;
        }
    }
    server
}

#[tokio::test]
async fn jellyfin_real_payloads_by_api_key() {
    let server = real_jellyfin().await;
    let app = app().await;
    let lib = real_library(&app).await;
    let request =
        json!({ "source": "jellyfin", "server_url": server.uri(), "token": "real-api-key" });

    let preview = app
        .post("/api/v1/import/preview", request.clone(), Some(&lib.admin))
        .await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.json);
    let users = preview.json["users"].as_array().unwrap();
    assert_eq!(users.len(), 2);
    assert_eq!(users[0]["source_user_id"], JF_FELIX);
    assert_eq!(users[0]["user_id"], lib.admin_id);
    assert_eq!(users[1]["user_id"], lib.gast_id);
    // The movie without metadata ("Ein Film ohne IDs (2015)") is found by title and
    // year, the favorite series by its provider ids.
    assert_eq!(
        users[0]["stats"],
        json!({
            "entries": 8, "matched": 8, "unmatched": 0, "by_provider_id": 4, "by_title_year": 1,
            "by_episode": 3, "played": 4, "in_progress": 2, "favorites": 3, "rated": 0
        })
    );
    assert_eq!(users[0]["unmatched_samples"], json!([]));
    assert_eq!(
        users[1]["stats"],
        json!({
            "entries": 2, "matched": 2, "unmatched": 0, "by_provider_id": 1, "by_title_year": 0,
            "by_episode": 1, "played": 1, "in_progress": 0, "favorites": 1, "rated": 0
        })
    );

    let applied = app
        .post("/api/v1/import/apply", request.clone(), Some(&lib.admin))
        .await;
    assert_eq!(applied.status, StatusCode::OK, "{}", applied.json);
    assert_eq!(applied.json["users"][0]["written_items"], 8);
    assert_eq!(applied.json["users"][0]["written_history"], 4);
    assert_eq!(applied.json["users"][1]["written_history"], 1);

    let get = |user: i64, key: &str| {
        let db = app.db.clone();
        let id = lib.ids[key];
        async move { userdata::get(&db, user, id).await.unwrap() }
    };
    let matrix = get(lib.admin_id, "matrix").await;
    assert!(matrix.played && matrix.favorite);
    assert_eq!(matrix.play_count, 3);
    // "2026-03-01T19:00:00.0000000Z"
    assert_eq!(matrix.last_played_at, Some(1_772_391_600_000));
    let inception = get(lib.admin_id, "inception").await;
    assert!(!inception.played);
    assert_eq!(inception.position_ms, 60_000);
    assert!(get(lib.admin_id, "alien").await.favorite);
    assert!(get(lib.admin_id, "ohne").await.played);
    assert!(get(lib.admin_id, "dark").await.favorite);
    assert!(get(lib.admin_id, "e1").await.played);
    assert!(get(lib.admin_id, "e2").await.played);
    let e3 = get(lib.admin_id, "e3").await;
    assert!(!e3.played);
    assert_eq!(e3.position_ms, 45_000);
    // Nothing of Felix leaked into Gast and vice versa.
    assert!(get(lib.gast_id, "inception").await.played);
    assert!(get(lib.gast_id, "e3").await.favorite);
    assert!(!get(lib.gast_id, "matrix").await.played);
    assert!(!get(lib.gast_id, "dark").await.favorite);

    // Repeating changes nothing.
    let again = app
        .post("/api/v1/import/apply", request, Some(&lib.admin))
        .await;
    assert_eq!(again.json["users"][0]["written_history"], 0);
    assert_eq!(again.json["users"][1]["written_history"], 0);
    assert_eq!(
        playstate::events(&app.db, lib.admin_id)
            .await
            .unwrap()
            .len(),
        4
    );
    assert_eq!(get(lib.admin_id, "matrix").await.play_count, 3);
    assert_eq!(get(lib.admin_id, "inception").await.position_ms, 60_000);
}

#[tokio::test]
async fn jellyfin_real_payloads_by_sign_in() {
    let server = real_jellyfin().await;
    let app = app().await;
    let lib = real_library(&app).await;
    let request = json!({
        "source": "jellyfin", "server_url": server.uri(), "username": "Felix", "password": "pw",
        "user_id": lib.gast_id
    });
    let applied = app
        .post("/api/v1/import/apply", request, Some(&lib.admin))
        .await;
    assert_eq!(applied.status, StatusCode::OK, "{}", applied.json);
    let users = applied.json["users"].as_array().unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0]["source_user_id"], JF_FELIX);
    assert_eq!(users[0]["user_id"], lib.gast_id);
    assert_eq!(users[0]["stats"]["matched"], 8);
    assert_eq!(users[0]["written_history"], 4);
    let items: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == "/Items")
        .collect();
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .all(|r| r.headers.get("x-emby-token").unwrap() == "jellyfin-user-token")
    );
    let matrix = userdata::get(&app.db, lib.gast_id, lib.ids["matrix"])
        .await
        .unwrap();
    assert!(matrix.played && matrix.favorite);
}
