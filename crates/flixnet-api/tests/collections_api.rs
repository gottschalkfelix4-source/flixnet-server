//! Milestone 5.3: collections (automatic from a mocked TMDB, and manual) and playlists
//! including smart playlists, with library access and age limits.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use common::{PASSWORD, TestApp, app, credentials};
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::collections::{self, ProviderCollection};
use flixnet_db::{items, libraries, userdata};
use flixnet_library::providers::{self, Tmdb, TmdbConfig};
use flixnet_library::refresh::refresh_local;
use flixnet_library::scan::scan_library;
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn write(root: &std::path::Path, relative: &str, content: &[u8]) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

async fn scan(app: &TestApp, name: &str, kind: &str, root: &std::path::Path) -> i64 {
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
    id
}

/// Creates a user (optionally with an age limit) and signs them in.
async fn user(app: &TestApp, admin: &str, name: &str, max_age: Option<i64>) -> (i64, String) {
    let created = app
        .post(
            "/api/v1/users",
            json!({ "name": name, "password": PASSWORD, "max_age_rating": max_age }),
            Some(admin),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    let token = app
        .post(
            "/api/v1/auth/login",
            credentials(name, PASSWORD, &format!("device-{name}")),
            None,
        )
        .await
        .json["token"]
        .as_str()
        .unwrap()
        .to_owned();
    (created.json["id"].as_i64().unwrap(), token)
}

fn titles(items: &Value) -> Vec<String> {
    items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            i.get("item").unwrap_or(i)["title"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

async fn movie_id(app: &TestApp, library: i64, title: &str) -> i64 {
    items::list_by_kind(&app.db, library, "movie")
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.title == title)
        .unwrap_or_else(|| panic!("no movie {title}"))
        .id
}

fn tmdb_movie(id: i64, title: &str, date: &str, fsk: &str, collection: Option<Value>) -> Value {
    json!({
        "id": id, "title": title, "original_title": title,
        "overview": format!("Über {title}."), "tagline": "Ein Slogan.",
        "runtime": 100, "release_date": date, "vote_average": 7.0,
        "release_dates": { "results": [{ "iso_3166_1": "DE", "release_dates": [{ "certification": fsk }] }] },
        "belongs_to_collection": collection,
    })
}

async fn tmdb_mock() -> MockServer {
    let server = MockServer::start().await;
    let ok = |body: Value| ResponseTemplate::new(200).set_body_json(body);
    let reference = json!({ "id": 4200, "name": "Alpha Filmreihe", "poster_path": "/alpha-col.jpg", "backdrop_path": null });
    for (id, title, date, fsk) in [
        (900_001, "Testfilm Alpha", "2021-03-04", "12"),
        (900_002, "Testfilm Alpha 2", "2023-05-01", "18"),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/movie/{id}")))
            .respond_with(ok(tmdb_movie(
                id,
                title,
                date,
                fsk,
                Some(reference.clone()),
            )))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/movie/900010"))
        .respond_with(ok(tmdb_movie(
            900_010,
            "Einzelstück",
            "2010-01-01",
            "0",
            None,
        )))
        .mount(&server)
        .await;
    // Parts out of order; the third is not in the library.
    Mock::given(method("GET"))
        .and(path("/collection/4200"))
        .respond_with(ok(json!({
            "id": 4200, "name": "Alpha Filmreihe", "overview": "Alle Alpha-Filme.",
            "poster_path": "/alpha-col.jpg", "backdrop_path": "/alpha-back.jpg",
            "parts": [
                { "id": 900_003, "title": "Testfilm Alpha 3", "release_date": "2026-12-24" },
                { "id": 900_002, "title": "Testfilm Alpha 2", "release_date": "2023-05-01" },
                { "id": 900_001, "title": "Testfilm Alpha", "release_date": "2021-03-04" }
            ]
        })))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn automatic_collections_from_tmdb_respect_access_and_age_limits() {
    let server = tmdb_mock().await;
    let app = app().await;
    let admin = app.setup_admin().await;
    let media = tempfile::tempdir().unwrap();
    let root = media.path().join("Filme");
    write(
        &root,
        "Testfilm Alpha 2 (2023) {tmdb-900002}/Testfilm.Alpha.2.2023.mkv",
        b"v",
    );
    write(
        &root,
        "Testfilm Alpha (2021) {tmdb-900001}/Testfilm.Alpha.2021.mkv",
        b"v",
    );
    write(
        &root,
        "Einzelstueck (2010) {tmdb-900010}/Einzelstueck.2010.mkv",
        b"v",
    );
    let library = scan(&app, "Filme", "movies", &root).await;

    let mut config = TmdbConfig::new("test-key");
    config.base_url = server.uri();
    let tmdb = Tmdb::new(config, app.db.clone());
    for item in items::list_by_kind(&app.db, library, "movie")
        .await
        .unwrap()
    {
        providers::refresh_remote(&app.db, &tmdb, item.id)
            .await
            .unwrap();
        userdata::update_identity(&app.db, item.id).await.unwrap();
    }
    let alpha = movie_id(&app, library, "Testfilm Alpha").await;
    let alpha2 = movie_id(&app, library, "Testfilm Alpha 2").await;
    let single = movie_id(&app, library, "Einzelstück").await;

    // One automatic collection (both movies share it), members in release order.
    let list = app.get("/api/v1/collections", Some(&admin)).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.json);
    assert_eq!(list.json.as_array().unwrap().len(), 1, "{}", list.json);
    let col = &list.json[0];
    assert_eq!(col["name"], "Alpha Filmreihe");
    assert_eq!(col["kind"], "auto");
    assert_eq!(col["tmdb_id"], 4200);
    assert_eq!(
        col["item_count"], 2,
        "the unreleased part is not in the library"
    );
    assert_eq!(col["played_count"], 0);
    assert!(
        col["poster_url"]
            .as_str()
            .unwrap()
            .contains("/collections/")
    );
    let id = col["id"].as_i64().unwrap();
    let detail = app
        .get(&format!("/api/v1/collections/{id}"), Some(&admin))
        .await;
    assert_eq!(detail.json["overview"], "Alle Alpha-Filme.");
    assert_eq!(
        titles(&detail.json["items"]),
        ["Testfilm Alpha", "Testfilm Alpha 2"]
    );

    // "Teil der Sammlung" on the movie, none for the standalone one.
    let of_movie = app
        .get(&format!("/api/v1/items/{alpha}/collections"), Some(&admin))
        .await;
    assert_eq!(of_movie.json[0]["name"], "Alpha Filmreihe");
    assert_eq!(of_movie.json[0]["items"].as_array().unwrap().len(), 2);
    let none = app
        .get(&format!("/api/v1/items/{single}/collections"), Some(&admin))
        .await;
    assert_eq!(none.json, json!([]));

    // Watched state shows on the collection.
    app.request(
        Method::PUT,
        &format!("/api/v1/items/{alpha}/played"),
        None,
        Some(&admin),
    )
    .await;
    let detail = app
        .get(&format!("/api/v1/collections/{id}"), Some(&admin))
        .await;
    assert_eq!(detail.json["played_count"], 1);
    assert_eq!(detail.json["items"][0]["played"], true);

    // A 12-year-old sees only the FSK 12 part.
    let (_, kid) = user(&app, &admin, "Kind", Some(12)).await;
    let kid_view = app
        .get(&format!("/api/v1/collections/{id}"), Some(&kid))
        .await;
    assert_eq!(titles(&kid_view.json["items"]), ["Testfilm Alpha"]);
    assert_eq!(kid_view.json["item_count"], 1);
    assert_eq!(
        kid_view.json["played_count"], 0,
        "watched state is per user"
    );
    assert_eq!(
        app.get(&format!("/api/v1/items/{alpha2}/collections"), Some(&kid))
            .await
            .status,
        StatusCode::NOT_FOUND,
        "the FSK 18 movie itself is hidden"
    );

    // Without access to the library the collection does not exist.
    let (guest_id, guest) = user(&app, &admin, "Gast", None).await;
    app.request(
        Method::PUT,
        &format!("/api/v1/users/{guest_id}/libraries"),
        Some(json!({ "all_libraries": false, "library_ids": [] })),
        Some(&admin),
    )
    .await;
    assert_eq!(
        app.get("/api/v1/collections", Some(&guest)).await.json,
        json!([])
    );
    assert_eq!(
        app.get(&format!("/api/v1/collections/{id}"), Some(&guest))
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // Automatic collections are not edited by hand.
    let edit = app
        .request(
            Method::PATCH,
            &format!("/api/v1/collections/{id}"),
            Some(json!({ "name": "Umbenannt" })),
            Some(&admin),
        )
        .await;
    assert_eq!(edit.status, StatusCode::CONFLICT);
    let add = app
        .post(
            &format!("/api/v1/collections/{id}/items"),
            json!({ "item_ids": [single] }),
            Some(&admin),
        )
        .await;
    assert_eq!(add.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn collection_artwork_is_signed_and_access_controlled() {
    let server = MockServer::start().await;
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(400, 600, image::Rgb([200, 40, 40]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    Mock::given(method("GET"))
        .and(path("/poster.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(png.into_inner()))
        .mount(&server)
        .await;

    let app = app().await;
    let admin = app.setup_admin().await;
    let media = tempfile::tempdir().unwrap();
    let root = media.path().join("Filme");
    write(&root, "Teil Eins (2001)/Teil.Eins.2001.mkv", b"v");
    let library = scan(&app, "Filme", "movies", &root).await;
    let movie = movie_id(&app, library, "Teil Eins").await;
    let identity = userdata::identity_key(&app.db, movie)
        .await
        .unwrap()
        .unwrap();
    let id = collections::sync_provider(
        &app.db,
        &ProviderCollection {
            provider: "tmdb".into(),
            provider_id: "77".into(),
            name: "Reihe".into(),
            sort_name: "reihe".into(),
            poster_url: Some(format!("{}/poster.png", server.uri())),
            ..Default::default()
        },
        &identity,
    )
    .await
    .unwrap();

    let detail = app
        .get(&format!("/api/v1/collections/{id}"), Some(&admin))
        .await;
    let url = detail.json["poster_url"].as_str().unwrap().to_owned();
    let get = |uri: String| {
        let router = app.router.clone();
        async move {
            router
                .oneshot(Request::get(uri).body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(get(url.clone()).await, StatusCode::OK);
    assert!(
        collections::get(&app.db, id)
            .await
            .unwrap()
            .unwrap()
            .poster_blurhash
            .is_some(),
        "blurhash stored on first render"
    );
    assert_eq!(
        get(format!("/api/v1/collections/{id}/images/poster")).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(url.replace("sig=", "sig=0")).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(url.replace("/poster?", "/backdrop?")).await,
        StatusCode::UNAUTHORIZED,
        "a poster signature does not open the backdrop"
    );
    assert_eq!(
        app.get(
            &format!("/api/v1/collections/{id}/images/backdrop"),
            Some(&admin)
        )
        .await
        .status,
        StatusCode::NOT_FOUND,
        "no backdrop"
    );
}

#[tokio::test]
async fn manual_collections_are_curated_by_administrators() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let media = tempfile::tempdir().unwrap();
    let movies_root = media.path().join("Filme");
    write(&movies_root, "Sommer (2015)/Sommer.2015.mkv", b"v");
    write(
        &movies_root,
        "Sommer (2015)/movie.nfo",
        b"<movie><title>Sommer</title><year>2015</year><premiered>2015-06-01</premiered><mpaa>FSK 6</mpaa></movie>",
    );
    write(&movies_root, "Winter (2012)/Winter.2012.mkv", b"v");
    write(
        &movies_root,
        "Winter (2012)/movie.nfo",
        b"<movie><title>Winter</title><year>2012</year><premiered>2012-12-01</premiered><mpaa>FSK 18</mpaa></movie>",
    );
    let shows_root = media.path().join("Serien");
    write(&shows_root, "Herbst/Season 01/Herbst.S01E01.mkv", b"v");
    write(
        &shows_root,
        "Herbst/tvshow.nfo",
        b"<tvshow><title>Herbst</title><mpaa>FSK 0</mpaa></tvshow>",
    );
    let movies = scan(&app, "Filme", "movies", &movies_root).await;
    let shows = scan(&app, "Serien", "shows", &shows_root).await;
    let sommer = movie_id(&app, movies, "Sommer").await;
    let winter = movie_id(&app, movies, "Winter").await;
    let herbst = items::list_by_kind(&app.db, shows, "series").await.unwrap()[0].id;
    let episode = items::list_by_kind(&app.db, shows, "episode")
        .await
        .unwrap()[0]
        .id;
    let (_, kid) = user(&app, &admin, "Kind", Some(12)).await;

    let forbidden = app
        .post(
            "/api/v1/collections",
            json!({ "name": "Meins" }),
            Some(&kid),
        )
        .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);
    let blank = app
        .post("/api/v1/collections", json!({ "name": "  " }), Some(&admin))
        .await;
    assert_eq!(blank.status, StatusCode::UNPROCESSABLE_ENTITY);
    let created = app
        .post(
            "/api/v1/collections",
            json!({ "name": "Jahreszeiten", "overview": "Vier Stück." }),
            Some(&admin),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    assert_eq!(created.json["kind"], "manual");
    let id = created.json["id"].as_i64().unwrap();

    // Empty: the admin sees it (to fill it), others do not.
    assert_eq!(
        app.get("/api/v1/collections", Some(&admin))
            .await
            .json
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        app.get(&format!("/api/v1/collections/{id}"), Some(&kid))
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    let episode_add = app
        .post(
            &format!("/api/v1/collections/{id}/items"),
            json!({ "item_ids": [episode] }),
            Some(&admin),
        )
        .await;
    assert_eq!(episode_add.status, StatusCode::UNPROCESSABLE_ENTITY);
    let add = app
        .post(
            &format!("/api/v1/collections/{id}/items"),
            json!({ "item_ids": [sommer, herbst, winter] }),
            Some(&admin),
        )
        .await;
    assert_eq!(add.status, StatusCode::OK, "{}", add.json);
    assert_eq!(add.json["added"], 3);
    let kid_add = app
        .post(
            &format!("/api/v1/collections/{id}/items"),
            json!({ "item_ids": [sommer] }),
            Some(&kid),
        )
        .await;
    assert_eq!(kid_add.status, StatusCode::FORBIDDEN);

    let detail = app
        .get(&format!("/api/v1/collections/{id}"), Some(&admin))
        .await;
    assert_eq!(
        titles(&detail.json["items"]),
        ["Winter", "Sommer", "Herbst"],
        "release order; undated last"
    );
    assert!(detail.json["poster_url"].is_null() || detail.json["poster_url"].is_string());
    let kid_view = app
        .get(&format!("/api/v1/collections/{id}"), Some(&kid))
        .await;
    assert_eq!(titles(&kid_view.json["items"]), ["Sommer", "Herbst"]);

    let renamed = app
        .request(
            Method::PATCH,
            &format!("/api/v1/collections/{id}"),
            Some(json!({ "name": "Jahreszeiten 2", "overview": null })),
            Some(&admin),
        )
        .await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.json);
    assert_eq!(renamed.json["name"], "Jahreszeiten 2");
    assert!(renamed.json["overview"].is_null());

    let removed = app
        .request(
            Method::DELETE,
            &format!("/api/v1/collections/{id}/items/{winter}"),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    let again = app
        .request(
            Method::DELETE,
            &format!("/api/v1/collections/{id}/items/{winter}"),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);

    let deleted = app
        .request(
            Method::DELETE,
            &format!("/api/v1/collections/{id}"),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.get(&format!("/api/v1/collections/{id}"), Some(&admin))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn playlists_are_private_unless_shared_and_keep_their_order() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let media = tempfile::tempdir().unwrap();
    let movies_root = media.path().join("Filme");
    for (title, fsk, genre) in [
        ("Erster", "FSK 0", "Abenteuer"),
        ("Zweiter", "FSK 18", "Horror"),
        ("Dritter", "FSK 6", "Horror"),
    ] {
        write(&movies_root, &format!("{title}/{title}.mkv"), b"v");
        write(
            &movies_root,
            &format!("{title}/movie.nfo"),
            format!(
                "<movie><title>{title}</title><mpaa>{fsk}</mpaa><genre>{genre}</genre></movie>"
            )
            .as_bytes(),
        );
    }
    let shows_root = media.path().join("Serien");
    write(&shows_root, "Serie/Season 01/Serie.S01E02.mkv", b"v");
    write(&shows_root, "Serie/Season 01/Serie.S01E01.mkv", b"v");
    write(
        &shows_root,
        "Serie/tvshow.nfo",
        b"<tvshow><title>Serie</title><mpaa>FSK 0</mpaa></tvshow>",
    );
    let movies = scan(&app, "Filme", "movies", &movies_root).await;
    let shows = scan(&app, "Serien", "shows", &shows_root).await;
    let first = movie_id(&app, movies, "Erster").await;
    let second = movie_id(&app, movies, "Zweiter").await;
    let third = movie_id(&app, movies, "Dritter").await;
    let series = items::list_by_kind(&app.db, shows, "series").await.unwrap()[0].id;
    let (_, anna) = user(&app, &admin, "Anna", None).await;
    let (_, kid) = user(&app, &admin, "Kind", Some(12)).await;

    let created = app
        .post(
            "/api/v1/playlists",
            json!({ "name": "Filmabend" }),
            Some(&anna),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    assert_eq!(created.json["is_owner"], true);
    assert_eq!(created.json["smart"], false);
    let id = created.json["id"].as_i64().unwrap();

    let add = app
        .post(
            &format!("/api/v1/playlists/{id}/items"),
            json!({ "item_ids": [second, series, first, second] }),
            Some(&anna),
        )
        .await;
    assert_eq!(add.status, StatusCode::OK, "{}", add.json);
    assert_eq!(
        add.json["added"], 4,
        "series expands to its two episodes; no duplicates"
    );
    let detail = app
        .get(&format!("/api/v1/playlists/{id}"), Some(&anna))
        .await;
    let entries = detail.json["entries"].as_array().unwrap().clone();
    let labels: Vec<_> = entries
        .iter()
        .map(|e| {
            (
                e["item"]["kind"].as_str().unwrap().to_owned(),
                e["item"]["index_number"].as_i64(),
            )
        })
        .collect();
    assert_eq!(
        labels,
        [
            ("movie".to_owned(), None),
            ("episode".to_owned(), Some(1)),
            ("episode".to_owned(), Some(2)),
            ("movie".to_owned(), None)
        ]
    );
    assert_eq!(detail.json["item_count"], 4);
    let entry = |i: usize| entries[i]["entry_id"].as_i64().unwrap();

    // Reorder: the last entry first, the rest keeps its order.
    let reorder = app
        .request(
            Method::PUT,
            &format!("/api/v1/playlists/{id}/order"),
            Some(json!({ "entry_ids": [entry(3)] })),
            Some(&anna),
        )
        .await;
    assert_eq!(reorder.status, StatusCode::NO_CONTENT);
    let detail = app
        .get(&format!("/api/v1/playlists/{id}"), Some(&anna))
        .await;
    assert_eq!(titles(&detail.json["entries"])[..2], ["Erster", "Zweiter"]);
    let bad_order = app
        .request(
            Method::PUT,
            &format!("/api/v1/playlists/{id}/order"),
            Some(json!({ "entry_ids": [entry(0), entry(0)] })),
            Some(&anna),
        )
        .await;
    assert_eq!(bad_order.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Private: other users (administrators too) get 404 everywhere.
    for token in [&admin, &kid] {
        assert_eq!(
            app.get(&format!("/api/v1/playlists/{id}"), Some(token))
                .await
                .status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            app.post(
                &format!("/api/v1/playlists/{id}/items"),
                json!({ "item_ids": [first] }),
                Some(token)
            )
            .await
            .status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            app.get("/api/v1/playlists", Some(token))
                .await
                .json
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    // Shared: visible to everyone, read-only, filtered by the viewer's age limit.
    let shared = app
        .request(
            Method::PATCH,
            &format!("/api/v1/playlists/{id}"),
            Some(json!({ "shared": true, "name": "Familienabend" })),
            Some(&anna),
        )
        .await;
    assert_eq!(shared.status, StatusCode::OK, "{}", shared.json);
    assert_eq!(shared.json["shared"], true);
    let kid_view = app
        .get(&format!("/api/v1/playlists/{id}"), Some(&kid))
        .await;
    assert_eq!(kid_view.status, StatusCode::OK);
    assert_eq!(kid_view.json["is_owner"], false);
    assert_eq!(kid_view.json["owner_name"], "Anna");
    assert!(
        !titles(&kid_view.json["entries"]).contains(&"Zweiter".to_owned()),
        "FSK 18 entry hidden from the child"
    );
    assert_eq!(kid_view.json["item_count"], 3);
    assert_eq!(
        app.get("/api/v1/playlists", Some(&kid)).await.json[0]["name"],
        "Familienabend"
    );
    for (method, uri, body) in [
        (
            Method::PATCH,
            format!("/api/v1/playlists/{id}"),
            Some(json!({ "name": "Meins" })),
        ),
        (Method::DELETE, format!("/api/v1/playlists/{id}"), None),
        (
            Method::POST,
            format!("/api/v1/playlists/{id}/items"),
            Some(json!({ "item_ids": [third] })),
        ),
        (
            Method::DELETE,
            format!("/api/v1/playlists/{id}/items/{}", entry(0)),
            None,
        ),
        (
            Method::PUT,
            format!("/api/v1/playlists/{id}/order"),
            Some(json!({ "entry_ids": [] })),
        ),
    ] {
        assert_eq!(
            app.request(method.clone(), &uri, body, Some(&kid))
                .await
                .status,
            StatusCode::FORBIDDEN,
            "{method} {uri}"
        );
    }

    // Removing entries and deleting the playlist (owner).
    let remove = app
        .request(
            Method::DELETE,
            &format!("/api/v1/playlists/{id}/items/{}", entry(0)),
            None,
            Some(&anna),
        )
        .await;
    assert_eq!(remove.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.get(&format!("/api/v1/playlists/{id}"), Some(&anna))
            .await
            .json["item_count"],
        3
    );
    // The kid cannot add what they may not see.
    let own = app
        .post(
            "/api/v1/playlists",
            json!({ "name": "Kinderliste" }),
            Some(&kid),
        )
        .await
        .json["id"]
        .as_i64()
        .unwrap();
    assert_eq!(
        app.post(
            &format!("/api/v1/playlists/{own}/items"),
            json!({ "item_ids": [second] }),
            Some(&kid)
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let delete = app
        .request(
            Method::DELETE,
            &format!("/api/v1/playlists/{id}"),
            None,
            Some(&anna),
        )
        .await;
    assert_eq!(delete.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.get(&format!("/api/v1/playlists/{id}"), Some(&anna))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn smart_playlists_evaluate_their_filter_live() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let media = tempfile::tempdir().unwrap();
    let root = media.path().join("Filme");
    for (title, genre, rating, year) in [
        ("Geist", "Horror", "6.0", 2001),
        ("Spuk", "Horror", "8.5", 2011),
        ("Lachen", "Komödie", "9.0", 2005),
        ("Nebel", "Thriller", "7.5", 2020),
    ] {
        write(&root, &format!("{title} ({year})/{title}.mkv"), b"v");
        write(
            &root,
            &format!("{title} ({year})/movie.nfo"),
            format!(
                "<movie><title>{title}</title><year>{year}</year><genre>{genre}</genre><rating>{rating}</rating><mpaa>FSK 12</mpaa></movie>"
            )
            .as_bytes(),
        );
    }
    let library = scan(&app, "Filme", "movies", &root).await;
    let spuk = movie_id(&app, library, "Spuk").await;

    let invalid = app
        .post(
            "/api/v1/playlists",
            json!({ "name": "Kaputt", "smart_filter": { "kinds": ["album"] } }),
            Some(&admin),
        )
        .await;
    assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);

    let created = app
        .post(
            "/api/v1/playlists",
            json!({
                "name": "Ungesehener Grusel",
                "smart_filter": {
                    "kinds": ["movie"], "genres": ["Horror", "Thriller"], "played": false,
                    "min_rating": 7.0, "sort": "rating", "order": "desc", "limit": 10
                }
            }),
            Some(&admin),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    assert_eq!(created.json["smart"], true);
    assert_eq!(
        created.json["smart_filter"]["genres"],
        json!(["Horror", "Thriller"])
    );
    let id = created.json["id"].as_i64().unwrap();

    let detail = app
        .get(&format!("/api/v1/playlists/{id}"), Some(&admin))
        .await;
    assert_eq!(titles(&detail.json["entries"]), ["Spuk", "Nebel"]);
    assert!(detail.json["entries"][0]["entry_id"].is_null());

    // Live: watching a movie removes it from "unwatched".
    app.request(
        Method::PUT,
        &format!("/api/v1/items/{spuk}/played"),
        None,
        Some(&admin),
    )
    .await;
    let detail = app
        .get(&format!("/api/v1/playlists/{id}"), Some(&admin))
        .await;
    assert_eq!(titles(&detail.json["entries"]), ["Nebel"]);
    assert_eq!(detail.json["item_count"], 1);

    // Smart playlists fill themselves.
    let add = app
        .post(
            &format!("/api/v1/playlists/{id}/items"),
            json!({ "item_ids": [spuk] }),
            Some(&admin),
        )
        .await;
    assert_eq!(add.status, StatusCode::CONFLICT);

    // The filter can be changed; regular playlists have none.
    let updated = app
        .request(
            Method::PATCH,
            &format!("/api/v1/playlists/{id}"),
            Some(json!({ "smart_filter": { "kinds": ["movie"], "sort": "year", "limit": 2 } })),
            Some(&admin),
        )
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.json);
    assert_eq!(updated.json["item_count"], 2, "limited to 2");
    let detail = app
        .get(&format!("/api/v1/playlists/{id}"), Some(&admin))
        .await;
    assert_eq!(titles(&detail.json["entries"]), ["Geist", "Lachen"]);
    let plain = app
        .post(
            "/api/v1/playlists",
            json!({ "name": "Normal" }),
            Some(&admin),
        )
        .await
        .json["id"]
        .as_i64()
        .unwrap();
    let wrong = app
        .request(
            Method::PATCH,
            &format!("/api/v1/playlists/{plain}"),
            Some(json!({ "smart_filter": {} })),
            Some(&admin),
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
}
