//! Milestone 5.4 (Jugendschutz): a restricted profile never gets to see anything
//! above its limit, through any endpoint.
//!
//! The library has restricted content whose texts all contain the marker `Geheim`
//! (titles, plots, file names; episodes inherit the FSK 16 rating of their series),
//! plus content the child may see. The walk then calls every GET operation of the
//! OpenAPI spec with every id as the restricted profile and asserts the marker never
//! appears and no restricted file, image or subtitle is served.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::{PASSWORD, TestApp, app, credentials};
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::{items, libraries, media, metadata, playstate, userdata};
use flixnet_library::refresh::refresh_local;
use flixnet_library::scan::scan_library;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const MARKER: &str = "Geheim";

fn write(root: &std::path::Path, relative: &str, content: &[u8]) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn movie_nfo(title: &str, mpaa: Option<&str>, genre: &str, actor: &str) -> String {
    let mpaa = mpaa
        .map(|m| format!("<mpaa>{m}</mpaa>"))
        .unwrap_or_default();
    format!(
        "<movie><title>{title}</title>{mpaa}<genre>{genre}</genre><plot>Über {title}.</plot>\
         <actor><name>{actor}</name><role>Hauptrolle</role></actor>\
         <actor><name>Paula Pan</name><role>Nebenrolle</role></actor></movie>"
    )
}

struct Library {
    movies: i64,
    shows: i64,
}

/// Movies: Kinderfilm (FSK 0), Teenfilm (FSK 12), Geheimfilm (FSK 18), Geheim
/// Gruselfilm (FSK 12, Horror), Geheimnis ohne Freigabe (unrated). Series:
/// Geheimakte (FSK 16, two episodes), Kinderserie (FSK 6, two episodes).
async fn library(app: &TestApp, root: &std::path::Path) -> Library {
    let movies = root.join("Filme");
    for (dir, file, title, mpaa, genre, actor) in [
        (
            "Kinderfilm (2020)",
            "Kinderfilm.2020.mkv",
            "Kinderfilm",
            Some("FSK 0"),
            "Familie",
            "Anton Alt",
        ),
        (
            "Teenfilm (2021)",
            "Teenfilm.2021.mkv",
            "Teenfilm",
            Some("FSK 12"),
            "Abenteuer",
            "Anton Alt",
        ),
        (
            "Geheimfilm (2019)",
            "Geheimfilm.2019.mkv",
            "Geheimfilm",
            Some("FSK 18"),
            "Thriller",
            "Anton Alt",
        ),
        (
            "Geheim Gruselfilm (2018)",
            "Geheim.Gruselfilm.2018.mkv",
            "Geheim Gruselfilm",
            Some("FSK 12"),
            "Horror",
            "Berta Bein",
        ),
        (
            "Geheimnis (2017)",
            "Geheimnis.2017.mkv",
            "Geheimnis ohne Freigabe",
            None,
            "Drama",
            "Berta Bein",
        ),
    ] {
        write(&movies, &format!("{dir}/{file}"), b"video");
        write(
            &movies,
            &format!("{dir}/movie.nfo"),
            movie_nfo(title, mpaa, genre, actor).as_bytes(),
        );
        image::RgbImage::new(10, 15)
            .save(movies.join(dir).join("poster.png"))
            .unwrap();
    }
    let shows = root.join("Serien");
    for (dir, title, mpaa, genre, episodes) in [
        (
            "Geheimakte (2015)",
            "Geheimakte",
            "FSK 16",
            "Krimi",
            ["Geheimer Auftrag", "Geheime Spur"],
        ),
        (
            "Kinderserie (2016)",
            "Kinderserie",
            "FSK 6",
            "Familie",
            ["Der Ausflug", "Das Fest"],
        ),
    ] {
        write(
            &shows,
            &format!("{dir}/tvshow.nfo"),
            format!(
                "<tvshow><title>{title}</title><mpaa>{mpaa}</mpaa><genre>{genre}</genre></tvshow>"
            )
            .as_bytes(),
        );
        for (i, episode) in episodes.iter().enumerate() {
            let stem = format!("{title}.S01E0{}", i + 1);
            write(&shows, &format!("{dir}/Staffel 1/{stem}.mkv"), b"video");
            write(
                &shows,
                &format!("{dir}/Staffel 1/{stem}.nfo"),
                format!(
                    "<episodedetails><title>{episode}</title><season>1</season><episode>{}</episode>\
                     <plot>{episode} – eine Folge.</plot></episodedetails>",
                    i + 1
                )
                .as_bytes(),
            );
        }
    }

    let movies_id = libraries::create(
        &app.db,
        "Filme",
        "movies",
        &[movies.to_string_lossy().into_owned()],
    )
    .await
    .unwrap()
    .id;
    let shows_id = libraries::create(
        &app.db,
        "Serien",
        "shows",
        &[shows.to_string_lossy().into_owned()],
    )
    .await
    .unwrap()
    .id;
    let queue = JobRuntime::new(app.db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    for library in [movies_id, shows_id] {
        scan_library(&app.db, &queue, library).await.unwrap();
        for kind in ["movie", "series", "season", "episode"] {
            for item in items::list_by_kind(&app.db, library, kind).await.unwrap() {
                refresh_local(&app.db, item.id).await.unwrap();
            }
        }
    }
    Library {
        movies: movies_id,
        shows: shows_id,
    }
}

async fn item_id(app: &TestApp, library: i64, kind: &str, title: &str) -> i64 {
    items::list_by_kind(&app.db, library, kind)
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.title == title)
        .unwrap_or_else(|| panic!("no {kind} {title}"))
        .id
}

