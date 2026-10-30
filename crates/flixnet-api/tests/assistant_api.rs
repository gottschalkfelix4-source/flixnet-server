//! Milestones 6.4–6.7 against a mocked LLM (wiremock): natural-language search,
//! explanations from the viewer's history, spoiler-free recaps, the chat tools,
//! theme collections and scene search — each also as a restricted profile, which
//! must never see (nor have sent to the LLM) anything above its limit.
//!
//! Restricted content carries the marker `Geheim` in titles, plots and dialogue.

mod common;

use axum::http::{Method, StatusCode};
use common::{PASSWORD, TestApp, app, credentials};
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::transcripts::{self, NewChunk};
use flixnet_db::{items, libraries, media, userdata};
use flixnet_library::refresh::refresh_local;
use flixnet_library::scan::scan_library;
use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const MARKER: &str = "Geheim";

fn write(root: &std::path::Path, relative: &str, content: &[u8]) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

struct Fixture {
    admin: String,
    kid: String,
    kid_id: i64,
    movies: i64,
    shows: i64,
    _media: tempfile::TempDir,
}

/// Movies: Kinderfilm (FSK 0, 2020, 90 min), Lachfilm (FSK 6, Komödie, 1995,
/// 100 min), Langer Lachfilm (FSK 6, Komödie, 1996, 150 min), Teenfilm (FSK 12),
/// and restricted Geheimfilm (FSK 18, Thriller), Geheim Zwei (FSK 16, Thriller),
/// Geheim Drei (FSK 18, Thriller). Anton Alt acts in Kinderfilm, Teenfilm and
/// Geheimfilm. Series: Kinderserie (FSK 6) with three episodes.
async fn fixture(app: &TestApp) -> Fixture {
    let admin = app.setup_admin().await;
    let dir = tempfile::tempdir().unwrap();
    let movies = dir.path().join("Filme");
    for (title, year, mpaa, genre, runtime, actor) in [
        ("Kinderfilm", 2020, "FSK 0", "Familie", 90, "Anton Alt"),
        ("Lachfilm", 1995, "FSK 6", "Komödie", 100, "Clara Clown"),
        (
            "Langer Lachfilm",
            1996,
            "FSK 6",
            "Komödie",
            150,
            "Clara Clown",
        ),
        ("Teenfilm", 2021, "FSK 12", "Abenteuer", 95, "Anton Alt"),
        ("Geheimfilm", 2019, "FSK 18", "Thriller", 110, "Anton Alt"),
        (
            "Geheim Zwei",
            2018,
            "FSK 16",
            "Thriller",
            105,
            "Gustav Geheim",
        ),
        (
            "Geheim Drei",
            2017,
            "FSK 18",
            "Thriller",
            99,
            "Gustav Geheim",
        ),
    ] {
        let folder = format!("{title} ({year})");
        write(&movies, &format!("{folder}/{title}.{year}.mkv"), b"video");
        write(
            &movies,
            &format!("{folder}/movie.nfo"),
            format!(
                "<movie><title>{title}</title><year>{year}</year><mpaa>{mpaa}</mpaa>\
                 <genre>{genre}</genre><runtime>{runtime}</runtime><plot>Über {title}.</plot>\
                 <actor><name>{actor}</name><role>Hauptrolle</role></actor></movie>"
            )
            .as_bytes(),
        );
    }
    let shows = dir.path().join("Serien");
    write(
        &shows,
        "Kinderserie (2016)/tvshow.nfo",
        b"<tvshow><title>Kinderserie</title><mpaa>FSK 6</mpaa><genre>Familie</genre></tvshow>",
    );
    for n in 1..=3 {
        let stem = format!("Kinderserie.S01E0{n}");
        write(
            &shows,
            &format!("Kinderserie (2016)/Staffel 1/{stem}.mkv"),
            b"video",
        );
        write(
            &shows,
            &format!("Kinderserie (2016)/Staffel 1/{stem}.nfo"),
            format!(
                "<episodedetails><title>Folge {n}</title><season>1</season><episode>{n}</episode>\
                 <plot>INHALT-E{n}</plot></episodedetails>"
            )
            .as_bytes(),
        );
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
    let created = app
        .post(
            "/api/v1/users",
            json!({ "name": "Kind", "password": PASSWORD, "max_age_rating": 12 }),
            Some(&admin),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    let login = app
        .post(
            "/api/v1/auth/login",
            credentials("Kind", PASSWORD, "device-kind"),
            None,
        )
        .await;
    Fixture {
        admin,
        kid: login.json["token"].as_str().unwrap().to_owned(),
        kid_id: created.json["id"].as_i64().unwrap(),
        movies: movies_id,
        shows: shows_id,
        _media: dir,
    }
}

async fn id(app: &TestApp, library: i64, kind: &str, title: &str) -> i64 {
    items::list_by_kind(&app.db, library, kind)
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.title == title)
        .unwrap_or_else(|| panic!("no {kind} {title}"))
        .id
}

async fn ids(app: &TestApp, library: i64, titles: &[&str]) -> Vec<i64> {
    let mut out = Vec::new();
    for title in titles {
        out.push(id(app, library, "movie", title).await);
    }
    out
}

async fn file_of(app: &TestApp, item: i64) -> i64 {
    media::files_for_item(&app.db, item).await.unwrap()[0].id
}

fn chunk(start_s: i64, end_s: i64, text: &str) -> NewChunk {
    NewChunk {
        start_ms: start_s * 1000,
        end_ms: end_s * 1000,
        text: text.to_owned(),
    }
}

async fn store_chunks(app: &TestApp, item: i64, chunks: &[NewChunk]) {
    let file = file_of(app, item).await;
    transcripts::store(&app.db, file, "subtitle", 3, Some("ger"), "sig", chunks)
        .await
        .unwrap();
}

async fn use_llm(app: &TestApp, f: &Fixture, server: &MockServer) {
    let r = app
        .request(
            Method::PUT,
            "/api/v1/settings/llm",
            Some(json!({
                "enabled": true,
                "base_url": format!("{}/v1", server.uri()),
                "model": "test-model",
                "api_key": "geheimer-schluessel"
            })),
            Some(&f.admin),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["configured"], true);
    assert!(
        !r.json.to_string().contains("geheimer-schluessel"),
        "key is never returned"
    );
}

fn tool_call(name: &str, arguments: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{ "message": {
            "role": "assistant", "content": null,
            "tool_calls": [{ "id": format!("call-{name}"), "type": "function",
                             "function": { "name": name, "arguments": arguments.to_string() } }]
        }}]
    }))
}

