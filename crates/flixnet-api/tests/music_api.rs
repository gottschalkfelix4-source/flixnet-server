//! Music API: album and artist pages, tracks, lyrics, playback sources (Direct Play
//! and AAC conversion), play counts, and what restricted profiles see.
//! Generates its audio with ffmpeg (lavfi sine + tags + an attached cover).

mod common;

use std::path::Path;
use std::process::Command;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::{PASSWORD, TestApp, app, credentials};
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::{items, jobs, libraries, settings};
use flixnet_library::jobs::{ProbePayload, probe_file};
use flixnet_library::scan::scan_library;
use flixnet_media::Ffprobe;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

fn encode(path: &Path, tags: &[(&str, &str)], cover: Option<&Path>, codec: &[&str]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "sine=frequency=330:duration=2"]);
    if let Some(cover) = cover {
        cmd.arg("-i").arg(cover);
        cmd.args([
            "-map",
            "0:a",
            "-map",
            "1:v",
            "-c:v",
            "png",
            "-disposition:v",
            "attached_pic",
        ]);
    }
    for (key, value) in tags {
        cmd.arg("-metadata").arg(format!("{key}={value}"));
    }
    let status = cmd
        .args(codec)
        .arg(path)
        .status()
        .expect("ffmpeg must be installed");
    assert!(status.success(), "ffmpeg failed for {}", path.display());
}

struct Music {
    musik: i64,
    geheim: i64,
}

const FLAC: &[&str] = &["-c:a", "flac"];
const MP3: &[&str] = &["-c:a", "libmp3lame", "-b:a", "128k"];

/// Musik: Anna Alpha – Sommer (Sonne, Regen, Wild [explicit]); Bert Beta – Laut
/// (Krach [Metal], Stille [WMA, untagged]); Sampler "Hits" (Anna Alpha, Dora Delta).
/// Geheimarchiv (second library): Geheimband – Geheimalbum.
async fn library(app: &TestApp, root: &Path) -> Music {
    let cover = root.join("cover.png");
    image::RgbImage::from_pixel(24, 24, image::Rgb([10, 10, 200]))
        .save(&cover)
        .unwrap();
    let musik = root.join("Musik");
    for (n, title) in [(1, "Sonne"), (2, "Regen")] {
        encode(
            &musik.join(format!("Anna Alpha/Sommer/0{n} - {title}.flac")),
            &[
                ("title", title),
                ("artist", "Anna Alpha"),
                ("album_artist", "Anna Alpha"),
                ("album", "Sommer"),
                ("track", &n.to_string()),
                ("date", "2020"),
                ("genre", "Pop"),
                ("REPLAYGAIN_TRACK_GAIN", "-4.00 dB"),
                ("REPLAYGAIN_TRACK_PEAK", "0.900000"),
                ("REPLAYGAIN_ALBUM_GAIN", "-5.00 dB"),
                ("REPLAYGAIN_ALBUM_PEAK", "0.950000"),
            ],
            Some(&cover),
            FLAC,
        );
    }
    encode(
        &musik.join("Anna Alpha/Sommer/03 - Wild.mp3"),
        &[
            ("title", "Wild"),
            ("artist", "Anna Alpha"),
            ("album_artist", "Anna Alpha"),
            ("album", "Sommer"),
            ("track", "3"),
            ("genre", "Pop"),
            ("ITUNESADVISORY", "1"),
        ],
        None,
        MP3,
    );
    std::fs::write(
        musik.join("Anna Alpha/Sommer/01 - Sonne.lrc"),
        "[00:00.20]Die Sonne scheint\n[00:01.10]Den ganzen Tag\n",
    )
    .unwrap();
    encode(
        &musik.join("Bert Beta/Laut/01 - Krach.flac"),
        &[
            ("title", "Krach"),
            ("artist", "Bert Beta"),
            ("album", "Laut"),
            ("track", "1"),
            ("genre", "Metal"),
        ],
        None,
        FLAC,
    );
    // WMA: no tags lofty reads (folder layout), and no browser plays it.
    encode(
        &musik.join("Bert Beta/Laut/02 - Stille.wma"),
        &[],
        None,
        &["-c:a", "wmav2", "-b:a", "64k"],
    );
    for (n, artist) in [(1, "Anna Alpha"), (2, "Dora Delta")] {
        encode(
            &musik.join(format!("Sampler/0{n}.mp3")),
            &[
                ("title", &format!("Hit {n}")),
                ("artist", artist),
                ("album", "Hits"),
                ("track", &n.to_string()),
            ],
            None,
            MP3,
        );
    }
    let archiv = root.join("Archiv");
    encode(
        &archiv.join("Geheimband/Geheimalbum/01 - Geheimnis.flac"),
        &[
            ("title", "Geheimnis"),
            ("artist", "Geheimband"),
            ("album", "Geheimalbum"),
        ],
        None,
        FLAC,
    );

    // Offline: no MusicBrainz lookups.
    settings::set(
        &app.db,
        flixnet_library::providers::SETTINGS_KEY,
        &json!({ "musicbrainz_enabled": false }),
    )
    .await
    .unwrap();
    let queue = JobRuntime::new(app.db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    let mut ids = Vec::new();
    for (name, dir) in [("Musik", &musik), ("Geheimarchiv", &archiv)] {
        let id = libraries::create(
            &app.db,
            name,
            "music",
            &[dir.to_string_lossy().into_owned()],
        )
        .await
        .unwrap()
        .id;
        scan_library(&app.db, &queue, id).await.unwrap();
        ids.push(id);
    }
    let ffprobe = Ffprobe::new("ffprobe");
    while let Some(job) = jobs::claim(&app.db, "probe").await.unwrap() {
        let payload: ProbePayload = serde_json::from_str(&job.payload).unwrap();
        probe_file(&app.db, &ffprobe, payload.file_id)
            .await
            .unwrap();
        jobs::complete(&app.db, job.id).await.unwrap();
    }
    for library in &ids {
        for kind in ["album", "artist"] {
            for item in items::list_by_kind(&app.db, *library, kind).await.unwrap() {
                flixnet_library::music::refresh(&app.db, item.id)
                    .await
                    .unwrap();
            }
        }
    }
    Music {
        musik: ids[0],
        geheim: ids[1],
    }
}

async fn id_of(app: &TestApp, library: i64, kind: &str, title: &str) -> i64 {
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

/// Status, headers and raw body (audio is not JSON).
async fn raw(
    app: &TestApp,
    uri: &str,
    headers: &[(header::HeaderName, &str)],
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(Method::GET).uri(uri);
    for (name, value) in headers {
        builder = builder.header(name, *value);
    }
    let response = app
        .router
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes.to_vec())
}

fn titles(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap().to_owned())
        .collect()
}

