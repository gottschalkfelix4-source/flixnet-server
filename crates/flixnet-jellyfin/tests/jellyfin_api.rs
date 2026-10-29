//! The Jellyfin-compatible API end to end (tower `oneshot` against a temp database):
//! sign-in → views → items → playback info → direct stream → HLS → playstate →
//! resume/next up, plus JSON shape checks of the main DTOs.

use std::path::Path as FsPath;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use flixnet_api::{AppState, Services};
use flixnet_core::Config;
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_core::scheduler::TaskRegistry;
use flixnet_db::{Db, libraries, media, metadata, playstate, users};
use flixnet_library::images::ImageCache;
use flixnet_library::scan::scan_library;
use flixnet_media::transcode::{TranscodeManager, TranscodeSettings};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

const PASSWORD: &str = "sicheres-passwort";
const MIN: i64 = 60_000;
const TICKS: i64 = 10_000;

struct TestApp {
    _dir: TempDir,
    router: Router,
    db: Db,
}

async fn app() -> TestApp {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    let events = EventBus::new();
    let runtime = JobRuntime::new(db.clone(), events.clone());
    let services = Services {
        events,
        jobs: runtime.handle(),
        tasks: TaskRegistry::new(flixnet_core::maintenance::tasks()),
        images: ImageCache::new(dir.path().join("cache/images")),
        url_secret: b"test-secret".to_vec(),
        watcher: None,
        transcoder: TranscodeManager::new(
            "ffmpeg",
            dir.path().join("transcode"),
            TranscodeSettings::default(),
        ),
        ffprobe: flixnet_media::Ffprobe::new("ffprobe"),
        subtitles: flixnet_media::subtitles::SubtitleCache::new(
            "ffmpeg",
            dir.path().join("cache/subtitles"),
            std::time::Duration::from_secs(60),
            2,
        ),
        livetv: flixnet_livetv::LiveTvManager::new(
            "ffmpeg",
            "ffprobe",
            dir.path().join("transcode/live"),
            flixnet_livetv::LiveSettings::default(),
        ),
    };
    let state = AppState::new(Config::default(), db.clone(), services);
    TestApp {
        router: flixnet_jellyfin::router(state),
        db,
        _dir: dir,
    }
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    bytes: Vec<u8>,
    json: Value,
}

impl Reply {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

/// `Authorization: MediaBrowser …` as Jellyfin clients send it.
fn auth_header(token: Option<&str>) -> String {
    let mut value =
        r#"MediaBrowser Client="Infuse", Device="iPad", DeviceId="device-1", Version="8.0""#
            .to_owned();
    if let Some(token) = token {
        value.push_str(&format!(r#", Token="{token}""#));
    }
    value
}

impl TestApp {
    async fn send(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        headers: &[(&str, String)],
    ) -> Reply {
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, value);
        }
        let request = match body {
            Some(body) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string())),
            None => builder.body(Body::empty()),
        }
        .unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Reply {
            status,
            headers,
            bytes,
            json,
        }
    }

    async fn get(&self, uri: &str, token: Option<&str>) -> Reply {
        self.send(
            Method::GET,
            uri,
            None,
            &[("authorization", auth_header(token))],
        )
        .await
    }

    async fn post(&self, uri: &str, body: Value, token: Option<&str>) -> Reply {
        self.send(
            Method::POST,
            uri,
            Some(body),
            &[("authorization", auth_header(token))],
        )
        .await
    }

    async fn sign_in(&self, name: &str, password: &str) -> Reply {
        self.post(
            "/Users/AuthenticateByName",
            json!({ "Username": name, "Pw": password }),
            None,
        )
        .await
    }

    async fn token(&self, name: &str) -> String {
        let reply = self.sign_in(name, PASSWORD).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.json);
        reply.json["AccessToken"].as_str().unwrap().to_owned()
    }
}

/// Every member name is PascalCase (maps whose keys are data are skipped).
fn assert_pascal_case(value: &Value, path: &str) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                assert!(
                    key.starts_with(|c: char| c.is_ascii_uppercase()),
                    "{path}.{key} is not PascalCase"
                );
                if !matches!(
                    key.as_str(),
                    "ImageBlurHashes" | "ProviderIds" | "ImageTags" | "CustomPrefs"
                ) {
                    assert_pascal_case(child, &format!("{path}.{key}"));
                }
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                assert_pascal_case(item, &format!("{path}[{i}]"));
            }
        }
        _ => {}
    }
}

fn assert_keys(value: &Value, keys: &[&str]) {
    for key in keys {
        assert!(value.get(key).is_some(), "missing {key} in {value}");
    }
}

fn assert_guid(value: &Value) {
    let s = value
        .as_str()
        .unwrap_or_else(|| panic!("not a string: {value}"));
    assert!(
        s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()),
        "not a GUID: {s}"
    );
}

fn stream(idx: i64, kind: &str, codec: &str) -> media::Stream {
    media::Stream {
        idx,
        kind: kind.into(),
        codec: Some(codec.into()),
        profile: None,
        level: None,
        width: None,
        height: None,
        fps: None,
        bit_depth: None,
        pix_fmt: None,
        color_transfer: None,
        color_primaries: None,
        hdr: None,
        channels: None,
        channel_layout: None,
        sample_rate: None,
        bitrate: None,
        language: None,
        title: None,
        is_default: false,
        is_forced: false,
        is_sdh: false,
        is_external: false,
        external_path: None,
    }
}

