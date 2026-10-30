//! MusicBrainz, Cover Art Archive and fanart.tv (music) against mocked APIs:
//! matching albums and artists, ids for tracks, covers, caching, the User-Agent and
//! the one-request-per-second limit.

use std::time::{Duration, Instant};

use flixnet_db::music::AudioTags;
use flixnet_db::{Db, items, libraries, media, metadata, music, userdata};
use flixnet_library::music::{refresh_album, refresh_artist};
use flixnet_library::providers::Fanart;
use flixnet_library::providers::musicbrainz::{MusicBrainz, USER_AGENT};
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param, query_param_contains};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Album {
    db: Db,
    _dir: tempfile::TempDir,
    artist: i64,
    album: i64,
    tracks: Vec<i64>,
}

/// "Planet Punk" by "Die Ärzte" with two untagged tracks (no MusicBrainz ids).
async fn album(title: &str) -> Album {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    let library = libraries::create(&db, "Musik", "music", &[])
        .await
        .unwrap()
        .id;
    let mut tx = db.writer().begin().await.unwrap();
    fn item<'a>(
        library: i64,
        kind: &'a str,
        key: &'a str,
        title: &'a str,
        parent: Option<i64>,
        index: Option<i64>,
    ) -> items::NewItem<'a> {
        items::NewItem {
            library_id: library,
            kind,
            parent_id: parent,
            series_id: None,
            title,
            sort_title: title,
            year: None,
            index_number: index,
            index_end: None,
            parent_index_number: index.map(|_| 1),
            premiere_date: None,
            content_key: key,
        }
    }

    let artist = items::upsert(
        &mut tx,
        &item(
            library,
            "artist",
            "local:artist:die arzte",
            "Die Ärzte",
            None,
            None,
        ),
    )
    .await
    .unwrap();
    let album_key = format!("local:album:die arzte:{title}");
    let album = items::upsert(
        &mut tx,
        &items::NewItem {
            series_id: Some(artist),
            ..item(library, "album", &album_key, title, Some(artist), None)
        },
    )
    .await
    .unwrap();
    let mut tracks = Vec::new();
    for n in 1..=2_i64 {
        let key = format!("local:album:x:d1:t{n}");
        let name = format!("Lied {n}");
        let track = items::upsert(
            &mut tx,
            &items::NewItem {
                series_id: Some(artist),
                ..item(library, "track", &key, &name, Some(album), Some(n))
            },
        )
        .await
        .unwrap();
        let path = dir.path().join(format!("Die Ärzte/{title}/0{n}.flac"));
        let file = media::insert_file(
            &mut tx,
            &media::NewFile {
                library_id: library,
                item_id: Some(track),
                path: &path.to_string_lossy(),
                relative_path: &format!("Die Ärzte/{title}/0{n}.flac"),
                size: 1,
                mtime: 1,
                role: "main",
                extra_kind: None,
                part: None,
                parsed_json: None,
            },
        )
        .await
        .unwrap();
        music::store_tags(
            &mut tx,
            file,
            &AudioTags {
                title: Some(format!("Lied {n}")),
                duration_ms: Some(180_000),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        tracks.push(track);
    }
    tx.commit().await.unwrap();
    Album {
        db,
        _dir: dir,
        artist,
        album,
        tracks,
    }
}

fn credit(name: &str, id: &str) -> serde_json::Value {
    json!([{ "name": name, "joinphrase": "", "artist": { "id": id, "name": name, "sort-name": name } }])
}

#[tokio::test]
async fn albums_are_matched_by_search_and_get_ids_genres_and_covers() {
    let server = MockServer::start().await;
    let a = album("Planet Punk").await;

    Mock::given(method("GET"))
        .and(path("/ws/2/release"))
        .and(header("user-agent", USER_AGENT))
        .and(query_param("fmt", "json"))
        .and(query_param_contains("query", "release:\"Planet Punk\""))
        .and(query_param_contains("query", "artist:\"Die Ärzte\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "releases": [
                { "id": "rel-tribute", "title": "Planet Punk", "score": 95, "track-count": 2,
                  "artist-credit": credit("Punk Tribute Band", "art-x") },
                { "id": "rel-1", "title": "Planet Punk", "score": 100, "track-count": 2,
                  "artist-credit": credit("Die Ärzte", "art-1") }
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/ws/2/release/rel-1"))
        .and(header("user-agent", USER_AGENT))
        .and(query_param(
            "inc",
            "artist-credits+release-groups+genres+recordings",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "rel-1", "title": "Planet Punk", "date": "1995-03-01", "country": "DE",
            "artist-credit": credit("Die Ärzte", "art-1"),
            "release-group": { "id": "rg-1", "title": "Planet Punk", "primary-type": "Album",
                               "first-release-date": "1995-03-01",
                               "genres": [{ "name": "punk", "count": 1 }] },
            "genres": [{ "name": "punk rock", "count": 4 }, { "name": "rock", "count": 2 }],
            "media": [{ "position": 1, "format": "CD", "track-count": 2, "tracks": [
                { "id": "trk-1", "position": 1, "number": "1", "title": "Lied 1", "length": 180000,
                  "recording": { "id": "rec-1", "title": "Lied 1" } },
                { "id": "trk-2", "position": 2, "number": "2", "title": "Lied 2", "length": 180000,
                  "recording": { "id": "rec-2", "title": "Lied 2" } }
            ]}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/caa/release/rel-1"))
        .and(header("user-agent", USER_AGENT))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "images": [
                { "front": false, "image": "http://caa/back.jpg", "thumbnails": {} },
                { "front": true, "image": "http://caa/front.jpg",
                  "thumbnails": { "250": "http://caa/front-250.jpg", "500": "http://caa/front-500.jpg" } }
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mb = MusicBrainz::new(a.db.clone()).with_base_urls(
        &format!("{}/ws/2", server.uri()),
        &format!("{}/caa", server.uri()),
    );
    let before = userdata::identity_key(&a.db, a.tracks[0]).await.unwrap();
    assert_eq!(before.as_deref(), Some("local:album:x:d1:t1"));

    let album_item = items::get(&a.db, a.album).await.unwrap().unwrap();
    refresh_album(&a.db, &album_item, Some(&mb)).await.unwrap();

    let ids = items::provider_ids(&a.db, a.album).await.unwrap();
    assert_eq!(
        ids,
        [
            ("musicbrainz_album".to_owned(), "rel-1".to_owned()),
            ("musicbrainz_albumartist".to_owned(), "art-1".to_owned()),
            ("musicbrainz_releasegroup".to_owned(), "rg-1".to_owned()),
        ]
    );
    let details = items::details(&a.db, a.album).await.unwrap().unwrap();
    assert_eq!(details.premiere_date.as_deref(), Some("1995-03-01"));
    assert_eq!(
        details.year,
        Some(1995),
        "no year in the tags: from MusicBrainz"
    );
    assert_eq!(details.match_state, "matched");
    assert_eq!(details.runtime_ms, Some(360_000));
    assert_eq!(
        metadata::genres(&a.db, a.album).await.unwrap(),
        ["Punk Rock", "Rock"]
    );
    // Tracks got release track ids; their identity follows (user data with it).
    assert_eq!(
        items::provider_ids(&a.db, a.tracks[1]).await.unwrap(),
        [
            ("musicbrainz_recording".to_owned(), "rec-2".to_owned()),
            ("musicbrainz_track".to_owned(), "trk-2".to_owned()),
        ]
    );
    assert_eq!(
        userdata::identity_key(&a.db, a.tracks[0])
            .await
            .unwrap()
            .as_deref(),
        Some("mb:track:trk-1")
    );
    // The front cover (500 px thumbnail, https).
    let images = metadata::images(&a.db, a.album).await.unwrap();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].source, "coverartarchive");
    assert_eq!(images[0].url.as_deref(), Some("https://caa/front-500.jpg"));
    // The artist borrows the cover.
    let artist_images = metadata::images(&a.db, a.artist).await.unwrap();
    assert_eq!(artist_images[0].source, "album");

    // A second refresh answers from the cache: every mock was called exactly once.
    let album_item = items::get(&a.db, a.album).await.unwrap().unwrap();
    refresh_album(&a.db, &album_item, Some(&mb)).await.unwrap();
}

#[tokio::test]
async fn unconvincing_search_hits_are_not_applied() {
    let server = MockServer::start().await;
    let a = album("Geheimtipp").await;
    Mock::given(method("GET"))
        .and(path("/ws/2/release"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "releases": [
                { "id": "rel-live", "title": "Geheimtipp Live", "score": 70, "track-count": 19,
                  "artist-credit": credit("Die Ärzte", "art-1") }
            ]
        })))
        .mount(&server)
        .await;
    let mb = MusicBrainz::new(a.db.clone()).with_base_urls(
        &format!("{}/ws/2", server.uri()),
        &format!("{}/caa", server.uri()),
    );
    let album_item = items::get(&a.db, a.album).await.unwrap().unwrap();
    refresh_album(&a.db, &album_item, Some(&mb)).await.unwrap();
    assert!(
        items::provider_ids(&a.db, a.album)
            .await
            .unwrap()
            .is_empty()
    );
    let details = items::details(&a.db, a.album).await.unwrap().unwrap();
    assert_eq!(details.match_state, "failed");
}

#[tokio::test]
async fn artists_get_an_id_by_exact_name_and_fanart_images() {
    let server = MockServer::start().await;
    let a = album("Planet Punk").await;
    Mock::given(method("GET"))
        .and(path("/ws/2/artist"))
        .and(query_param_contains("query", "artist:\"Die Ärzte\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "artists": [
                { "id": "art-1", "name": "Die Ärzte", "score": 100, "type": "Group", "country": "DE" },
                { "id": "art-9", "name": "Die Ärzte Coverband", "score": 80 }
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/fanart/music/art-1"))
        .and(query_param("api_key", "fan-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "artistthumb": [{ "url": "https://f/thumb.jpg", "likes": "3" }],
            "artistbackground": [{ "url": "https://f/bg.jpg", "likes": "1" }],
            "hdmusiclogo": [{ "url": "https://f/logo.png", "lang": "en", "likes": "2" }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mb = MusicBrainz::new(a.db.clone()).with_base_urls(
        &format!("{}/ws/2", server.uri()),
        &format!("{}/caa", server.uri()),
    );
    let fanart = Fanart::new("fan-key", "de-DE", a.db.clone())
        .with_base_url(format!("{}/fanart", server.uri()));
    let artist = items::get(&a.db, a.artist).await.unwrap().unwrap();
    refresh_artist(&a.db, &artist, Some(&mb), Some(&fanart))
        .await
        .unwrap();
    assert_eq!(
        items::provider_ids(&a.db, a.artist).await.unwrap(),
        [("musicbrainz_artist".to_owned(), "art-1".to_owned())]
    );
    let kinds: Vec<(String, String)> = metadata::images(&a.db, a.artist)
        .await
        .unwrap()
        .into_iter()
        .map(|i| (i.kind, i.url.unwrap_or_default()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("backdrop".to_owned(), "https://f/bg.jpg".to_owned()),
            ("logo".to_owned(), "https://f/logo.png".to_owned()),
            ("poster".to_owned(), "https://f/thumb.jpg".to_owned()),
        ]
    );
    userdata::update_identity(&a.db, a.artist).await.unwrap();
    assert_eq!(
        userdata::identity_key(&a.db, a.artist)
            .await
            .unwrap()
            .as_deref(),
        Some("mb:artist:art-1")
    );
}

#[tokio::test]
async fn requests_are_spaced_one_second_apart_and_503_is_retried() {
    let server = MockServer::start().await;
    let a = album("Egal").await;
    for id in ["a", "b", "c"] {
        Mock::given(method("GET"))
            .and(path(format!("/ws/2/artist/{id}")))
            .and(header("user-agent", USER_AGENT))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": id, "name": id })))
            .expect(1)
            .mount(&server)
            .await;
    }
    // MusicBrainz' "slow down": answered once with 503, then fine.
    Mock::given(method("GET"))
        .and(path("/ws/2/recording/r"))
        .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "1"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/ws/2/recording/r"))
        .and(query_param("inc", "artist-credits+genres"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "r", "title": "Aufnahme", "length": 1000,
            "artist-credit": credit("Anna", "art-a")
        })))
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/ws/2/artist/missing"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;

    let mb = MusicBrainz::new(a.db.clone()).with_base_urls(
        &format!("{}/ws/2", server.uri()),
        &format!("{}/caa", server.uri()),
    );
    let started = Instant::now();
    for id in ["a", "b", "c"] {
        assert_eq!(mb.artist(id).await.unwrap().unwrap().name, id);
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(1950),
        "three requests need two gaps of a second: {elapsed:?}"
    );

    // Cached: no request, no wait.
    let started = Instant::now();
    assert_eq!(mb.artist("b").await.unwrap().unwrap().name, "b");
    assert!(started.elapsed() < Duration::from_millis(500));

    // 503 → wait → retry succeeds.
    let recording = mb.recording("r").await.unwrap().unwrap();
    assert_eq!(recording.title, "Aufnahme");
    // 404 is a cached miss.
    assert!(mb.artist("missing").await.unwrap().is_none());
    assert!(mb.artist("missing").await.unwrap().is_none());
}