/// What Chromium plays (no WMA, no ALAC).
fn browser_profile() -> Value {
    let codecs: Vec<Value> = ["flac", "mp3", "aac", "vorbis", "opus", "pcm_s16le"]
        .iter()
        .map(|c| json!({ "codec": c, "max_channels": 8 }))
        .collect();
    json!({
        "containers": ["flac", "mp3", "m4a", "ogg", "opus", "wav"],
        "audio_codecs": codecs
    })
}

#[tokio::test]
async fn albums_artists_tracks_lyrics_and_play_counts() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let media_dir = tempfile::tempdir().unwrap();
    let lib = library(&app, media_dir.path()).await;

    // Grids through the general items API.
    let albums = app
        .get(
            &format!("/api/v1/items?kind=album&library_id={}", lib.musik),
            Some(&admin),
        )
        .await;
    assert_eq!(albums.status, StatusCode::OK, "{}", albums.json);
    assert_eq!(titles(&albums.json["items"]), ["Hits", "Laut", "Sommer"]);
    let sommer_card = &albums.json["items"][2];
    assert_eq!(sommer_card["series_title"], "Anna Alpha");
    assert_eq!(sommer_card["child_count"], 3);
    assert!(
        sommer_card["poster_url"].is_string(),
        "embedded cover: {sommer_card}"
    );
    let artists = app
        .get(
            &format!("/api/v1/items?kind=artist&library_id={}", lib.musik),
            Some(&admin),
        )
        .await;
    assert_eq!(
        titles(&artists.json["items"]),
        ["Anna Alpha", "Bert Beta", "Verschiedene Interpreten"]
    );

    // Album page: tracks in order, durations, lyrics flag, ReplayGain via playback.
    let sommer = id_of(&app, lib.musik, "album", "Sommer").await;
    let album = app
        .get(&format!("/api/v1/music/albums/{sommer}"), Some(&admin))
        .await;
    assert_eq!(album.status, StatusCode::OK, "{}", album.json);
    assert_eq!(album.json["discs"].as_array().unwrap().len(), 1);
    let tracks = &album.json["discs"][0]["tracks"];
    assert_eq!(titles(tracks), ["Sonne", "Regen", "Wild"]);
    assert_eq!(tracks[0]["artist"], "Anna Alpha");
    assert_eq!(tracks[0]["has_lyrics"], true);
    assert_eq!(tracks[1]["has_lyrics"], false);
    assert_eq!(album.json["track_count"], 3);
    let duration = album.json["duration_ms"].as_i64().unwrap();
    assert!((5800..=6400).contains(&duration), "{duration}");
    assert_eq!(album.json["genres"], json!(["Pop"]));
    assert_eq!(album.json["year"], 2020);
    assert!(album.json["cover_url"].is_string());
    let sonne = tracks[0]["id"].as_i64().unwrap();
    let regen = tracks[1]["id"].as_i64().unwrap();

    // Not an album → 404.
    let anna = id_of(&app, lib.musik, "artist", "Anna Alpha").await;
    let wrong = app
        .get(&format!("/api/v1/music/albums/{anna}"), Some(&admin))
        .await;
    assert_eq!(wrong.status, StatusCode::NOT_FOUND);

    // Lyrics from the .lrc.
    let lyrics = app
        .get(
            &format!("/api/v1/music/tracks/{sonne}/lyrics"),
            Some(&admin),
        )
        .await;
    assert_eq!(lyrics.status, StatusCode::OK, "{}", lyrics.json);
    assert_eq!(lyrics.json["synced"], true);
    assert_eq!(lyrics.json["source"], "sidecar");
    assert_eq!(lyrics.json["lines"][1]["time_ms"], 1100);
    assert_eq!(lyrics.json["lines"][1]["text"], "Den ganzen Tag");
    let none = app
        .get(
            &format!("/api/v1/music/tracks/{regen}/lyrics"),
            Some(&admin),
        )
        .await;
    assert_eq!(none.status, StatusCode::NOT_FOUND);

    // Play counts: a short listen does not count, half the track does.
    let skip = app
        .post(
            &format!("/api/v1/music/tracks/{regen}/scrobble"),
            json!({ "listened_ms": 400 }),
            Some(&admin),
        )
        .await;
    assert_eq!(skip.status, StatusCode::OK, "{}", skip.json);
    assert_eq!(skip.json, json!({ "counted": false, "play_count": 0 }));
    for expected in [1, 2] {
        let played = app
            .post(
                &format!("/api/v1/music/tracks/{regen}/scrobble"),
                json!({ "listened_ms": 1500 }),
                Some(&admin),
            )
            .await;
        assert_eq!(
            played.json,
            json!({ "counted": true, "play_count": expected })
        );
    }
    let bad = app
        .post(
            &format!("/api/v1/music/tracks/{regen}/scrobble"),
            json!({ "listened_ms": -1 }),
            Some(&admin),
        )
        .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    // Scrobbles are music only.
    let not_track = app
        .post(
            &format!("/api/v1/music/tracks/{sommer}/scrobble"),
            json!({ "listened_ms": 5000 }),
            Some(&admin),
        )
        .await;
    assert_eq!(not_track.status, StatusCode::NOT_FOUND);

    // Artist page: own albums, the compilation, popular tracks (most played first).
    let artist = app
        .get(&format!("/api/v1/music/artists/{anna}"), Some(&admin))
        .await;
    assert_eq!(artist.status, StatusCode::OK, "{}", artist.json);
    assert_eq!(titles(&artist.json["albums"]), ["Sommer"]);
    assert_eq!(titles(&artist.json["appears_on"]), ["Hits"]);
    let popular = titles(&artist.json["popular"]);
    assert_eq!(popular[0], "Regen");
    assert!(popular.contains(&"Hit 1".to_owned()), "{popular:?}");
    assert_eq!(artist.json["popular"][0]["play_count"], 2);
    assert!(
        artist.json["poster_url"].is_string(),
        "album cover stands in"
    );

    // Track lists: search, favorites.
    let found = app
        .get("/api/v1/music/tracks?search=Rege", Some(&admin))
        .await;
    assert_eq!(titles(&found.json["items"]), ["Regen"]);
    let fav = app
        .request(
            Method::PUT,
            &format!("/api/v1/items/{sonne}/favorite"),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(fav.status, StatusCode::NO_CONTENT);
    let favorites = app
        .get("/api/v1/music/tracks?favorite=true", Some(&admin))
        .await;
    assert_eq!(titles(&favorites.json["items"]), ["Sonne"]);
    assert_eq!(favorites.json["items"][0]["favorite"], true);
    let of_album = app
        .get(
            &format!("/api/v1/music/tracks?album_id={sommer}"),
            Some(&admin),
        )
        .await;
    assert_eq!(titles(&of_album.json["items"]), ["Sonne", "Regen", "Wild"]);

    // Library cards count music.
    let library = app
        .get(&format!("/api/v1/libraries/{}", lib.musik), Some(&admin))
        .await;
    assert_eq!(
        (
            &library.json["counts"]["artists"],
            &library.json["counts"]["albums"],
            &library.json["counts"]["tracks"]
        ),
        (&json!(3), &json!(3), &json!(7))
    );
    let _ = lib.geheim;
}