async fn scan(app: &TestApp, root: &FsPath, libraries_: &[(&str, &str)]) -> Vec<i64> {
    let queue = JobRuntime::new(app.db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    let mut ids = Vec::new();
    for (name, kind) in libraries_ {
        let path = root.join(name).to_string_lossy().into_owned();
        let id = libraries::create(&app.db, name, kind, &[path])
            .await
            .unwrap()
            .id;
        scan_library(&app.db, &queue, id).await.unwrap();
        ids.push(id);
    }
    ids
}

async fn create_user(app: &TestApp, name: &str, admin: bool, max_age: Option<i64>) -> i64 {
    let hash = flixnet_core::auth::hash_password(PASSWORD);
    users::create(
        &app.db,
        &users::NewUser {
            name,
            password_hash: Some(&hash),
            pin_hash: None,
            is_admin: admin,
            is_hidden: false,
            max_age_rating: max_age,
        },
    )
    .await
    .unwrap()
    .id
}

/// Probe data for a file without running ffprobe: H.264 1080p, AAC stereo (default),
/// AC-3 5.1, embedded SubRip and PGS subtitles.
async fn fake_probe(app: &TestApp, file_id: i64, duration_ms: i64) {
    let streams = vec![
        media::Stream {
            width: Some(1920),
            height: Some(1080),
            bit_depth: Some(8),
            hdr: Some("sdr".into()),
            fps: Some(23.976),
            ..stream(0, "video", "h264")
        },
        media::Stream {
            channels: Some(2),
            is_default: true,
            language: Some("deu".into()),
            ..stream(1, "audio", "aac")
        },
        media::Stream {
            channels: Some(6),
            language: Some("eng".into()),
            ..stream(2, "audio", "ac3")
        },
        media::Stream {
            language: Some("deu".into()),
            ..stream(3, "subtitle", "subrip")
        },
        stream(4, "subtitle", "hdmv_pgs_subtitle"),
    ];
    media::store_probe(
        &app.db,
        file_id,
        &media::ProbeData {
            container: Some("matroska,webm".into()),
            duration_ms: Some(duration_ms),
            bitrate: Some(5_000_000),
            streams,
            chapters: vec![],
        },
    )
    .await
    .unwrap();
}

struct Library {
    movies: String,
    shows: String,
    movie: String,
    movie_file: String,
    series: String,
    season: String,
    e1: String,
    e2: String,
    content: Vec<u8>,
}

/// A movie with 1000 known bytes and a poster, a series with two episodes; all with
/// fake probe data (100-minute movie, 40-minute episodes).
async fn library(app: &TestApp, root: &FsPath, token: &str) -> Library {
    let content: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    let movie = root.join("Filme/Alpha (2020)/Alpha.2020.mkv");
    std::fs::create_dir_all(movie.parent().unwrap()).unwrap();
    std::fs::write(&movie, &content).unwrap();
    for e in ["Dark.S01E01.mkv", "Dark.S01E02.mkv"] {
        let path = root.join("Serien/Dark/Staffel 1").join(e);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"e").unwrap();
    }
    let ids = scan(app, root, &[("Filme", "movies"), ("Serien", "shows")]).await;

    let views = app.get("/UserViews", Some(token)).await;
    assert_eq!(views.status, StatusCode::OK, "{}", views.json);
    let view = |n: &str| {
        views.json["Items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["Name"] == n)
            .unwrap()["Id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let (movies, shows) = (view("Filme"), view("Serien"));

    let first_id =
        |reply: &Reply, n: usize| reply.json["Items"][n]["Id"].as_str().unwrap().to_owned();
    let movie_list = app
        .get(
            &format!("/Items?ParentId={movies}&IncludeItemTypes=Movie&Recursive=true"),
            Some(token),
        )
        .await;
    let movie_id = first_id(&movie_list, 0);
    let series_list = app
        .get(&format!("/Items?ParentId={shows}"), Some(token))
        .await;
    let series = first_id(&series_list, 0);
    let seasons = app
        .get(&format!("/Shows/{series}/Seasons"), Some(token))
        .await;
    let season = first_id(&seasons, 0);
    let episodes = app
        .get(&format!("/Shows/{series}/Episodes"), Some(token))
        .await;

    // Fake probe data and a poster.
    for library in ids {
        for file in media::file_states(&app.db, library).await.unwrap() {
            let duration = if file.path.contains("Alpha") { 100 } else { 40 };
            fake_probe(app, file.id, duration * MIN).await;
        }
    }
    let movie_db_id = flixnet_jellyfin::ids::parse(&movie_id).unwrap().1;
    let poster = root.join("poster.jpg");
    image::RgbImage::from_pixel(60, 90, image::Rgb([200, 30, 30]))
        .save(&poster)
        .unwrap();
    metadata::replace_images(
        &app.db,
        movie_db_id,
        "local",
        &[metadata::NewImage {
            kind: "poster".into(),
            path: Some(poster.to_string_lossy().into_owned()),
            score: 1.0,
            ..Default::default()
        }],
    )
    .await
    .unwrap();
    let movie_file = media::files_for_item(&app.db, movie_db_id).await.unwrap()[0].id;

    Library {
        movies,
        shows,
        movie: movie_id,
        movie_file: flixnet_jellyfin::ids::guid(flixnet_jellyfin::ids::Kind::File, movie_file),
        series,
        season,
        e1: first_id(&episodes, 0),
        e2: first_id(&episodes, 1),
        content,
    }
}

fn report(item: &str, play_session: &str, position_ms: i64) -> Value {
    json!({
        "ItemId": item,
        "PlaySessionId": play_session,
        "PositionTicks": position_ms * TICKS,
        "PlayMethod": "DirectPlay",
        "IsPaused": false,
        "CanSeek": true,
    })
}

#[tokio::test]
async fn sign_in_browse_play_and_report_progress() {
    let app = app().await;
    let root = tempfile::tempdir().unwrap();
    let admin_id = create_user(&app, "Felix", true, None).await;

    // ---- Public endpoints and sign-in --------------------------------------------
    let info = app.get("/System/Info/Public", None).await;
    assert_eq!(info.status, StatusCode::OK);
    assert_keys(
        &info.json,
        &[
            "ServerName",
            "Version",
            "ProductName",
            "Id",
            "StartupWizardCompleted",
        ],
    );
    assert_eq!(info.json["ProductName"], "Jellyfin Server");
    assert_eq!(info.json["ServerName"], "Flixnet");
    assert_guid(&info.json["Id"]);
    assert_eq!(app.get("/system/ping", None).await.json, "Jellyfin Server");
    assert_eq!(app.get("/QuickConnect/Enabled", None).await.json, false);
    assert_eq!(app.get("/Users/Public", None).await.json, json!([]));
    assert_eq!(
        app.get("/Branding/Configuration", None).await.status,
        StatusCode::OK
    );

    assert_eq!(
        app.get("/Users/Me", None).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.get("/Items", None).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.sign_in("Felix", "falsch").await.status,
        StatusCode::UNAUTHORIZED
    );
    let no_device = app
        .send(
            Method::POST,
            "/Users/AuthenticateByName",
            Some(json!({ "Username": "Felix", "Pw": PASSWORD })),
            &[],
        )
        .await;
    assert_eq!(no_device.status, StatusCode::BAD_REQUEST);

    let auth = app.sign_in("Felix", PASSWORD).await;
    assert_eq!(auth.status, StatusCode::OK, "{}", auth.json);
    assert_pascal_case(&auth.json, "AuthenticationResult");
    assert_keys(
        &auth.json,
        &["User", "SessionInfo", "AccessToken", "ServerId"],
    );
    assert_keys(
        &auth.json["User"],
        &[
            "Name",
            "ServerId",
            "Id",
            "HasPassword",
            "Configuration",
            "Policy",
        ],
    );
    assert_keys(
        &auth.json["User"]["Policy"],
        &[
            "IsAdministrator",
            "IsHidden",
            "IsDisabled",
            "EnableAllFolders",
            "EnableMediaPlayback",
            "AuthenticationProviderId",
            "PasswordResetProviderId",
        ],
    );
    assert_eq!(auth.json["User"]["Policy"]["IsAdministrator"], true);
    assert_eq!(auth.json["ServerId"], info.json["Id"]);
    assert_eq!(auth.json["SessionInfo"]["DeviceId"], "device-1");
    assert_eq!(auth.json["SessionInfo"]["Client"], "Infuse");
    let user_guid = auth.json["User"]["Id"].as_str().unwrap().to_owned();
    assert_guid(&auth.json["User"]["Id"]);
    let token = auth.json["AccessToken"].as_str().unwrap().to_owned();
    let t = Some(token.as_str());

    // Every way Jellyfin clients pass the token works.
    for (name, value) in [
        ("x-emby-token", token.clone()),
        ("x-mediabrowser-token", token.clone()),
        ("x-emby-authorization", auth_header(t)),
    ] {
        let me = app
            .send(Method::GET, "/Users/Me", None, &[(name, value)])
            .await;
        assert_eq!(me.status, StatusCode::OK, "{name}");
        assert_eq!(me.json["Name"], "Felix");
    }
    let me = app
        .send(
            Method::GET,
            &format!("/Users/Me?api_key={token}"),
            None,
            &[],
        )
        .await;
    assert_eq!(me.json["Id"], user_guid.as_str());
    let by_id = app.get(&format!("/Users/{user_guid}"), t).await;
    assert_eq!(by_id.json["Name"], "Felix");
    assert_eq!(app.get("/System/Info", t).await.status, StatusCode::OK);

    // ---- Views and items ---------------------------------------------------------
    let lib = library(&app, root.path(), &token).await;
    let views = app.get(&format!("/Users/{user_guid}/Views"), t).await;
    assert_pascal_case(&views.json, "Views");
    assert_keys(&views.json, &["Items", "TotalRecordCount", "StartIndex"]);
    assert_eq!(views.json["TotalRecordCount"], 2);
    let movies_view = views.json["Items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["Id"] == lib.movies.as_str())
        .unwrap();
    assert_eq!(movies_view["Type"], "CollectionFolder");
    assert_eq!(movies_view["CollectionType"], "movies");
    assert_eq!(movies_view["IsFolder"], true);
    assert_eq!(movies_view["ChildCount"], 1);

    let movies = app
        .get(
            &format!(
                "/Items?ParentId={}&IncludeItemTypes=Movie&Recursive=true&SortBy=SortName&SortOrder=Ascending&Fields=Genres,ProviderIds,MediaSources,MediaStreams,Overview&StartIndex=0&Limit=50",
                lib.movies
            ),
            t,
        )
        .await;
    assert_eq!(movies.status, StatusCode::OK, "{}", movies.json);
    assert_pascal_case(&movies.json, "Items");
    assert_eq!(movies.json["TotalRecordCount"], 1);
    let movie = &movies.json["Items"][0];
    assert_keys(
        movie,
        &[
            "Name",
            "ServerId",
            "Id",
            "Type",
            "IsFolder",
            "ParentId",
            "RunTimeTicks",
            "ProductionYear",
            "UserData",
            "ImageTags",
            "BackdropImageTags",
            "MediaType",
            "LocationType",
            "MediaSources",
            "MediaStreams",
            "ProviderIds",
            "DateCreated",
        ],
    );
    assert_eq!(movie["Name"], "Alpha");
    assert_eq!(movie["Type"], "Movie");
    assert_eq!(movie["MediaType"], "Video");
    assert_eq!(movie["ProductionYear"], 2020);
    assert_eq!(movie["ParentId"], lib.movies.as_str());
    assert_eq!(movie["RunTimeTicks"], 100 * MIN * TICKS);
    assert!(movie.get("Overview").is_none(), "nulls are left out");
    assert!(
        movie["DateCreated"].as_str().unwrap().ends_with("0000Z"),
        "{}",
        movie["DateCreated"]
    );
    assert_keys(
        &movie["UserData"],
        &[
            "PlaybackPositionTicks",
            "PlayCount",
            "IsFavorite",
            "Played",
            "Key",
            "ItemId",
        ],
    );
    assert_eq!(movie["UserData"]["Played"], false);
    let poster_tag = movie["ImageTags"]["Primary"].as_str().unwrap().to_owned();
    let source = &movie["MediaSources"][0];
    assert_keys(
        source,
        &[
            "Protocol",
            "Id",
            "Type",
            "Container",
            "Size",
            "Name",
            "IsRemote",
            "RunTimeTicks",
            "SupportsTranscoding",
            "SupportsDirectStream",
            "SupportsDirectPlay",
            "IsInfiniteStream",
            "RequiresOpening",
            "RequiresClosing",
            "SupportsProbing",
            "MediaStreams",
            "RequiredHttpHeaders",
            "DefaultAudioStreamIndex",
        ],
    );
    assert_eq!(source["Id"], lib.movie_file.as_str());
    assert_eq!(source["Container"], "mkv");
    assert_eq!(source["Size"], 1000);
    assert_eq!(source["DefaultAudioStreamIndex"], 1);
    let streams = source["MediaStreams"].as_array().unwrap();
    assert_eq!(streams.len(), 5);
    assert_keys(
        &streams[0],
        &[
            "Codec",
            "Type",
            "Index",
            "IsDefault",
            "IsExternal",
            "IsTextSubtitleStream",
            "SupportsExternalStream",
            "VideoRange",
            "VideoRangeType",
            "DisplayTitle",
        ],
    );
    assert_eq!(streams[0]["Type"], "Video");
    assert_eq!(streams[0]["VideoRangeType"], "SDR");
    assert_eq!(streams[0]["DisplayTitle"], "1080p H264 SDR");
    assert_eq!(streams[1]["Type"], "Audio");
    assert_eq!(streams[1]["Language"], "deu");
    assert_eq!(streams[3]["Type"], "Subtitle");
    assert_eq!(streams[3]["IsTextSubtitleStream"], true);
    assert_eq!(streams[4]["Codec"], "PGSSUB");
    assert_eq!(streams[4]["IsTextSubtitleStream"], false);

    // Paths are case-insensitive and may carry the legacy /emby prefix.
    let detail = app
        .get(&format!("/emby/users/{user_guid}/items/{}", lib.movie), t)
        .await;
    assert_eq!(detail.status, StatusCode::OK, "{}", detail.json);
    assert_pascal_case(&detail.json, "Item");
    assert_eq!(detail.json["Name"], "Alpha");
    assert!(detail.json["SortName"].is_string());
    assert!(detail.json["MediaSources"].is_array());
    assert_eq!(
        app.get(&format!("/Items/{}", lib.movies), t).await.json["Type"],
        "CollectionFolder"
    );
    assert_eq!(
        app.get("/Items/f27caa37e5142225cceded48f6553502", t)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // Series → seasons → episodes.
    let shows = app.get(&format!("/Items?ParentId={}", lib.shows), t).await;
    assert_eq!(shows.json["Items"][0]["Type"], "Series");
    assert_eq!(shows.json["Items"][0]["IsFolder"], true);
    assert_eq!(shows.json["Items"][0]["ChildCount"], 1);
    let children = app.get(&format!("/Items?ParentId={}", lib.series), t).await;
    assert_eq!(children.json["Items"][0]["Id"], lib.season.as_str());
    assert_eq!(children.json["Items"][0]["Type"], "Season");
    let episodes = app
        .get(
            &format!(
                "/Shows/{}/Episodes?SeasonId={}&Fields=Overview",
                lib.series, lib.season
            ),
            t,
        )
        .await;
    assert_eq!(episodes.json["TotalRecordCount"], 2);
    let e1 = &episodes.json["Items"][0];
    assert_eq!(e1["Type"], "Episode");
    assert_eq!(e1["IndexNumber"], 1);
    assert_eq!(e1["ParentIndexNumber"], 1);
    assert_eq!(e1["SeriesId"], lib.series.as_str());
    assert_eq!(e1["SeasonId"], lib.season.as_str());
    assert_eq!(e1["SeriesName"], "Dark");
    let recursive = app
        .get(
            &format!(
                "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode",
                lib.series
            ),
            t,
        )
        .await;
    assert_eq!(recursive.json["TotalRecordCount"], 2);
    let by_season_number = app
        .get(
            &format!(
                "/Shows/{}/Episodes?Season=1&StartItemId={}",
                lib.series, lib.e2
            ),
            t,
        )
        .await;
    assert_eq!(by_season_number.json["Items"][0]["Id"], lib.e2.as_str());

    // Filters, search, latest, genres, people.
    let search = app.get("/Items?SearchTerm=alp&Recursive=true", t).await;
    assert_eq!(search.json["Items"][0]["Id"], lib.movie.as_str());
    let hints = app.get("/Search/Hints?SearchTerm=dar", t).await;
    assert_pascal_case(&hints.json, "SearchHints");
    assert!(
        hints.json["SearchHints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["Name"] == "Dark"
                && h["Type"] == "Series"
                && h["Id"] == lib.series.as_str()),
        "{}",
        hints.json
    );
    let only_boxsets = app
        .get("/Items?IncludeItemTypes=BoxSet&Recursive=true", t)
        .await;
    assert_eq!(only_boxsets.json["TotalRecordCount"], 0);
    let by_id = app
        .get(&format!("/Items?Ids={},{}", lib.movie, lib.e1), t)
        .await;
    assert_eq!(by_id.json["TotalRecordCount"], 2);
    let latest = app
        .get(&format!("/Items/Latest?ParentId={}", lib.shows), t)
        .await;
    assert_eq!(latest.status, StatusCode::OK, "{}", latest.json);
    assert_eq!(
        latest.json[0]["Id"],
        lib.series.as_str(),
        "grouped by series"
    );
    let latest_movies = app
        .get(
            &format!("/Users/{user_guid}/Items/Latest?ParentId={}", lib.movies),
            t,
        )
        .await;
    assert_eq!(latest_movies.json[0]["Id"], lib.movie.as_str());
    assert_eq!(app.get("/Genres", t).await.status, StatusCode::OK);
    assert_eq!(app.get("/Persons?Limit=10", t).await.status, StatusCode::OK);
    let similar = app.get(&format!("/Items/{}/Similar", lib.movie), t).await;
    assert_eq!(similar.status, StatusCode::OK);
    let ancestors = app.get(&format!("/Items/{}/Ancestors", lib.e1), t).await;
    let chain: Vec<&str> = ancestors
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["Type"].as_str().unwrap())
        .collect();
    assert_eq!(chain, ["Season", "Series", "CollectionFolder"]);

    // ---- Images ------------------------------------------------------------------
    let image = app
        .send(
            Method::GET,
            &format!(
                "/Items/{}/Images/Primary?tag={poster_tag}&maxWidth=40&quality=90",
                lib.movie
            ),
            None,
            &[],
        )
        .await;
    assert_eq!(image.status, StatusCode::OK, "tag is enough");
    assert_eq!(image.headers[header::CONTENT_TYPE], "image/jpeg");
    let decoded = image::load_from_memory(&image.bytes).unwrap();
    assert!(decoded.width() <= 92, "resized: {}", decoded.width());
    let forged = app
        .send(
            Method::GET,
            &format!(
                "/Items/{}/Images/Primary?tag=00000000000000000000000000000000",
                lib.movie
            ),
            None,
            &[],
        )
        .await;
    assert_eq!(forged.status, StatusCode::UNAUTHORIZED);
    let with_token = app
        .get(
            &format!("/Items/{}/Images/Primary/0?fillHeight=300", lib.movie),
            t,
        )
        .await;
    assert_eq!(with_token.status, StatusCode::OK);
    assert_eq!(
        app.get(&format!("/Items/{}/Images/Backdrop", lib.movie), t)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // ---- Playback info -----------------------------------------------------------
    let native = json!({
        "DeviceProfile": {
            "MaxStreamingBitrate": 120_000_000,
            "DirectPlayProfiles": [
                { "Container": "mkv,mp4", "Type": "Video", "VideoCodec": "h264,hevc", "AudioCodec": "aac,ac3" }
            ],
            "TranscodingProfiles": [
                { "Container": "mp4", "Type": "Video", "VideoCodec": "h264", "AudioCodec": "aac", "Protocol": "hls" }
            ]
        }
    });
    let info = app
        .post(
            &format!("/Items/{}/PlaybackInfo?UserId={user_guid}", lib.movie),
            native,
            t,
        )
        .await;
    assert_eq!(info.status, StatusCode::OK, "{}", info.json);
    assert_pascal_case(&info.json, "PlaybackInfo");
    assert_keys(&info.json, &["MediaSources", "PlaySessionId"]);
    let source = &info.json["MediaSources"][0];
    assert_eq!(source["SupportsDirectPlay"], true, "{source}");
    assert!(source.get("TranscodingUrl").is_none());
    assert_eq!(source["MediaStreams"][3]["DeliveryMethod"], "Embed");
    let direct_session = info.json["PlaySessionId"].as_str().unwrap().to_owned();

    // A browser-like profile (MP4 only) gets an HLS remux with WebVTT subtitles.
    let browser = json!({
        "AudioStreamIndex": 1,
        "SubtitleStreamIndex": 3,
        "DeviceProfile": {
            "DirectPlayProfiles": [
                { "Container": "mp4", "Type": "Video", "VideoCodec": "h264", "AudioCodec": "aac" }
            ]
        }
    });
    let info = app
        .post(&format!("/Items/{}/PlaybackInfo", lib.movie), browser, t)
        .await;
    let source = &info.json["MediaSources"][0];
    assert_eq!(source["SupportsDirectPlay"], false);
    assert_eq!(source["TranscodingSubProtocol"], "hls");
    assert_eq!(source["DefaultSubtitleStreamIndex"], 3);
    let transcoding_url = source["TranscodingUrl"].as_str().unwrap().to_owned();
    assert!(
        transcoding_url.contains("/master.m3u8?"),
        "{transcoding_url}"
    );
    let subtitles = &source["MediaStreams"][3];
    assert_eq!(subtitles["DeliveryMethod"], "External");
    assert!(
        subtitles["DeliveryUrl"]
            .as_str()
            .unwrap()
            .contains("/Stream.vtt?")
    );
    assert_eq!(source["MediaStreams"][4]["DeliveryMethod"], "Encode");
    let hls_session = info.json["PlaySessionId"].as_str().unwrap().to_owned();

    // ---- Direct stream with ranges ------------------------------------------------
    let stream_uri = format!(
        "/Videos/{}/stream.mkv?Static=true&MediaSourceId={}&api_key={token}",
        lib.movie, lib.movie_file
    );
    let ranged = app
        .send(
            Method::GET,
            &stream_uri,
            None,
            &[("range", "bytes=10-19".to_owned())],
        )
        .await;
    assert_eq!(ranged.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(ranged.bytes, &lib.content[10..20]);
    let whole = app
        .get(&format!("/Videos/{}/stream?static=true", lib.movie), t)
        .await;
    assert_eq!(whole.status, StatusCode::OK);
    assert_eq!(whole.bytes, lib.content);
    let anonymous = app
        .send(
            Method::GET,
            &format!("/Videos/{}/stream?static=true", lib.movie),
            None,
            &[],
        )
        .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

    // ---- HLS ---------------------------------------------------------------------
    let master = app.send(Method::GET, &transcoding_url, None, &[]).await;
    assert_eq!(master.status, StatusCode::OK, "{}", master.text());
    assert_eq!(
        master.headers[header::CONTENT_TYPE],
        "application/vnd.apple.mpegurl"
    );
    let master_text = master.text();
    assert!(master_text.starts_with("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH="));
    assert!(
        master_text.contains("RESOLUTION=1920x1080"),
        "{master_text}"
    );
    assert!(master_text.trim_end().ends_with("main.m3u8"));
    let main_url = transcoding_url
        .split('?')
        .next()
        .unwrap()
        .replace("master.m3u8", "main.m3u8");
    let main = app.send(Method::GET, &main_url, None, &[]).await;
    assert_eq!(main.status, StatusCode::OK);
    let main_text = main.text();
    assert!(main_text.contains("#EXT-X-MAP:URI=\"init.mp4\""));
    assert!(main_text.contains("#EXTINF:"));
    assert!(main_text.ends_with("#EXT-X-ENDLIST\n"));
    let unknown = app
        .send(
            Method::GET,
            &format!(
                "/Videos/{}/hls/0123456789abcdef0123456789abcdef/main.m3u8",
                lib.movie
            ),
            None,
            &[],
        )
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);

    // A client-built master playlist (no PlaybackInfo session) needs a token.
    let built = format!(
        "/Videos/{}/master.m3u8?MediaSourceId={}&VideoCodec=h264&AudioCodec=aac&MaxStreamingBitrate=4000000",
        lib.movie, lib.movie_file
    );
    assert_eq!(
        app.send(Method::GET, &built, None, &[]).await.status,
        StatusCode::UNAUTHORIZED
    );
    let built_master = app.get(&built, t).await;
    assert_eq!(built_master.status, StatusCode::OK);
    let variant = built_master.text().lines().last().unwrap().to_owned();
    assert!(
        variant.starts_with("hls/") && variant.ends_with("/main.m3u8"),
        "{variant}"
    );
    let variant_url = format!("/Videos/{}/{variant}", lib.movie);
    assert_eq!(
        app.send(Method::GET, &variant_url, None, &[]).await.status,
        StatusCode::OK
    );
    let built_main = app.get(&built.replace("master.m3u8", "main.m3u8"), t).await;
    assert!(
        built_main.text().contains("URI=\"hls/"),
        "{}",
        built_main.text()
    );

    // ---- Playstate: movie ----------------------------------------------------------
    let start = app
        .post(
            "/Sessions/Playing",
            report(&lib.movie, &direct_session, 0),
            t,
        )
        .await;
    assert_eq!(start.status, StatusCode::NO_CONTENT, "{}", start.json);
    let progress = app
        .post(
            "/Sessions/Playing/Progress",
            json!({ "ItemId": lib.movie, "PlaySessionId": direct_session,
                    "PositionTicks": 30 * MIN * TICKS, "IsPaused": true, "PlayMethod": "DirectPlay" }),
            t,
        )
        .await;
    assert_eq!(progress.status, StatusCode::NO_CONTENT, "{}", progress.json);
    let resume = app.get("/UserItems/Resume?MediaTypes=Video", t).await;
    assert_eq!(resume.json["TotalRecordCount"], 1, "{}", resume.json);
    assert_eq!(resume.json["Items"][0]["Id"], lib.movie.as_str());
    assert_eq!(
        resume.json["Items"][0]["UserData"]["PlaybackPositionTicks"],
        30 * MIN * TICKS
    );
    let stop = app
        .post(
            "/Sessions/Playing/Stopped",
            report(&lib.movie, &direct_session, 40 * MIN),
            t,
        )
        .await;
    assert_eq!(stop.status, StatusCode::NO_CONTENT);
    let legacy_resume = app
        .get(&format!("/Users/{user_guid}/Items/Resume"), t)
        .await;
    assert_eq!(
        legacy_resume.json["Items"][0]["UserData"]["PlaybackPositionTicks"],
        40 * MIN * TICKS
    );
    let events: Vec<String> = playstate::events(&app.db, admin_id)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert_eq!(
        events,
        ["start", "progress", "stop"],
        "one session, one history"
    );

    // The HLS play session: reports carry the transcode, the stop ends it.
    app.post(
        "/Sessions/Playing",
        report(&lib.movie, &hls_session, 40 * MIN),
        t,
    )
    .await;
    let stopped = app
        .post(
            "/Sessions/Playing/Stopped",
            report(&lib.movie, &hls_session, 41 * MIN),
            t,
        )
        .await;
    assert_eq!(stopped.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.send(Method::GET, &main_url, None, &[]).await.status,
        StatusCode::NOT_FOUND,
        "transcode session stopped"
    );
    let methods: Vec<Option<String>> = playstate::events(&app.db, admin_id)
        .await
        .unwrap()
        .into_iter()
        .skip(3)
        .map(|e| e.play_method)
        .collect();
    assert_eq!(methods, [Some("remux".into()), Some("remux".into())]);

    // ---- Playstate: episodes, next up ----------------------------------------------
    app.post("/Sessions/Playing", report(&lib.e1, "ep-session", 0), t)
        .await;
    app.post(
        "/Sessions/Playing/Stopped",
        report(&lib.e1, "ep-session", 39 * MIN),
        t,
    )
    .await;
    let e1 = app.get(&format!("/Items/{}", lib.e1), t).await;
    assert_eq!(e1.json["UserData"]["Played"], true, "watched near the end");
    let next_up = app.get("/Shows/NextUp?Limit=5", t).await;
    assert_eq!(next_up.status, StatusCode::OK);
    assert_eq!(next_up.json["Items"][0]["Id"], lib.e2.as_str());
    let series_next = app
        .get(&format!("/Shows/NextUp?SeriesId={}", lib.series), t)
        .await;
    assert_eq!(series_next.json["Items"][0]["Id"], lib.e2.as_str());

    // Legacy reporting endpoints.
    let legacy = app
        .post(
            &format!(
                "/Users/{user_guid}/PlayingItems/{}/Progress?PositionTicks={}",
                lib.e2,
                10 * MIN * TICKS
            ),
            json!({}),
            t,
        )
        .await;
    assert_eq!(legacy.status, StatusCode::NO_CONTENT, "{}", legacy.json);
    let legacy_stop = app
        .send(
            Method::DELETE,
            &format!(
                "/Users/{user_guid}/PlayingItems/{}?PositionTicks={}",
                lib.e2,
                12 * MIN * TICKS
            ),
            None,
            &[("authorization", auth_header(t))],
        )
        .await;
    assert_eq!(legacy_stop.status, StatusCode::NO_CONTENT);
    let e2 = app.get(&format!("/Items/{}", lib.e2), t).await;
    assert_eq!(
        e2.json["UserData"]["PlaybackPositionTicks"],
        12 * MIN * TICKS
    );

    // ---- Played / favorite -------------------------------------------------------
    let played = app
        .post(&format!("/UserPlayedItems/{}", lib.e2), json!({}), t)
        .await;
    assert_pascal_case(&played.json, "UserData");
    assert_eq!(played.json["Played"], true);
    assert_eq!(played.json["PlaybackPositionTicks"], 0);
    let unplayed = app
        .send(
            Method::DELETE,
            &format!("/Users/{user_guid}/PlayedItems/{}", lib.e2),
            None,
            &[("authorization", auth_header(t))],
        )
        .await;
    assert_eq!(unplayed.json["Played"], false);
    let favorite = app
        .post(&format!("/UserFavoriteItems/{}", lib.movie), json!({}), t)
        .await;
    assert_eq!(favorite.json["IsFavorite"], true);
    let favorites = app.get("/Items?Recursive=true&Filters=IsFavorite", t).await;
    assert_eq!(favorites.json["TotalRecordCount"], 1);
    assert_eq!(favorites.json["Items"][0]["Id"], lib.movie.as_str());
    let data = app
        .get(&format!("/UserItems/{}/UserData", lib.movie), t)
        .await;
    assert_eq!(data.json["IsFavorite"], true);

    // ---- Misc --------------------------------------------------------------------
    for path in ["/Sessions/Capabilities/Full", "/Sessions/Capabilities"] {
        assert_eq!(
            app.post(path, json!({ "PlayableMediaTypes": ["Video"] }), t)
                .await
                .status,
            StatusCode::NO_CONTENT
        );
    }
    let sessions = app.get("/Sessions?DeviceId=device-1", t).await;
    assert_pascal_case(&sessions.json, "Sessions");
    assert_eq!(sessions.json[0]["DeviceId"], "device-1");
    let prefs_url = "/DisplayPreferences/usersettings?client=emby";
    let prefs = app.get(prefs_url, t).await;
    assert_eq!(prefs.json["SortBy"], "SortName");
    let saved = app
        .post(
            prefs_url,
            json!({ "Id": "usersettings", "SortBy": "DateCreated", "CustomPrefs": { "homesection0": "resume" } }),
            t,
        )
        .await;
    assert_eq!(saved.status, StatusCode::NO_CONTENT);
    let prefs = app.get(prefs_url, t).await;
    assert_eq!(prefs.json["SortBy"], "DateCreated");
    assert_eq!(prefs.json["CustomPrefs"]["homesection0"], "resume");

    // Sign-out revokes the token.
    let logout = app.post("/Sessions/Logout", json!({}), t).await;
    assert_eq!(logout.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.get("/Users/Me", t).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn library_access_and_age_limits_apply() {
    let app = app().await;
    let root = tempfile::tempdir().unwrap();
    create_user(&app, "Felix", true, None).await;
    let admin = app.token("Felix").await;
    let lib = library(&app, root.path(), &admin).await;

    // No libraries granted.
    let guest = create_user(&app, "Gast", false, None).await;
    libraries::set_access(&app.db, guest, false, &[])
        .await
        .unwrap();
    let guest_token = app.token("Gast").await;
    let g = Some(guest_token.as_str());
    assert_eq!(app.get("/UserViews", g).await.json["TotalRecordCount"], 0);
    assert_eq!(
        app.get("/Items?Recursive=true", g).await.json["TotalRecordCount"],
        0
    );
    for uri in [
        format!("/Items/{}", lib.movie),
        format!("/Items/{}", lib.movies),
        format!("/Shows/{}/Episodes", lib.series),
    ] {
        assert_eq!(
            app.get(&uri, g).await.status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    let info = app
        .post(&format!("/Items/{}/PlaybackInfo", lib.movie), json!({}), g)
        .await;
    assert_eq!(info.status, StatusCode::NOT_FOUND);
    let stream = app
        .get(&format!("/Videos/{}/stream?static=true", lib.movie), g)
        .await;
    assert_eq!(stream.status, StatusCode::NOT_FOUND);
    let report = app
        .post("/Sessions/Playing", report(&lib.movie, "x", 0), g)
        .await;
    assert_eq!(report.status, StatusCode::NOT_FOUND);
    let favorite = app
        .post(&format!("/UserFavoriteItems/{}", lib.movie), json!({}), g)
        .await;
    assert_eq!(favorite.status, StatusCode::NOT_FOUND);
    // Users only see themselves.
    let users = app.get("/Users", g).await;
    assert_eq!(users.json.as_array().unwrap().len(), 1);

    // An age limit hides unrated content.
    create_user(&app, "Kind", false, Some(12)).await;
    let kid = app.token("Kind").await;
    let k = Some(kid.as_str());
    assert_eq!(app.get("/UserViews", k).await.json["TotalRecordCount"], 2);
    assert_eq!(
        app.get("/Items?Recursive=true", k).await.json["TotalRecordCount"],
        0
    );
    assert_eq!(
        app.get(&format!("/Items/{}", lib.movie), k).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.get("/Search/Hints?SearchTerm=alpha", k).await.json["SearchHints"],
        json!([])
    );
}

/// Real media through ffmpeg: HLS segments and subtitle conversion.
#[tokio::test]
async fn hls_segments_and_subtitles_through_ffmpeg() {
    let app = app().await;
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("Filme/Clip (2020)");
    std::fs::create_dir_all(&folder).unwrap();
    let srt = root.path().join("subs.srt");
    std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:03,000\nHallo Welt\n").unwrap();
    let status = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc=s=320x240:r=25:d=8"])
        .args(["-f", "lavfi", "-i", "sine=duration=8"])
        .arg("-i")
        .arg(&srt)
        .args(["-map", "0", "-map", "1", "-map", "2"])
        .args(["-c:v", "mpeg4", "-c:a", "libmp3lame", "-c:s", "srt"])
        .arg(folder.join("Clip.2020.mkv"))
        .status()
        .expect("ffmpeg on PATH");
    assert!(status.success());
    let library = scan(&app, root.path(), &[("Filme", "movies")]).await[0];
    let ffprobe = flixnet_media::Ffprobe::new("ffprobe");
    for file in media::file_states(&app.db, library).await.unwrap() {
        flixnet_library::jobs::probe_file(&app.db, &ffprobe, file.id)
            .await
            .unwrap();
    }
    create_user(&app, "Felix", true, None).await;
    let token = app.token("Felix").await;
    let t = Some(token.as_str());
    let movie = app
        .get("/Items?Recursive=true&IncludeItemTypes=Movie", t)
        .await
        .json["Items"][0]["Id"]
        .as_str()
        .unwrap()
        .to_owned();

    // A client that plays only H.264/AAC in MP4: MPEG-4 Part 2 is transcoded.
    let info = app
        .post(
            &format!("/Items/{movie}/PlaybackInfo"),
            json!({ "DeviceProfile": { "DirectPlayProfiles": [
                { "Container": "mp4", "Type": "Video", "VideoCodec": "h264", "AudioCodec": "aac" }
            ]}}),
            t,
        )
        .await;
    let source = &info.json["MediaSources"][0];
    let url = source["TranscodingUrl"].as_str().unwrap().to_owned();
    let base = url.split('?').next().unwrap().replace("master.m3u8", "");
    let master = app.send(Method::GET, &url, None, &[]).await;
    assert!(
        master.text().contains("RESOLUTION=320x240"),
        "{}",
        master.text()
    );
    let main = app
        .send(Method::GET, &format!("{base}main.m3u8"), None, &[])
        .await;
    assert_eq!(main.text().matches("#EXTINF").count(), 2);
    let init = app
        .send(Method::GET, &format!("{base}init.mp4"), None, &[])
        .await;
    assert_eq!(init.status, StatusCode::OK);
    assert_eq!(&init.bytes[4..8], b"ftyp");
    let segment = app
        .send(Method::GET, &format!("{base}1.m4s"), None, &[])
        .await;
    assert_eq!(segment.status, StatusCode::OK);
    assert!(segment.bytes.len() > 1000);
    assert_eq!(
        app.send(Method::GET, &format!("{base}9.m4s"), None, &[])
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // Subtitles: the signed DeliveryUrl works without a token, as VTT and SRT.
    let subtitle = source["MediaStreams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["Type"] == "Subtitle")
        .unwrap();
    assert_eq!(subtitle["Codec"], "subrip");
    let delivery = subtitle["DeliveryUrl"].as_str().unwrap().to_owned();
    let vtt = app.send(Method::GET, &delivery, None, &[]).await;
    assert_eq!(vtt.status, StatusCode::OK, "{}", vtt.text());
    assert!(vtt.text().starts_with("WEBVTT"), "{}", vtt.text());
    assert!(vtt.text().contains("Hallo Welt"));
    let srt = app
        .send(
            Method::GET,
            &delivery.replace("Stream.vtt", "Stream.srt"),
            None,
            &[],
        )
        .await;
    assert!(
        srt.text()
            .starts_with("1\n00:00:01,000 --> 00:00:03,000\nHallo Welt"),
        "{}",
        srt.text()
    );
    let unsigned = delivery.split('?').next().unwrap().to_owned();
    assert_eq!(
        app.send(Method::GET, &unsigned, None, &[]).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(app.get(&unsigned, t).await.status, StatusCode::OK);

    // Closing the player ends the transcode.
    let play_session = info.json["PlaySessionId"].as_str().unwrap();
    let stop = app
        .send(
            Method::DELETE,
            &format!("/Videos/ActiveEncodings?DeviceId=device-1&PlaySessionId={play_session}"),
            None,
            &[("authorization", auth_header(t))],
        )
        .await;
    assert_eq!(stop.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.send(Method::GET, &format!("{base}main.m3u8"), None, &[])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}
