//! fanart.tv artwork against a mocked API.

use flixnet_db::metadata::{self, set_provider_id};
use flixnet_db::{Db, libraries};
use flixnet_library::providers::{self, Fanart};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn fanart_logos_rank_by_language_and_quality() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/movies/603"))
        .and(query_param("api_key", "fan-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "hdmovielogo": [
                { "url": "https://f/logo-en.png", "lang": "en", "likes": "9" },
                { "url": "https://f/logo-de.png", "lang": "de", "likes": "1" }
            ],
            "movielogo": [{ "url": "https://f/logo-sd-de.png", "lang": "de", "likes": "30" }],
            "moviebackground": [{ "url": "https://f/bg.jpg", "lang": "", "likes": "4" }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/movies/999"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    let library = libraries::create(&db, "Filme", "movies", &[])
        .await
        .unwrap();
    let mut conn = db.writer().acquire().await.unwrap();
    let item = flixnet_db::items::upsert(
        &mut conn,
        &flixnet_db::items::NewItem {
            library_id: library.id,
            kind: "movie",
            parent_id: None,
            series_id: None,
            title: "Matrix",
            sort_title: "matrix",
            year: Some(1999),
            index_number: None,
            index_end: None,
            parent_index_number: None,
            premiere_date: None,
            content_key: "local:movie:matrix:1999",
        },
    )
    .await
    .unwrap();
    drop(conn);

    let fanart = Fanart::new("fan-key", "de-DE", db.clone()).with_base_url(server.uri());
    assert_eq!(
        providers::apply_fanart(&db, &fanart, item).await.unwrap(),
        0,
        "not matched yet: nothing to look up"
    );

    set_provider_id(&db, item, "tmdb", "603").await.unwrap();
    let added = providers::apply_fanart(&db, &fanart, item).await.unwrap();
    assert_eq!(added, 4);
    let logos: Vec<_> = metadata::images(&db, item)
        .await
        .unwrap()
        .into_iter()
        .filter(|i| i.kind == "logo")
        .map(|i| i.url.unwrap())
        .collect();
    assert_eq!(
        logos,
        [
            "https://f/logo-de.png",
            "https://f/logo-sd-de.png",
            "https://f/logo-en.png"
        ],
        "German HD first, then German SD, then English"
    );

    set_provider_id(&db, item, "tmdb", "999").await.unwrap();
    assert_eq!(
        providers::apply_fanart(&db, &fanart, item).await.unwrap(),
        0,
        "404 is not an error"
    );
}