#[tokio::test]
async fn playback_direct_plays_or_converts_to_aac() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let media_dir = tempfile::tempdir().unwrap();
    let lib = library(&app, media_dir.path()).await;
    let sonne = id_of(&app, lib.musik, "track", "Sonne").await;
    let stille = id_of(&app, lib.musik, "track", "Stille").await;

    let sources = app
        .post(
            "/api/v1/music/playback",
            json!({ "track_ids": [stille, sonne, 999_999], "profile": browser_profile() }),
            Some(&admin),
        )
        .await;
    assert_eq!(sources.status, StatusCode::OK, "{}", sources.json);
    let list = sources.json.as_array().unwrap();
    assert_eq!(list.len(), 2, "unknown ids are left out: {}", sources.json);
    assert_eq!(list[0]["track_id"], stille);
    assert_eq!(list[0]["method"], "transcode");
    assert_eq!(list[0]["container"], "wma");
    assert_eq!(
        list[0]["reasons"],
        json!(["container_not_supported", "audio_codec_not_supported"])
    );
    assert_eq!(list[1]["method"], "direct_play");
    assert_eq!(list[1]["codec"], "flac");
    assert_eq!(list[1]["replaygain"]["track_gain_db"], -4.0);
    assert_eq!(list[1]["replaygain"]["album_peak"], 0.95);
    let direct = list[1]["url"].as_str().unwrap();
    assert!(direct.starts_with("/api/v1/stream/"), "{direct}");
    let (status, _, _) = raw(&app, direct, &[(header::RANGE, "bytes=0-99")]).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);

    // The WMA is converted once to AAC in MP4 and served with ranges.
    let converted = list[0]["url"].as_str().unwrap();
    assert!(
        converted.starts_with("/api/v1/music/stream/"),
        "{converted}"
    );
    let (status, headers, body) = raw(&app, converted, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "audio/mp4");
    assert_eq!(&body[4..8], b"ftyp", "an MP4 file");
    let (status, headers, body) = raw(&app, converted, &[(header::RANGE, "bytes=0-15")]).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body.len(), 16);
    assert!(
        headers[header::CONTENT_RANGE]
            .to_str()
            .unwrap()
            .starts_with("bytes 0-15/")
    );
    let cached = app.dir.path().join("cache/audio");
    assert_eq!(
        std::fs::read_dir(&cached).unwrap().count(),
        1,
        "one conversion"
    );
    // The converted file plays as AAC: probe it.
    let file = std::fs::read_dir(&cached)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let info = Ffprobe::new("ffprobe").probe(&file).await.unwrap();
    assert_eq!(info.streams[0].codec.as_deref(), Some("aac"));

    // Without a signature: a token works, nothing else.
    let unsigned = converted.split('?').next().unwrap();
    assert_eq!(raw(&app, unsigned, &[]).await.0, StatusCode::UNAUTHORIZED);
    let with_token = format!("{unsigned}?api_key={admin}");
    assert_eq!(raw(&app, &with_token, &[]).await.0, StatusCode::OK);
    // A tampered signature fails.
    let tampered = converted.replace("sig=", "sig=00");
    assert_eq!(raw(&app, &tampered, &[]).await.0, StatusCode::UNAUTHORIZED);

    // The conversion endpoint only serves tracks.
    let missing = app
        .get(
            &format!("/api/v1/music/stream/999999?api_key={admin}"),
            None,
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn restricted_profiles_see_music_but_only_what_they_may() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let media_dir = tempfile::tempdir().unwrap();
    let lib = library(&app, media_dir.path()).await;
    let (kid_id, kid) = user(
        &app,
        &admin,
        json!({ "name": "Kind", "password": PASSWORD, "max_age_rating": 12, "blocked_genres": ["Metal"] }),
    )
    .await;
    // Only the Musik library.
    libraries::set_access(&app.db, kid_id, false, &[lib.musik])
        .await
        .unwrap();

    // Music is unrated, yet visible with an age limit (unrated films are not).
    let albums = app.get("/api/v1/items?kind=album", Some(&kid)).await;
    assert_eq!(albums.status, StatusCode::OK);
    assert_eq!(
        titles(&albums.json["items"]),
        ["Hits", "Sommer"],
        "Laut is Metal only; Geheimalbum is in a library the profile may not see"
    );
    let artists = app.get("/api/v1/items?kind=artist", Some(&kid)).await;
    assert_eq!(
        titles(&artists.json["items"]),
        ["Anna Alpha", "Bert Beta", "Verschiedene Interpreten"]
    );

    // The explicit track (age 16) is hidden; the rest of the album is there.
    let sommer = id_of(&app, lib.musik, "album", "Sommer").await;
    let album = app
        .get(&format!("/api/v1/music/albums/{sommer}"), Some(&kid))
        .await;
    assert_eq!(
        titles(&album.json["discs"][0]["tracks"]),
        ["Sonne", "Regen"]
    );
    let wild = id_of(&app, lib.musik, "track", "Wild").await;
    let krach = id_of(&app, lib.musik, "track", "Krach").await;
    let geheim_track = id_of(&app, lib.geheim, "track", "Geheimnis").await;
    let geheim_album = id_of(&app, lib.geheim, "album", "Geheimalbum").await;
    for id in [wild, krach, geheim_track] {
        let lyrics = app
            .get(&format!("/api/v1/music/tracks/{id}/lyrics"), Some(&kid))
            .await;
        assert_eq!(lyrics.status, StatusCode::NOT_FOUND, "lyrics {id}");
        let scrobble = app
            .post(
                &format!("/api/v1/music/tracks/{id}/scrobble"),
                json!({ "listened_ms": 9000 }),
                Some(&kid),
            )
            .await;
        assert_eq!(scrobble.status, StatusCode::NOT_FOUND, "scrobble {id}");
    }
    let hidden = app
        .get(&format!("/api/v1/music/albums/{geheim_album}"), Some(&kid))
        .await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    let tracks = app.get("/api/v1/music/tracks?limit=500", Some(&kid)).await;
    let visible = titles(&tracks.json["items"]);
    for title in ["Wild", "Krach", "Geheimnis"] {
        assert!(!visible.contains(&title.to_owned()), "{visible:?}");
    }
    assert_eq!(tracks.json["total"], 5, "{visible:?}");

    // Playback sources leave hidden tracks out; their streams answer 404 even signed.
    let sources = app
        .post(
            "/api/v1/music/playback",
            json!({ "track_ids": [wild, geheim_track, krach], "profile": browser_profile() }),
            Some(&kid),
        )
        .await;
    assert_eq!(sources.json, json!([]));
    let admin_sources = app
        .post(
            "/api/v1/music/playback",
            json!({ "track_ids": [geheim_track], "profile": {} }),
            Some(&admin),
        )
        .await;
    let geheim_file = admin_sources.json[0]["file_id"].as_i64().unwrap();
    let signer = flixnet_api::signing::UrlSigner::new(b"test-secret".to_vec());
    for url in [
        signer.audio_url(geheim_file, kid_id),
        signer.stream_url(geheim_file, kid_id),
    ] {
        assert_eq!(raw(&app, &url, &[]).await.0, StatusCode::NOT_FOUND, "{url}");
    }

    // Artist pages follow the same rules.
    let bert = id_of(&app, lib.musik, "artist", "Bert Beta").await;
    let artist = app
        .get(&format!("/api/v1/music/artists/{bert}"), Some(&kid))
        .await;
    assert_eq!(artist.status, StatusCode::OK);
    assert_eq!(titles(&artist.json["albums"]), Vec::<String>::new());
    assert_eq!(titles(&artist.json["popular"]), ["Stille"]);

    // Unrestricted again: everything is back.
    let updated = app
        .request(
            Method::PATCH,
            &format!("/api/v1/users/{kid_id}"),
            Some(json!({ "max_age_rating": null, "blocked_genres": [] })),
            Some(&admin),
        )
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.json);
    let album = app
        .get(&format!("/api/v1/music/albums/{sommer}"), Some(&kid))
        .await;
    assert_eq!(
        titles(&album.json["discs"][0]["tracks"]),
        ["Sonne", "Regen", "Wild"]
    );
}