async fn user(app: &TestApp, admin: &str, body: Value) -> (i64, String) {
    let name = body["name"].as_str().unwrap().to_owned();
    let created = app.post("/api/v1/users", body, Some(admin)).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    let login = app
        .post(
            "/api/v1/auth/login",
            credentials(&name, PASSWORD, &format!("device-{name}")),
            None,
        )
        .await;
    assert_eq!(login.status, StatusCode::OK, "{}", login.json);
    (
        created.json["id"].as_i64().unwrap(),
        login.json["token"].as_str().unwrap().to_owned(),
    )
}

/// Status and raw body (images and streams are not JSON).
async fn raw(
    app: &TestApp,
    method: Method,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let request = match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string())),
        None => builder.body(Body::empty()),
    }
    .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// Every id the walk tries (items, files, images, people, collections, playlists and
/// users of the fixture all stay below this).
const MAX_ID: i64 = 80;

/// Ids of everything that carries the marker (items, also through their series),
/// their files and images; plus the largest id of all of them.
async fn restricted_ids(app: &TestApp, lib: &Library) -> (Vec<i64>, Vec<i64>, Vec<i64>, i64) {
    let mut all = Vec::new();
    for library in [lib.movies, lib.shows] {
        for kind in ["movie", "series", "season", "episode"] {
            all.extend(items::list_by_kind(&app.db, library, kind).await.unwrap());
        }
    }
    let marked: Vec<i64> = all
        .iter()
        .filter(|i| i.title.contains(MARKER))
        .map(|i| i.id)
        .collect();
    let mut restricted = Vec::new();
    let (mut files, mut images, mut max) = (Vec::new(), Vec::new(), 0);
    for item in &all {
        max = max.max(item.id);
        let item_files = media::files_for_item(&app.db, item.id).await.unwrap();
        let item_images = metadata::images(&app.db, item.id).await.unwrap();
        max = item_files
            .iter()
            .map(|f| f.id)
            .chain(item_images.iter().map(|i| i.id))
            .fold(max, i64::max);
        if marked.contains(&item.id) || item.series_id.is_some_and(|s| marked.contains(&s)) {
            restricted.push(item.id);
            files.extend(item_files.iter().map(|f| f.id));
            images.extend(item_images.iter().map(|i| i.id));
        }
    }
    (restricted, files, images, max)
}

