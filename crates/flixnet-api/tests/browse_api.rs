mod common;

use axum::http::{Method, StatusCode};
use common::{PASSWORD, TestApp, app, credentials};
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::{items, libraries};
use flixnet_library::refresh::refresh_local;
use flixnet_library::scan::scan_library;
use serde_json::json;

fn write(root: &std::path::Path, relative: &str, content: &[u8]) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// A small library: three movies with NFO ratings and a poster, one series.
async fn library(app: &TestApp, root: &std::path::Path) -> (i64, i64) {
    let movies = root.join("Filme");
    let nfo = |title: &str, mpaa: &str, genre: &str, plot: &str| {
        format!(
            "<movie><title>{title}</title><mpaa>{mpaa}</mpaa><genre>{genre}</genre><plot>{plot}</plot></movie>"
        )
    };
    write(&movies, "Alpha (2020)/Alpha.2020.mkv", b"v");
    write(
        &movies,
        "Alpha (2020)/movie.nfo",
        nfo("Alpha", "FSK 6", "Abenteuer", "Eine Reise ins All.").as_bytes(),
    );
    image::RgbImage::new(10, 15)
        .save(movies.join("Alpha (2020)/poster.png"))
        .unwrap();
    write(&movies, "Ärger (2021)/Aerger.2021.mkv", b"v");
    write(
        &movies,
        "Ärger (2021)/movie.nfo",
        nfo("Ärger", "FSK 12", "Komödie", "Chaos im Büro.").as_bytes(),
    );
    write(&movies, "Zombie (2019)/Zombie.2019.mkv", b"v");
    write(
        &movies,
        "Zombie (2019)/movie.nfo",
        nfo("Zombie", "FSK 18", "Horror", "Untote überall.").as_bytes(),
    );
    let shows = root.join("Serien");
    write(&shows, "Dark (2017)/Staffel 1/Dark.S01E01.mkv", b"v");
    write(&shows, "Dark (2017)/Staffel 1/Dark.S01E02.mkv", b"v");
    write(
        &shows,
        "Dark (2017)/tvshow.nfo",
        b"<tvshow><title>Dark</title><mpaa>FSK 16</mpaa></tvshow>",
    );

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
    scan_library(&app.db, &queue, movies_id).await.unwrap();
    scan_library(&app.db, &queue, shows_id).await.unwrap();
    for library in [movies_id, shows_id] {
        for kind in ["movie", "series", "season", "episode"] {
            for item in items::list_by_kind(&app.db, library, kind).await.unwrap() {
                refresh_local(&app.db, item.id).await.unwrap();
            }
        }
    }
    (movies_id, shows_id)
}