fn text(content: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(
        json!({ "choices": [{ "message": { "role": "assistant", "content": content } }] }),
    )
}

async fn bodies(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

fn titles(items: &Value) -> Vec<String> {
    items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn natural_language_search_uses_the_llm_filter_within_the_profile() {
    let app = app().await;
    let f = fixture(&app).await;

    // Without an LLM: keyword search, filler words dropped.
    let r = app
        .get(
            "/api/v1/assistant/search?q=der%20lustige%20Lachfilm",
            Some(&f.admin),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["mode"], "keyword");
    assert!(
        titles(&r.json["items"]).contains(&"Lachfilm".to_owned()),
        "{}",
        r.json
    );
    let status = app.get("/api/v1/assistant/status", Some(&f.kid)).await;
    assert_eq!(status.json["llm_enabled"], false);

    let server = MockServer::start().await;
    use_llm(&app, &f, &server).await;
    Mock::given(method("POST"))
        .respond_with(tool_call(
            "set_filter",
            json!({"kinds": ["movie"], "genres": ["Komödie"], "year_from": 1990, "year_to": 1999,
                   "max_runtime_minutes": 120, "played": false}),
        ))
        .mount(&server)
        .await;
    let q = "lustige%20Filme%20aus%20den%2090ern%20unter%202%20Stunden%20die%20ich%20noch%20nicht%20gesehen%20habe";
    let r = app
        .get(&format!("/api/v1/assistant/search?q={q}"), Some(&f.admin))
        .await;
    assert_eq!(r.json["mode"], "llm", "{}", r.json);
    assert_eq!(
        titles(&r.json["items"]),
        vec!["Lachfilm"],
        "the 150 min film is too long"
    );
    assert_eq!(r.json["interpreted"]["max_runtime_minutes"], 120);
    assert_eq!(r.json["interpreted"]["genres"], json!(["Komödie"]));

    // Watched films drop out.
    let lachfilm = id(&app, f.movies, "movie", "Lachfilm").await;
    let r = app
        .request(
            Method::PUT,
            &format!("/api/v1/items/{lachfilm}/played"),
            None,
            Some(&f.admin),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = app
        .get(&format!("/api/v1/assistant/search?q={q}"), Some(&f.admin))
        .await;
    assert_eq!(r.json["total"], 0, "{}", r.json);

    // A model asking for restricted content gets nothing restricted for the child, and
    // the restricted genre is not even offered to it.
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(tool_call(
            "set_filter",
            json!({"genres": ["Thriller"], "search_text": "Geheim", "people": ["Gustav Geheim"]}),
        ))
        .mount(&server)
        .await;
    let r = app
        .get(
            "/api/v1/assistant/search?q=spannende%20Geheimfilme",
            Some(&f.kid),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let body = r.json.to_string();
    assert!(
        !body.contains("Geheimfilm") && !body.contains("Geheim Zwei"),
        "{body}"
    );
    let sent = bodies(&server).await;
    let genres =
        &sent[0]["tools"][0]["function"]["parameters"]["properties"]["genres"]["items"]["enum"];
    assert!(
        genres.as_array().unwrap().iter().all(|g| g != "Thriller"),
        "{genres}"
    );
    assert!(
        genres.as_array().unwrap().iter().any(|g| g == "Familie"),
        "{genres}"
    );

    // Malformed answers twice → keyword fallback with a notice.
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(text("Das weiß ich nicht."))
        .mount(&server)
        .await;
    let r = app
        .get("/api/v1/assistant/search?q=Lachfilm", Some(&f.admin))
        .await;
    assert_eq!(r.json["mode"], "keyword");
    assert!(r.json["notice"].as_str().is_some());
    assert_eq!(bodies(&server).await.len(), 2, "one retry");
}

#[tokio::test]
async fn explanations_come_from_the_viewers_own_visible_history() {
    let app = app().await;
    let f = fixture(&app).await;
    let kinderfilm = id(&app, f.movies, "movie", "Kinderfilm").await;
    let teenfilm = id(&app, f.movies, "movie", "Teenfilm").await;
    let geheimfilm = id(&app, f.movies, "movie", "Geheimfilm").await;
    let r = app
        .request(
            Method::PUT,
            &format!("/api/v1/items/{kinderfilm}/played"),
            None,
            Some(&f.kid),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    // History from before the age limit: restricted, so it never counts.
    userdata::set_played(&app.db, f.kid_id, geheimfilm, true)
        .await
        .unwrap();

    let r = app
        .get(
            &format!("/api/v1/assistant/explain/{teenfilm}"),
            Some(&f.kid),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["generated"], false);
    let text_ = r.json["text"].as_str().unwrap();
    assert!(text_.starts_with("Weil du „Kinderfilm“"), "{text_}");
    assert!(text_.contains("Anton Alt"), "{text_}");
    assert!(!r.json.to_string().contains(MARKER), "{}", r.json);
    let r = app
        .get(
            &format!("/api/v1/assistant/explain/{geheimfilm}"),
            Some(&f.kid),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let server = MockServer::start().await;
    use_llm(&app, &f, &server).await;
    Mock::given(method("POST"))
        .respond_with(text(
            "Weil du „Kinderfilm“ gesehen hast, passt das Abenteuer mit Anton Alt.",
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // An invented title is rejected in favour of the rule-based sentence.
    Mock::given(method("POST"))
        .respond_with(text("Weil du „Der Pate“ mochtest."))
        .mount(&server)
        .await;
    let r = app
        .get(
            &format!("/api/v1/assistant/explain/{teenfilm}"),
            Some(&f.kid),
        )
        .await;
    assert_eq!(r.json["generated"], true, "{}", r.json);
    assert!(r.json["text"].as_str().unwrap().contains("Abenteuer"));
    let r = app
        .get(
            &format!("/api/v1/assistant/explain/{teenfilm}"),
            Some(&f.kid),
        )
        .await;
    assert_eq!(r.json["generated"], false, "{}", r.json);
    for body in bodies(&server).await {
        assert!(
            !body.to_string().contains(MARKER),
            "sent to the LLM: {body}"
        );
    }
}

#[tokio::test]
async fn recaps_only_use_material_before_the_position() {
    let app = app().await;
    let f = fixture(&app).await;
    let e1 = id(&app, f.shows, "episode", "Folge 1").await;
    let e2 = id(&app, f.shows, "episode", "Folge 2").await;
    let e3 = id(&app, f.shows, "episode", "Folge 3").await;
    store_chunks(
        &app,
        e1,
        &[chunk(0, 40, "E1-ANFANG"), chunk(40, 80, "E1-ENDE")],
    )
    .await;
    store_chunks(
        &app,
        e2,
        &[
            chunk(0, 30, "E2-VORHER-EINS"),
            chunk(30, 60, "E2-VORHER-ZWEI"),
            chunk(55, 90, "E2-NACHHER-UEBERLAPPEND"),
            chunk(90, 120, "E2-NACHHER-ZWEI"),
        ],
    )
    .await;
    store_chunks(&app, e3, &[chunk(0, 40, "E3-ZUKUNFT")]).await;

    // Without an LLM: earlier overviews only.
    let r = app
        .get(
            &format!("/api/v1/assistant/recap/{e2}?position_ms=60000"),
            Some(&f.kid),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["mode"], "overviews");
    let recap = r.json["text"].as_str().unwrap();
    assert!(
        recap.contains("INHALT-E1") && !recap.contains("INHALT-E2"),
        "{recap}"
    );

    let server = MockServer::start().await;
    use_llm(&app, &f, &server).await;
    Mock::given(method("POST"))
        .respond_with(text("Bisher: Die Kinder planen einen Ausflug."))
        .mount(&server)
        .await;
    let r = app
        .get(
            &format!("/api/v1/assistant/recap/{e2}?position_ms=60000"),
            Some(&f.kid),
        )
        .await;
    assert_eq!(r.json["mode"], "llm", "{}", r.json);
    assert_eq!(r.json["text"], "Bisher: Die Kinder planen einen Ausflug.");

    // The resume point is the default position.
    flixnet_db::playstate::save_progress(&app.db, f.kid_id, e2, 150_000, Some(1_200_000))
        .await
        .unwrap();
    let r = app
        .get(&format!("/api/v1/assistant/recap/{e2}"), Some(&f.kid))
        .await;
    assert_eq!(r.json["position_ms"], 150_000, "{}", r.json);

    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 2);
    let first = sent[0].to_string();
    for expected in [
        "E1-ANFANG",
        "E1-ENDE",
        "INHALT-E1",
        "E2-VORHER-EINS",
        "E2-VORHER-ZWEI",
    ] {
        assert!(first.contains(expected), "{expected} missing: {first}");
    }
    // Nothing after 60 s: not even the chunk that starts before and ends after it.
    for forbidden in ["NACHHER", "E3-ZUKUNFT", "INHALT-E2", "INHALT-E3"] {
        assert!(
            !first.contains(forbidden),
            "{forbidden} sent to the LLM: {first}"
        );
    }
    let second = sent[1].to_string();
    assert!(second.contains("E2-NACHHER-ZWEI"), "{second}");
    for forbidden in ["E3-ZUKUNFT", "INHALT-E2", "INHALT-E3"] {
        assert!(
            !second.contains(forbidden),
            "{forbidden} sent to the LLM: {second}"
        );
    }

    // A movie at its start has nothing to recap.
    let kinderfilm = id(&app, f.movies, "movie", "Kinderfilm").await;
    let r = app
        .get(
            &format!("/api/v1/assistant/recap/{kinderfilm}?position_ms=0"),
            Some(&f.kid),
        )
        .await;
    assert_eq!(r.json["mode"], "empty");
    let geheimfilm = id(&app, f.movies, "movie", "Geheimfilm").await;
    let r = app
        .get(
            &format!("/api/v1/assistant/recap/{geheimfilm}"),
            Some(&f.kid),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn chat_tools_never_hand_restricted_items_to_the_llm() {
    let app = app().await;
    let f = fixture(&app).await;
    let geheimfilm = id(&app, f.movies, "movie", "Geheimfilm").await;
    let kinderfilm = id(&app, f.movies, "movie", "Kinderfilm").await;
    store_chunks(
        &app,
        geheimfilm,
        &[chunk(60, 100, "Das Geheimnis liegt im Tresor.")],
    )
    .await;
    store_chunks(
        &app,
        kinderfilm,
        &[chunk(120, 160, "Der Tresor ist offen!")],
    )
    .await;

    let ask =
        json!({ "messages": [{ "role": "user", "content": "Was gibt es mit einem Tresor?" }] });
    let r = app
        .post("/api/v1/assistant/chat", ask.clone(), Some(&f.kid))
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "no LLM configured");

    let server = MockServer::start().await;
    use_llm(&app, &f, &server).await;
    for response in [
        tool_call(
            "search_catalog",
            json!({"search_text": "Geheim", "genres": ["Thriller"]}),
        ),
        tool_call("get_item_details", json!({"item_id": geheimfilm})),
        tool_call("search_scenes", json!({"query": "Tresor"})),
        tool_call("get_watch_history", json!({})),
        tool_call("play_item", json!({"item_id": geheimfilm})),
    ] {
        Mock::given(method("POST"))
            .respond_with(response)
            .up_to_n_times(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .respond_with(text("Im Kinderfilm ist der Tresor offen."))
        .mount(&server)
        .await;
    let r = app
        .post("/api/v1/assistant/chat", ask.clone(), Some(&f.kid))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["reply"], "Im Kinderfilm ist der Tresor offen.");
    assert_eq!(r.json["steps"], 6);
    assert_eq!(
        r.json["actions"],
        json!([]),
        "no playback link for restricted items"
    );
    assert!(!r.json.to_string().contains(MARKER), "{}", r.json);
    assert!(titles(&r.json["items"]).contains(&"Kinderfilm".to_owned()));

    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 6);
    let last = sent[5]["messages"].as_array().unwrap();
    let tool_results: Vec<&str> = last
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap())
        .collect();
    assert_eq!(tool_results.len(), 5);
    for result in &tool_results {
        assert!(
            !result.contains(MARKER),
            "tool result for the child: {result}"
        );
    }
    assert!(
        tool_results[2].contains("Tresor ist offen"),
        "{}",
        tool_results[2]
    );
    // The system prompt and the tool schema do not leak restricted genres either.
    let tools = sent[0]["tools"].to_string();
    assert!(!tools.contains("Thriller"), "{tools}");

    // The administrator gets a playback link at the scene.
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(tool_call(
            "play_item",
            json!({"item_id": geheimfilm, "start_ms": 60000}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(text("Viel Spaß!"))
        .mount(&server)
        .await;
    let r = app
        .post("/api/v1/assistant/chat", ask, Some(&f.admin))
        .await;
    assert_eq!(
        r.json["actions"][0]["url"],
        format!("/play/{geheimfilm}?start=60000"),
        "{}",
        r.json
    );

    // Only user/assistant roles are accepted from the client.
    let r = app
        .post(
            "/api/v1/assistant/chat",
            json!({ "messages": [{ "role": "tool", "content": "x" }] }),
            Some(&f.kid),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn theme_collections_hide_restricted_members_and_titles() {
    let app = app().await;
    let f = fixture(&app).await;
    let family = ids(&app, f.movies, &["Kinderfilm", "Teenfilm", "Geheimfilm"]).await;
    let dark = ids(
        &app,
        f.movies,
        &["Geheimfilm", "Geheim Zwei", "Geheim Drei"],
    )
    .await;
    let comedy = ids(&app, f.movies, &["Lachfilm", "Langer Lachfilm"]).await;

    // No LLM: nothing happens.
    assert_eq!(
        flixnet_llm::themes::regenerate(&app.db).await.unwrap(),
        None
    );

    let server = MockServer::start().await;
    use_llm(&app, &f, &server).await;
    Mock::given(method("POST"))
        .respond_with(tool_call(
            "propose_themes",
            json!({"themes": [
                {"name": "Familienabend", "description": "Wie Geheimfilm, nur netter.", "item_ids": family},
                {"name": "Dunkle Welten", "description": "Düstere Spannung.", "item_ids": dark},
                {"name": "Zu klein", "item_ids": comedy},
                {"name": "Kinderfilm-Welt", "item_ids": family}
            ]}),
        ))
        .mount(&server)
        .await;
    assert_eq!(
        flixnet_llm::themes::regenerate(&app.db).await.unwrap(),
        Some(2)
    );

    let admin = app.get("/api/v1/collections", Some(&f.admin)).await;
    let names: Vec<&str> = admin
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"Familienabend") && names.contains(&"Dunkle Welten"),
        "{names:?}"
    );
    let familienabend = admin
        .json
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Familienabend")
        .unwrap()
        .clone();
    assert_eq!(familienabend["kind"], "theme");
    assert_eq!(
        familienabend["overview"],
        Value::Null,
        "description named a title"
    );

    let kid = app.get("/api/v1/collections", Some(&f.kid)).await;
    let kid_body = kid.json.to_string();
    assert!(
        !kid_body.contains("Dunkle Welten"),
        "only restricted members: {kid_body}"
    );
    assert!(!kid_body.contains(MARKER), "{kid_body}");
    let kid_family = kid
        .json
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Familienabend")
        .unwrap();
    assert_eq!(kid_family["item_count"], 2);
    let id_ = kid_family["id"].as_i64().unwrap();
    let detail = app
        .get(&format!("/api/v1/collections/{id_}"), Some(&f.kid))
        .await;
    assert_eq!(
        titles(&detail.json["items"]),
        vec!["Kinderfilm", "Teenfilm"]
    );
    // Theme collections are not edited by hand.
    let r = app
        .request(
            Method::PATCH,
            &format!("/api/v1/collections/{id_}"),
            Some(json!({"name": "Neu"})),
            Some(&f.admin),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);

    // A new run replaces the themes.
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(tool_call(
            "propose_themes",
            json!({"themes": [{"name": "Lachen", "item_ids": [comedy[0], comedy[1], family[0]]}]}),
        ))
        .mount(&server)
        .await;
    assert_eq!(
        flixnet_llm::themes::regenerate(&app.db).await.unwrap(),
        Some(1)
    );
    let admin = app.get("/api/v1/collections", Some(&f.admin)).await;
    let names: Vec<&str> = admin
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Lachen"]);
}

#[tokio::test]
async fn scene_search_respects_the_profile() {
    let app = app().await;
    let f = fixture(&app).await;
    let geheimfilm = id(&app, f.movies, "movie", "Geheimfilm").await;
    let kinderfilm = id(&app, f.movies, "movie", "Kinderfilm").await;
    store_chunks(
        &app,
        geheimfilm,
        &[chunk(60, 100, "Das Geheimnis liegt im Tresor.")],
    )
    .await;
    store_chunks(
        &app,
        kinderfilm,
        &[
            chunk(0, 40, "Guten Morgen."),
            chunk(120, 160, "Der Tresor ist offen!"),
        ],
    )
    .await;

    let r = app
        .get(
            "/api/v1/scenes/search?q=die%20Szene%20mit%20dem%20Tresor",
            Some(&f.admin),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["terms"], json!(["tresor"]));
    assert_eq!(r.json["hits"].as_array().unwrap().len(), 2);

    let r = app
        .get("/api/v1/scenes/search?q=Tresor", Some(&f.kid))
        .await;
    let hits = r.json["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{}", r.json);
    assert_eq!(hits[0]["item"]["title"], "Kinderfilm");
    assert_eq!(hits[0]["start_ms"], 120_000);
    assert_eq!(
        hits[0]["play_url"],
        format!("/play/{kinderfilm}?start=120000")
    );
    assert!(!r.json.to_string().contains(MARKER));
    let r = app
        .get(
            &format!("/api/v1/scenes/search?q=Tresor&item_id={geheimfilm}"),
            Some(&f.kid),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = app
        .get("/api/v1/scenes/search?q=Geheimnis", Some(&f.kid))
        .await;
    assert_eq!(r.json["hits"], json!([]));
    // Settings are for administrators.
    assert_eq!(
        app.get("/api/v1/settings/scenes", Some(&f.kid))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.get("/api/v1/settings/llm", Some(&f.kid)).await.status,
        StatusCode::FORBIDDEN
    );
    let s = app.get("/api/v1/settings/scenes", Some(&f.admin)).await;
    assert_eq!(s.json["chunks"], 3);
    assert_eq!(s.json["indexed_files"], 2);
}