#[tokio::test]
async fn a_restricted_profile_sees_nothing_above_its_limit_anywhere() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let media_dir = tempfile::tempdir().unwrap();
    let lib = library(&app, media_dir.path()).await;
    let (kid_id, kid) = user(
        &app,
        &admin,
        json!({ "name": "Kind", "password": PASSWORD, "max_age_rating": 12, "blocked_genres": ["Horror"] }),
    )
    .await;

    // Sanity: the administrator sees the restricted content.
    let (status, body) = raw(
        &app,
        Method::GET,
        "/api/v1/items?search=Geheim",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for title in [
        "Geheimfilm",
        "Geheim Gruselfilm",
        "Geheimnis ohne Freigabe",
        "Geheimakte",
    ] {
        assert!(body.contains(title), "admin must see {title}: {body}");
    }

    let geheimfilm = item_id(&app, lib.movies, "movie", "Geheimfilm").await;
    let grusel = item_id(&app, lib.movies, "movie", "Geheim Gruselfilm").await;
    let kinderfilm = item_id(&app, lib.movies, "movie", "Kinderfilm").await;
    let teenfilm = item_id(&app, lib.movies, "movie", "Teenfilm").await;
    let akte = item_id(&app, lib.shows, "series", "Geheimakte").await;
    let akte_ep1 = item_id(&app, lib.shows, "episode", "Geheimer Auftrag").await;
    let kinderserie_ep1 = item_id(&app, lib.shows, "episode", "Der Ausflug").await;

    // The child's state on restricted items (as if set before the limit was lowered):
    // resume points, a watched episode (next up), favorites.
    for id in [geheimfilm, grusel] {
        playstate::save_progress(&app.db, kid_id, id, 600_000, Some(6_000_000))
            .await
            .unwrap();
        userdata::set_favorite(&app.db, kid_id, id, true)
            .await
            .unwrap();
    }
    userdata::set_played(&app.db, kid_id, akte_ep1, true)
        .await
        .unwrap();
    userdata::set_played(&app.db, kid_id, kinderserie_ep1, true)
        .await
        .unwrap();
    playstate::save_progress(&app.db, kid_id, kinderfilm, 600_000, Some(6_000_000))
        .await
        .unwrap();

    // A subtitle track on a restricted movie.
    let geheim_file = media::files_for_item(&app.db, geheimfilm).await.unwrap()[0].id;
    let srt = media_dir
        .path()
        .join("Filme/Geheimfilm (2019)/Geheimfilm.2019.de.srt");
    std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:02,000\nGeheim!\n").unwrap();
    media::store_probe(
        &app.db,
        geheim_file,
        &media::ProbeData {
            streams: vec![media::Stream {
                idx: 1000,
                kind: "subtitle".into(),
                codec: Some("srt".into()),
                language: Some("deu".into()),
                is_external: true,
                external_path: Some(srt.to_string_lossy().into_owned()),
                ..Default::default()
            }],
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // Collections and playlists mixing allowed and restricted items.
    let mixed = app
        .post(
            "/api/v1/collections",
            json!({ "name": "Gemischte Sammlung" }),
            Some(&admin),
        )
        .await;
    assert_eq!(mixed.status, StatusCode::CREATED, "{}", mixed.json);
    let mixed = mixed.json["id"].as_i64().unwrap();
    let only_restricted = app
        .post(
            "/api/v1/collections",
            json!({ "name": "Geheime Sammlung" }),
            Some(&admin),
        )
        .await
        .json["id"]
        .as_i64()
        .unwrap();
    for (collection, ids) in [
        (mixed, vec![kinderfilm, geheimfilm, akte, grusel]),
        (only_restricted, vec![geheimfilm, akte]),
    ] {
        let added = app
            .post(
                &format!("/api/v1/collections/{collection}/items"),
                json!({ "item_ids": ids }),
                Some(&admin),
            )
            .await;
        assert_eq!(added.status, StatusCode::OK, "{}", added.json);
    }
    let shared = app
        .post(
            "/api/v1/playlists",
            json!({ "name": "Familienabend", "shared": true }),
            Some(&admin),
        )
        .await;
    assert_eq!(shared.status, StatusCode::CREATED, "{}", shared.json);
    let shared = shared.json["id"].as_i64().unwrap();
    let added = app
        .post(
            &format!("/api/v1/playlists/{shared}/items"),
            json!({ "item_ids": [kinderfilm, geheimfilm, akte_ep1, grusel] }),
            Some(&admin),
        )
        .await;
    assert_eq!(added.status, StatusCode::OK, "{}", added.json);
    let smart = app
        .post(
            "/api/v1/playlists",
            json!({ "name": "Alle Filme", "shared": true, "smart_filter": { "kinds": ["movie"] } }),
            Some(&admin),
        )
        .await;
    assert_eq!(smart.status, StatusCode::CREATED, "{}", smart.json);

    // The child cannot add restricted items to an own playlist.
    let own = app
        .post(
            "/api/v1/playlists",
            json!({ "name": "Meine Liste" }),
            Some(&kid),
        )
        .await
        .json["id"]
        .as_i64()
        .unwrap();
    let sneaky = app
        .post(
            &format!("/api/v1/playlists/{own}/items"),
            json!({ "item_ids": [geheimfilm] }),
            Some(&kid),
        )
        .await;
    assert_eq!(sneaky.status, StatusCode::NOT_FOUND);

    // What the child does see.
    let page = app.get("/api/v1/items?kind=movie", Some(&kid)).await;
    let titles: Vec<&str> = page.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Kinderfilm", "Teenfilm"]);
    assert_eq!(page.json["total"], 2);
    let resume = app.get("/api/v1/items/resume", Some(&kid)).await;
    assert_eq!(resume.json.as_array().unwrap().len(), 1, "{}", resume.json);
    assert_eq!(resume.json[0]["id"], kinderfilm);
    let next_up = app.get("/api/v1/items/next-up", Some(&kid)).await;
    assert_eq!(next_up.json[0]["title"], "Das Fest", "{}", next_up.json);
    assert_eq!(next_up.json.as_array().unwrap().len(), 1);
    let collection = app
        .get(&format!("/api/v1/collections/{mixed}"), Some(&kid))
        .await;
    assert_eq!(collection.json["item_count"], 1, "{}", collection.json);
    let person = metadata::credits(&app.db, teenfilm).await.unwrap()[0].person_id;
    let person = app
        .get(&format!("/api/v1/people/{person}"), Some(&kid))
        .await;
    assert_eq!(
        person.json["credits"].as_array().unwrap().len(),
        2,
        "{}",
        person.json
    );

    // The walk: every GET operation, every id, plus list variants with filters.
    let spec = serde_json::to_value(flixnet_api::openapi()).unwrap();
    let (restricted_items, restricted_files, restricted_images, largest) =
        restricted_ids(&app, &lib).await;
    assert!(restricted_items.len() >= 7, "{restricted_items:?}");
    assert!(largest < MAX_ID, "raise MAX_ID to {largest}");
    let max = MAX_ID;
    let mut uris: Vec<String> = [
        "/api/v1/items?search=Geheim",
        "/api/v1/items?search=Geheimakte&kind=episode",
        "/api/v1/items?kind=episode",
        "/api/v1/items?kind=season",
        "/api/v1/items?kind=series",
        "/api/v1/items?genre=Horror",
        "/api/v1/items?genre=Thriller",
        "/api/v1/items?genre=Krimi",
        "/api/v1/items?favorite=true",
        "/api/v1/items?played=true",
        "/api/v1/items?played=false",
        "/api/v1/items?sort=rating&order=desc",
        "/api/v1/items?letter=G",
        "/api/v1/items/letters?kind=movie",
        "/api/v1/items/resume?limit=100",
        "/api/v1/items/next-up?limit=100",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    uris.push(format!("/api/v1/items?series_id={akte}"));
    uris.push(format!("/api/v1/items?parent_id={akte}"));
    uris.push(format!("/api/v1/items?library_id={}", lib.movies));
    uris.push(format!("/api/v1/items?library_id={}", lib.shows));
    let mut operations = 0;
    for (path, item) in spec["paths"].as_object().unwrap() {
        if item.get("get").is_none() || path.contains("{session}") || path.contains("{code}") {
            continue;
        }
        operations += 1;
        let params = path.matches('{').count();
        if params == 0 {
            uris.push(path.clone());
            continue;
        }
        for id in 1..=max {
            let base = path
                .replace("{id}", &id.to_string())
                .replace("{file_id}", &id.to_string())
                .replace("{item_id}", &id.to_string())
                .replace("{entry_id}", &id.to_string());
            if base.contains("{track}") {
                uris.extend(["1000.vtt", "0.vtt"].map(|t| base.replace("{track}", t)));
            } else if base.contains("{kind}") {
                uris.extend(["poster", "backdrop"].map(|k| base.replace("{kind}", k)));
            } else if base.contains("{chapter}") {
                uris.extend(["0.jpg", "1.jpg"].map(|c| base.replace("{chapter}", c)));
            } else if base.contains("{sheet}") {
                uris.extend(["0.jpg", "1.jpg"].map(|n| base.replace("{sheet}", n)));
            } else {
                assert!(!base.contains('{'), "unhandled path parameter in {path}");
                uris.push(base);
            }
        }
    }
    assert!(operations >= 25, "only {operations} GET operations found");

    let mut ok_responses = 0;
    for uri in &uris {
        let (status, body) = raw(&app, Method::GET, uri, None, Some(&kid)).await;
        assert!(
            !body.contains(MARKER),
            "GET {uri} leaked restricted content to the child ({status}): {body}"
        );
        if status == StatusCode::OK {
            ok_responses += 1;
        }
        // Binary responses carry no titles: check by id.
        let id_of = |prefix: &str| {
            uri.strip_prefix(prefix)
                .and_then(|rest| rest.split(['/', '?']).next())
                .and_then(|id| id.parse::<i64>().ok())
        };
        if let Some(id) = id_of("/api/v1/images/") {
            assert!(
                !(restricted_images.contains(&id) && status.is_success()),
                "GET {uri} served a restricted image"
            );
        }
        for prefix in ["/api/v1/stream/", "/api/v1/subtitles/"] {
            if let Some(id) = id_of(prefix) {
                assert!(
                    !(restricted_files.contains(&id) && status.is_success()),
                    "GET {uri} served a restricted file"
                );
            }
        }
    }
    assert!(
        ok_responses > 50,
        "the walk must reach real content ({ok_responses} OK)"
    );

    // Item-specific writes and playback answer 404 for restricted items.
    for id in &restricted_items {
        let (status, body) = raw(
            &app,
            Method::POST,
            &format!("/api/v1/items/{id}/playback-info"),
            Some(json!({})),
            Some(&kid),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "playback-info {id}: {body}");
        for method in [Method::PUT, Method::DELETE] {
            for action in ["played", "favorite"] {
                let (status, _) = raw(
                    &app,
                    method.clone(),
                    &format!("/api/v1/items/{id}/{action}"),
                    None,
                    Some(&kid),
                )
                .await;
                assert_eq!(status, StatusCode::NOT_FOUND, "{method} {action} {id}");
            }
        }
    }
}

#[tokio::test]
async fn unrated_content_and_blocked_genres_follow_the_profile_settings() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let media_dir = tempfile::tempdir().unwrap();
    library(&app, media_dir.path()).await;
    let (kid_id, kid) = user(
        &app,
        &admin,
        json!({ "name": "Kind", "password": PASSWORD, "max_age_rating": 12 }),
    )
    .await;
    let movies = |json: &Value| -> Vec<String> {
        json["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["title"].as_str().unwrap().to_owned())
            .collect()
    };

    let page = app.get("/api/v1/items?kind=movie", Some(&kid)).await;
    assert_eq!(
        movies(&page.json),
        ["Geheim Gruselfilm", "Kinderfilm", "Teenfilm"]
    );

    // Unrated content allowed, Horror blocked.
    let updated = app
        .request(
            Method::PATCH,
            &format!("/api/v1/users/{kid_id}"),
            Some(json!({ "allow_unrated": true, "blocked_genres": ["Horror", " horror ", ""] })),
            Some(&admin),
        )
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.json);
    assert_eq!(updated.json["allow_unrated"], true);
    assert_eq!(updated.json["blocked_genres"], json!(["Horror"]));
    let page = app.get("/api/v1/items?kind=movie", Some(&kid)).await;
    assert_eq!(
        movies(&page.json),
        ["Geheimnis ohne Freigabe", "Kinderfilm", "Teenfilm"],
        "takes effect for the existing session at once"
    );
    let genres = app.get("/api/v1/genres", Some(&kid)).await;
    let names: Vec<&str> = genres
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["name"].as_str().unwrap())
        .collect();
    assert!(
        !names.contains(&"Horror") && !names.contains(&"Thriller") && !names.contains(&"Krimi"),
        "{names:?}"
    );
    assert!(
        names.contains(&"Drama") && names.contains(&"Familie"),
        "{names:?}"
    );

    // A blocked series genre hides its episodes too.
    app.request(
        Method::PATCH,
        &format!("/api/v1/users/{kid_id}"),
        Some(json!({ "max_age_rating": 16, "blocked_genres": ["Krimi"] })),
        Some(&admin),
    )
    .await;
    let episodes = app.get("/api/v1/items?kind=episode", Some(&kid)).await;
    assert_eq!(movies(&episodes.json), ["Der Ausflug", "Das Fest"]);
    let me = app.get("/api/v1/users/me", Some(&kid)).await;
    assert_eq!(me.json["max_age_rating"], 16);
    assert_eq!(me.json["blocked_genres"], json!(["Krimi"]));

    // Clearing the options.
    let cleared = app
        .request(
            Method::PATCH,
            &format!("/api/v1/users/{kid_id}"),
            Some(json!({ "allow_unrated": false, "blocked_genres": [] })),
            Some(&admin),
        )
        .await;
    assert_eq!(cleared.json["blocked_genres"], json!([]));
    let bad = app
        .request(
            Method::PATCH,
            &format!("/api/v1/users/{kid_id}"),
            Some(json!({ "blocked_genres": ["x".repeat(101)] })),
            Some(&admin),
        )
        .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn old_signed_urls_stop_working_when_the_limit_is_lowered() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let media_dir = tempfile::tempdir().unwrap();
    let lib = library(&app, media_dir.path()).await;
    let (kid_id, kid) = user(
        &app,
        &admin,
        json!({ "name": "Kind", "password": PASSWORD, "max_age_rating": 18 }),
    )
    .await;

    let geheimfilm = item_id(&app, lib.movies, "movie", "Geheimfilm").await;
    let info = app
        .post(
            &format!("/api/v1/items/{geheimfilm}/playback-info"),
            json!({}),
            Some(&kid),
        )
        .await;
    assert_eq!(info.status, StatusCode::OK, "{}", info.json);
    let url = info.json["sources"][0]["direct_url"]
        .as_str()
        .unwrap()
        .to_owned();
    let poster = app
        .get(&format!("/api/v1/items/{geheimfilm}"), Some(&kid))
        .await
        .json["artwork"]["poster_url"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, _) = raw(&app, Method::GET, &url, None, None).await;
    assert_eq!(status, StatusCode::OK, "signed URL works while allowed");

    app.request(
        Method::PATCH,
        &format!("/api/v1/users/{kid_id}"),
        Some(json!({ "max_age_rating": 12 })),
        Some(&admin),
    )
    .await;
    let (status, _) = raw(&app, Method::GET, &url, None, None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the lowered limit applies to issued URLs"
    );
    let (status, _) = raw(&app, Method::GET, &url, None, Some(&kid)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Without the signature, the token alone does not open restricted images.
    let unsigned = poster.split('?').next().unwrap().to_owned();
    let (status, _) = raw(&app, Method::GET, &unsigned, None, Some(&kid)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = raw(&app, Method::GET, &unsigned, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn switching_to_a_less_restricted_profile_needs_its_pin() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let (_, kid) = user(
        &app,
        &admin,
        json!({ "name": "Kind", "password": PASSWORD, "max_age_rating": 12 }),
    )
    .await;
    let create = |body: Value| {
        let app = &app;
        let admin = admin.clone();
        async move {
            let created = app.post("/api/v1/users", body, Some(&admin)).await;
            assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
            created.json["id"].as_i64().unwrap()
        }
    };
    // Unrestricted without PIN or password; with a PIN; with a password only; a
    // sibling with the same limit; a stricter one; one that allows unrated content.
    let oma = create(json!({ "name": "Oma" })).await;
    let mama = create(json!({ "name": "Mama", "pin": "4711" })).await;
    let papa = create(json!({ "name": "Papa", "password": PASSWORD })).await;
    let bruder = create(json!({ "name": "Bruder", "max_age_rating": 12 })).await;
    let baby = create(json!({ "name": "Baby", "max_age_rating": 0 })).await;
    let cousin =
        create(json!({ "name": "Cousin", "max_age_rating": 12, "allow_unrated": true })).await;

    let profiles = app.get("/api/v1/auth/profiles", Some(&kid)).await;
    let unlock = |name: &str| {
        profiles
            .json
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == name)
            .unwrap()["unlock"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(unlock("Felix"), "password");
    assert_eq!(unlock("Oma"), "locked");
    assert_eq!(unlock("Mama"), "pin");
    assert_eq!(unlock("Papa"), "password");
    assert_eq!(unlock("Bruder"), "none");
    assert_eq!(unlock("Baby"), "none");
    assert_eq!(unlock("Cousin"), "locked");

    let device = json!({ "id": "device-Kind", "name": "Testbrowser", "client": "Flixnet Tests" });
    let switch = |user_id: i64, extra: Value| {
        let mut body = json!({ "user_id": user_id, "device": device.clone() });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };
    let locked = app
        .post("/api/v1/auth/switch", switch(oma, json!({})), Some(&kid))
        .await;
    assert_eq!(locked.status, StatusCode::FORBIDDEN);
    assert_eq!(locked.json["code"], "profile_locked");
    let cousin_switch = app
        .post("/api/v1/auth/switch", switch(cousin, json!({})), Some(&kid))
        .await;
    assert_eq!(cousin_switch.status, StatusCode::FORBIDDEN);
    let no_pin = app
        .post("/api/v1/auth/switch", switch(mama, json!({})), Some(&kid))
        .await;
    assert_eq!(no_pin.status, StatusCode::FORBIDDEN);
    let no_password = app
        .post("/api/v1/auth/switch", switch(papa, json!({})), Some(&kid))
        .await;
    assert_eq!(no_password.status, StatusCode::FORBIDDEN);

    // A profile that is not less restricted is open; from there on its rules apply.
    let to_baby = app
        .post("/api/v1/auth/switch", switch(baby, json!({})), Some(&kid))
        .await;
    assert_eq!(to_baby.status, StatusCode::OK, "{}", to_baby.json);
    let baby_token = to_baby.json["token"].as_str().unwrap().to_owned();
    let back = app
        .post(
            "/api/v1/auth/switch",
            switch(bruder, json!({})),
            Some(&baby_token),
        )
        .await;
    assert_eq!(
        back.status,
        StatusCode::FORBIDDEN,
        "12 is less restricted than 0 → locked"
    );
    let to_mama = app
        .post(
            "/api/v1/auth/switch",
            switch(mama, json!({ "pin": "4711" })),
            Some(&baby_token),
        )
        .await;
    assert_eq!(to_mama.status, StatusCode::OK, "{}", to_mama.json);
    let mama_token = to_mama.json["token"].as_str().unwrap().to_owned();
    // An unrestricted profile switches freely to unrestricted profiles without PIN.
    let to_oma = app
        .post(
            "/api/v1/auth/switch",
            switch(oma, json!({})),
            Some(&mama_token),
        )
        .await;
    assert_eq!(to_oma.status, StatusCode::OK, "{}", to_oma.json);
}