fn titles(json: &serde_json::Value) -> Vec<String> {
    json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn browse_search_letters_and_details() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let media = tempfile::tempdir().unwrap();
    let (movies, shows) = library(&app, media.path()).await;

    let page = app
        .get(
            &format!("/api/v1/items?library_id={movies}&kind=movie"),
            Some(&admin),
        )
        .await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.json);
    assert_eq!(titles(&page.json), ["Alpha", "Ärger", "Zombie"]);
    assert_eq!(page.json["total"], 3);
    let alpha = &page.json["items"][0];
    assert!(
        alpha["poster_url"]
            .as_str()
            .unwrap()
            .starts_with("/api/v1/images/")
    );
    assert!(alpha["poster_url"].as_str().unwrap().contains("sig="));

    let newest = app
        .get(
            "/api/v1/items?kind=movie&sort=date_added&order=desc&limit=1",
            Some(&admin),
        )
        .await;
    assert_eq!(newest.json["items"].as_array().unwrap().len(), 1);
    assert_eq!(newest.json["total"], 3);

    let search = app.get("/api/v1/items?search=reise", Some(&admin)).await;
    assert_eq!(titles(&search.json), ["Alpha"], "matches the description");
    let accent = app.get("/api/v1/items?search=arger", Some(&admin)).await;
    assert_eq!(titles(&accent.json), ["Ärger"]);
    let genre = app.get("/api/v1/items?genre=Horror", Some(&admin)).await;
    assert_eq!(titles(&genre.json), ["Zombie"]);

    let letters = app
        .get(
            &format!("/api/v1/items/letters?library_id={movies}"),
            Some(&admin),
        )
        .await;
    assert_eq!(
        letters.json,
        json!([{ "letter": "A", "count": 2, "offset": 0 }, { "letter": "Z", "count": 1, "offset": 2 }])
    );
    let by_letter = app
        .get(
            &format!("/api/v1/items?library_id={movies}&letter=Z"),
            Some(&admin),
        )
        .await;
    assert_eq!(titles(&by_letter.json), ["Zombie"]);

    let genres = app.get("/api/v1/genres", Some(&admin)).await;
    assert_eq!(genres.json.as_array().unwrap().len(), 3);

    // Series → seasons → episodes.
    let series = app
        .get(
            &format!("/api/v1/items?library_id={shows}&kind=series"),
            Some(&admin),
        )
        .await;
    let series_id = series.json["items"][0]["id"].as_i64().unwrap();
    assert_eq!(series.json["items"][0]["child_count"], 1);
    let seasons = app
        .get(
            &format!("/api/v1/items?parent_id={series_id}"),
            Some(&admin),
        )
        .await;
    let season_id = seasons.json["items"][0]["id"].as_i64().unwrap();
    let episodes = app
        .get(
            &format!("/api/v1/items?parent_id={season_id}"),
            Some(&admin),
        )
        .await;
    let numbers: Vec<_> = episodes.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["index_number"].as_i64().unwrap())
        .collect();
    assert_eq!(numbers, [1, 2]);
    let reversed = app
        .get(
            &format!("/api/v1/items?parent_id={season_id}&sort=index&order=desc"),
            Some(&admin),
        )
        .await;
    assert_eq!(reversed.json["items"][0]["index_number"], 2);

    // Details.
    let detail = app
        .get(&format!("/api/v1/items/{}", alpha["id"]), Some(&admin))
        .await;
    assert_eq!(detail.status, StatusCode::OK);
    assert_eq!(detail.json["title"], "Alpha");
    assert_eq!(detail.json["overview"], "Eine Reise ins All.");
    assert_eq!(detail.json["genres"], json!(["Abenteuer"]));
    assert_eq!(detail.json["age_rating"], 6);
    assert!(detail.json["artwork"]["poster_url"].is_string());
    assert_eq!(detail.json["files"][0]["file_name"], "Alpha.2020.mkv");
    assert_eq!(detail.json["match_state"], "unmatched");
    let episode_id = episodes.json["items"][0]["id"].as_i64().unwrap();
    let episode = app
        .get(&format!("/api/v1/items/{episode_id}"), Some(&admin))
        .await;
    assert_eq!(episode.json["series_title"], "Dark");
    assert_eq!(episode.json["season_title"], "Staffel 1");

    let bad = app.get("/api/v1/items?kind=podcast", Some(&admin)).await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn age_limits_and_library_grants_apply_everywhere() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let media = tempfile::tempdir().unwrap();
    let (movies, shows) = library(&app, media.path()).await;

    let kid_id = app
        .post(
            "/api/v1/users",
            json!({ "name": "Kind", "password": PASSWORD, "max_age_rating": 12 }),
            Some(&admin),
        )
        .await
        .json["id"]
        .as_i64()
        .unwrap();
    let kid = app
        .post(
            "/api/v1/auth/login",
            credentials("Kind", PASSWORD, "tablet"),
            None,
        )
        .await
        .json["token"]
        .as_str()
        .unwrap()
        .to_owned();

    let visible = app.get("/api/v1/items?kind=movie", Some(&kid)).await;
    assert_eq!(titles(&visible.json), ["Alpha", "Ärger"], "FSK 18 hidden");
    let series = app
        .get("/api/v1/items?kind=series,episode", Some(&kid))
        .await;
    assert_eq!(
        series.json["total"], 0,
        "FSK 16 series and its episodes hidden"
    );
    let search = app.get("/api/v1/items?search=untote", Some(&kid)).await;
    assert_eq!(search.json["total"], 0, "search respects the age limit");

    let zombie = items::list_by_kind(&app.db, movies, "movie")
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.title == "Zombie")
        .unwrap();
    assert_eq!(
        app.get(&format!("/api/v1/items/{}", zombie.id), Some(&kid))
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // Library grants.
    app.request(
        Method::PUT,
        &format!("/api/v1/users/{kid_id}/libraries"),
        Some(json!({ "all_libraries": false, "library_ids": [shows] })),
        Some(&admin),
    )
    .await;
    assert_eq!(
        app.get("/api/v1/items?kind=movie", Some(&kid)).await.json["total"],
        0
    );
    let explicit = app
        .get(&format!("/api/v1/items?library_id={movies}"), Some(&kid))
        .await;
    assert_eq!(
        explicit.json["total"], 0,
        "asking for a hidden library yields nothing"
    );
}
