//! AI routes with the deterministic stub embedder: hybrid search, recommendations,
//! onboarding, "Mehr wie das", group mode and the admin settings — including parental
//! controls everywhere.

mod common;

use axum::http::{Method, StatusCode};
use flixnet_core::config::EmbedderOverride;
use flixnet_db::Db;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{PASSWORD, TestApp, app, app_with, credentials};

struct Item {
    title: &'static str,
    kind: &'static str,
    age: i64,
    genres: &'static [&'static str],
    overview: &'static str,
}

const ITEMS: &[Item] = &[
    Item {
        title: "Kreidezeit-Expedition",
        kind: "movie",
        age: 12,
        genres: &["Abenteuer", "Science Fiction"],
        overview: "Ein Forscherteam reist mit einer Zeitmaschine in die Urzeit und begegnet einem T-Rex.",
    },
    Item {
        title: "Dino-Camp",
        kind: "series",
        age: 6,
        genres: &["Animation", "Familie", "Abenteuer"],
        overview: "Kinder entdecken in einem Ferienlager auf einer Insel echte Dinosaurier.",
    },
    Item {
        title: "Saurier-Insel",
        kind: "movie",
        age: 16,
        genres: &["Abenteuer", "Horror"],
        overview: "Auf einer Insel brechen genetisch erzeugte Saurier aus ihren Gehegen aus.",
    },
    Item {
        title: "Liebe in Paris",
        kind: "movie",
        age: 0,
        genres: &["Liebesfilm"],
        overview: "Zwei Fremde verlieben sich an einem Sommerabend in Paris.",
    },
    Item {
        title: "Hochzeit mit Hindernissen",
        kind: "movie",
        age: 0,
        genres: &["Komödie", "Liebesfilm"],
        overview: "Eine Hochzeit auf dem Land gerät zur turbulenten Komödie.",
    },
    Item {
        title: "Der Kommissar",
        kind: "series",
        age: 12,
        genres: &["Krimi"],
        overview: "Ein Kommissar ermittelt in Mordfällen in München.",
    },
    Item {
        title: "Weltraumstation",
        kind: "movie",
        age: 12,
        genres: &["Science Fiction"],
        overview: "Astronauten kämpfen auf einer beschädigten Raumstation im Orbit ums Überleben.",
    },
    Item {
        title: "Kleiner Drache",
        kind: "movie",
        age: 0,
        genres: &["Animation", "Familie"],
        overview: "Ein kleiner Drache sucht im Zauberwald nach seiner Familie.",
    },
];

/// One library with the items above; returns (library id, ids in `ITEMS` order).
async fn catalogue(db: &Db) -> (i64, Vec<i64>) {
    let dir = tempfile::tempdir().unwrap();
    let library = flixnet_db::libraries::create(
        db,
        "Filme",
        "movies",
        &[dir.path().to_string_lossy().into_owned()],
    )
    .await
    .unwrap();
    let mut ids = Vec::new();
    for (n, item) in ITEMS.iter().enumerate() {
        let key = format!("test:{n}");
        let (id,): (i64,) = sqlx::query_as(
            "INSERT INTO items (library_id, kind, title, sort_title, overview, age_rating, community_rating,
                                content_key, identity_key, match_state, date_added, date_modified)
             VALUES (?, ?, ?, lower(?), ?, ?, 7.0, ?, ?, 'matched', 0, 0) RETURNING id",
        )
        .bind(library.id)
        .bind(item.kind)
        .bind(item.title)
        .bind(item.title)
        .bind(item.overview)
        .bind(item.age)
        .bind(&key)
        .bind(&key)
        .fetch_one(db.writer())
        .await
        .unwrap();
        let genres: Vec<String> = item.genres.iter().map(|g| (*g).to_owned()).collect();
        flixnet_db::metadata::set_genres(db, id, &genres)
            .await
            .unwrap();
        ids.push(id);
    }
    (library.id, ids)
}

async fn stub_app() -> TestApp {
    app_with(|config| config.ai.embedder = EmbedderOverride::Stub).await
}

/// Computes the vectors with the app's own AI service (the job's work).
async fn embed(app: &TestApp) {
    let ai = &app.state.ai;
    let embedder = ai.embedder().await.unwrap().unwrap();
    flixnet_ai::jobs::embed_all(ai, &embedder, false, &|| false, |_| async {})
        .await
        .unwrap();
}

async fn profile(app: &TestApp, admin: &str, body: Value) -> (i64, String) {
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

fn titles(items: &Value, path: &[&str]) -> Vec<String> {
    items
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            let mut v = v;
            for key in path {
                v = &v[*key];
            }
            v["title"].as_str().unwrap().to_owned()
        })
        .collect()
}

#[tokio::test]
async fn semantic_search_finds_what_full_text_misses() {
    let app = stub_app().await;
    let admin = app.setup_admin().await;
    catalogue(&app.db).await;
    let query = "/api/v1/items?search=Film%20%C3%BCber%20Zeitreisen%20mit%20Dinosauriern";

    // Before anything is embedded: full text only, which finds nothing.
    let before = app.get(query, Some(&admin)).await;
    assert_eq!(before.status, StatusCode::OK);
    assert_eq!(before.json["total"], 0, "{}", before.json);

    embed(&app).await;
    let after = app.get(query, Some(&admin)).await;
    let found = titles(&after.json["items"], &[]);
    assert_eq!(found[0], "Kreidezeit-Expedition", "{found:?}");
    assert!(!found.contains(&"Liebe in Paris".to_owned()), "{found:?}");
    // Plain full text (sorted by title instead of relevance) still finds nothing.
    let fts = app.get(&format!("{query}&sort=title"), Some(&admin)).await;
    assert_eq!(fts.json["total"], 0);
    // Exact titles keep winning.
    let exact = app
        .get("/api/v1/items?search=Kommissar", Some(&admin))
        .await;
    assert_eq!(titles(&exact.json["items"], &[])[0], "Der Kommissar");
}

#[tokio::test]
async fn recommendations_follow_watching_and_onboarding() {
    let app = stub_app().await;
    let admin = app.setup_admin().await;
    let (_, ids) = catalogue(&app.db).await;

    // Nothing embedded yet: not available.
    let off = app.get("/api/v1/recommendations", Some(&admin)).await;
    assert_eq!(off.status, StatusCode::OK);
    assert_eq!(off.json["available"], false);
    embed(&app).await;

    // No history: onboarding first.
    let fresh = app.get("/api/v1/recommendations", Some(&admin)).await;
    assert_eq!(
        (
            fresh.json["available"].clone(),
            fresh.json["needs_onboarding"].clone()
        ),
        (json!(true), json!(true))
    );
    let onboarding = app
        .get("/api/v1/recommendations/onboarding", Some(&admin))
        .await;
    assert_eq!(
        onboarding.json["items"].as_array().unwrap().len(),
        ITEMS.len()
    );
    let liked = ids[3]; // Liebe in Paris
    let saved = app
        .request(
            Method::PUT,
            "/api/v1/recommendations/onboarding",
            Some(json!({ "item_ids": [liked] })),
            Some(&admin),
        )
        .await;
    assert_eq!(saved.status, StatusCode::NO_CONTENT, "{}", saved.json);
    let seeded = app.get("/api/v1/recommendations", Some(&admin)).await;
    assert_eq!(seeded.json["needs_onboarding"], false);
    assert_eq!(
        seeded.json["items"][0]["item"]["title"], "Hochzeit mit Hindernissen",
        "{}",
        seeded.json
    );
    assert_eq!(
        seeded.json["items"][0]["reason"],
        "Weil du „Liebe in Paris“ magst"
    );
    let picked = app
        .get("/api/v1/recommendations/onboarding", Some(&admin))
        .await;
    assert_eq!(picked.json["selected"], json!([liked]));

    // Watching a dinosaur film: dinosaurs come first, explained by that film.
    let watched = app
        .request(
            Method::PUT,
            &format!("/api/v1/items/{}/played", ids[0]),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(watched.status, StatusCode::NO_CONTENT);
    let recs = app
        .get("/api/v1/recommendations?limit=4", Some(&admin))
        .await;
    let items = recs.json["items"].as_array().unwrap();
    assert_eq!(items.len(), 4);
    // The onboarding pick still counts, so its neighbour may lead; the dinosaurs follow.
    let top: Vec<&str> = items[..3]
        .iter()
        .map(|i| i["item"]["title"].as_str().unwrap())
        .collect();
    assert!(
        top.contains(&"Dino-Camp") && top.contains(&"Saurier-Insel"),
        "{}",
        recs.json
    );
    let dino = items
        .iter()
        .find(|i| i["item"]["title"] == "Dino-Camp")
        .unwrap();
    assert_eq!(
        dino["reason"],
        "Weil du „Kreidezeit-Expedition“ gesehen hast"
    );
    assert_eq!(dino["because_item_id"], ids[0]);
    assert!(
        items
            .iter()
            .all(|i| i["item"]["id"] != ids[0] && i["item"]["id"] != liked)
    );

    // Home rows: "Für dich" plus "Weil du … gesehen hast".
    let rows = app.get("/api/v1/recommendations/rows", Some(&admin)).await;
    let rows = rows.json.as_array().unwrap();
    assert_eq!(rows[0]["title"], "Für dich");
    assert!(
        rows.iter().any(
            |r| r["title"] == "Weil du „Kreidezeit-Expedition“ gesehen hast"
                && r["kind"] == "because"
        ),
        "{rows:?}"
    );

    // "Mehr wie das".
    let similar = app
        .get(
            &format!("/api/v1/items/{}/similar?limit=2", ids[1]),
            Some(&admin),
        )
        .await;
    let similar = titles(&similar.json, &[]);
    assert!(
        similar.contains(&"Kreidezeit-Expedition".to_owned())
            || similar.contains(&"Saurier-Insel".to_owned())
    );
    assert!(!similar.contains(&"Dino-Camp".to_owned()));
}

#[tokio::test]
async fn restricted_profiles_and_groups_never_get_what_they_must_not_see() {
    let app = stub_app().await;
    let admin = app.setup_admin().await;
    let (_, ids) = catalogue(&app.db).await;
    embed(&app).await;
    let (kid_id, kid) = profile(
        &app,
        &admin,
        json!({ "name": "Kind", "password": PASSWORD, "max_age_rating": 6 }),
    )
    .await;
    let (hidden_id, _) = profile(
        &app,
        &admin,
        json!({ "name": "Versteckt", "password": PASSWORD, "is_hidden": true }),
    )
    .await;
    let allowed = |title: &str| ITEMS.iter().any(|i| i.title == title && i.age <= 6);

    // The child watched the dinosaur camp; the restricted dinosaur films stay away.
    let played = app
        .request(
            Method::PUT,
            &format!("/api/v1/items/{}/played", ids[1]),
            None,
            Some(&kid),
        )
        .await;
    assert_eq!(played.status, StatusCode::NO_CONTENT);
    let recs = app.get("/api/v1/recommendations", Some(&kid)).await;
    let found = titles(&recs.json["items"], &["item"]);
    assert!(!found.is_empty());
    assert!(found.iter().all(|t| allowed(t)), "{found:?}");
    let rows = app.get("/api/v1/recommendations/rows", Some(&kid)).await;
    for row in rows.json.as_array().unwrap() {
        assert!(
            titles(&row["items"], &[]).iter().all(|t| allowed(t)),
            "{row}"
        );
    }
    let onboarding = app
        .get("/api/v1/recommendations/onboarding", Some(&kid))
        .await;
    assert!(
        titles(&onboarding.json["items"], &[])
            .iter()
            .all(|t| allowed(t))
    );
    let similar = app
        .get(&format!("/api/v1/items/{}/similar", ids[1]), Some(&kid))
        .await;
    assert!(
        titles(&similar.json, &[]).iter().all(|t| allowed(t)),
        "{}",
        similar.json
    );
    // Restricted items: 404 like everywhere else.
    let hidden = app
        .get(&format!("/api/v1/items/{}/similar", ids[2]), Some(&kid))
        .await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    let sneaky = app
        .request(
            Method::PUT,
            "/api/v1/recommendations/onboarding",
            Some(json!({ "item_ids": [ids[2]] })),
            Some(&kid),
        )
        .await;
    assert_eq!(sneaky.status, StatusCode::NOT_FOUND);
    let search = app
        .get(
            "/api/v1/items?search=Saurier%20Insel%20Dinosaurier",
            Some(&kid),
        )
        .await;
    assert!(
        titles(&search.json["items"], &[])
            .iter()
            .all(|t| allowed(t)),
        "{}",
        search.json
    );

    // Group mode: the strictest member decides, and what anyone saw is left out.
    app.request(
        Method::PUT,
        &format!("/api/v1/items/{}/played", ids[0]),
        None,
        Some(&admin),
    )
    .await;
    for (caller, other) in [(&admin, kid_id), (&kid, 1)] {
        let group = app
            .post(
                "/api/v1/recommendations/group",
                json!({ "user_ids": [other] }),
                Some(caller),
            )
            .await;
        assert_eq!(group.status, StatusCode::OK, "{}", group.json);
        assert_eq!(group.json["max_age_rating"], 6);
        assert_eq!(group.json["members"].as_array().unwrap().len(), 2);
        let found = titles(&group.json["items"], &["item"]);
        assert!(!found.is_empty());
        assert!(found.iter().all(|t| allowed(t)), "{found:?}");
        assert!(
            !found.contains(&"Dino-Camp".to_owned()),
            "seen by the child"
        );
        assert!(
            group.json["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|i| i["reason"].is_string())
        );
    }
    let with_seen = app
        .post(
            "/api/v1/recommendations/group",
            json!({ "user_ids": [kid_id], "exclude_watched": false }),
            Some(&admin),
        )
        .await;
    assert!(titles(&with_seen.json["items"], &["item"]).contains(&"Dino-Camp".to_owned()));
    // Hidden or unknown profiles cannot be added.
    for id in [hidden_id, 999] {
        let bad = app
            .post(
                "/api/v1/recommendations/group",
                json!({ "user_ids": [id] }),
                Some(&kid),
            )
            .await;
        assert_eq!(bad.status, StatusCode::NOT_FOUND, "{id}");
    }
}

#[tokio::test]
async fn ai_settings_are_for_administrators_and_endpoints_are_tested() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let (_, kid) = profile(
        &app,
        &admin,
        json!({ "name": "Kind", "password": PASSWORD }),
    )
    .await;
    for (method, uri) in [
        (Method::GET, "/api/v1/settings/ai"),
        (Method::PUT, "/api/v1/settings/ai"),
        (Method::GET, "/api/v1/ai/status"),
        (Method::POST, "/api/v1/ai/model"),
        (Method::DELETE, "/api/v1/ai/model"),
        (Method::POST, "/api/v1/ai/embeddings"),
    ] {
        let response = app
            .request(method.clone(), uri, Some(json!({})), Some(&kid))
            .await;
        assert_eq!(response.status, StatusCode::FORBIDDEN, "{method} {uri}");
    }

    let settings = app.get("/api/v1/settings/ai", Some(&admin)).await;
    assert_eq!(settings.json["embedder"], "local");
    assert_eq!(settings.json["auto_download"], true);
    let status = app.get("/api/v1/ai/status", Some(&admin)).await;
    assert_eq!(status.json["embedder"], "local");
    assert_eq!(status.json["ready"], false, "no model downloaded in tests");
    assert!(!status.json["missing"].as_array().unwrap().is_empty());

    // An OpenAI-compatible endpoint is tested before it is saved.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(header("authorization", "Bearer geheim"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "index": 0, "embedding": [0.1, 0.2, 0.3] }]
        })))
        .mount(&server)
        .await;
    let bad_url = app
        .request(
            Method::PUT,
            "/api/v1/settings/ai",
            Some(
                json!({ "embedder": "openai", "openai_base_url": "ftp://x", "openai_model": "m" }),
            ),
            Some(&admin),
        )
        .await;
    assert_eq!(bad_url.status, StatusCode::UNPROCESSABLE_ENTITY);
    let wrong_key = app
        .request(
            Method::PUT,
            "/api/v1/settings/ai",
            Some(json!({ "embedder": "openai", "openai_base_url": format!("{}/v1", server.uri()), "openai_model": "test-embed", "openai_api_key": "falsch" })),
            Some(&admin),
        )
        .await;
    assert_eq!(
        wrong_key.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        wrong_key.json
    );
    let saved = app
        .request(
            Method::PUT,
            "/api/v1/settings/ai",
            Some(json!({ "embedder": "openai", "openai_base_url": format!("{}/v1/", server.uri()), "openai_model": "test-embed", "openai_api_key": "geheim" })),
            Some(&admin),
        )
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.json);
    assert_eq!(saved.json["has_openai_api_key"], true);
    assert_eq!(
        saved.json["openai_base_url"],
        format!("{}/v1", server.uri())
    );
    assert!(
        saved.json.get("openai_api_key").is_none(),
        "keys are never returned"
    );

    // The endpoint embeds; vectors are stored under its model key.
    catalogue(&app.db).await;
    let ai = &app.state.ai;
    let embedder = ai.embedder().await.unwrap().unwrap();
    assert_eq!(embedder.model_key(), "openai:test-embed");
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let n = body["input"].as_array().unwrap().len();
            let data: Vec<Value> = (0..n)
                .map(|i| json!({ "index": i, "embedding": [1.0, i as f64, 0.5] }))
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({ "data": data }))
        })
        .with_priority(1)
        .mount(&server)
        .await;
    let report = flixnet_ai::jobs::embed_all(ai, &embedder, false, &|| false, |_| async {})
        .await
        .unwrap();
    assert_eq!(report.embedded, ITEMS.len());
    let status = app.get("/api/v1/ai/status", Some(&admin)).await;
    assert_eq!(
        status.json["embedded"],
        ITEMS.len() as i64,
        "{}",
        status.json
    );
    assert_eq!(status.json["model_key"], "openai:test-embed");

    let queued = app
        .request(
            Method::POST,
            "/api/v1/ai/embeddings",
            Some(json!({ "rebuild": true })),
            Some(&admin),
        )
        .await;
    assert_eq!(queued.status, StatusCode::ACCEPTED);
    let off = app
        .request(
            Method::PUT,
            "/api/v1/settings/ai",
            Some(json!({ "embedder": "off" })),
            Some(&admin),
        )
        .await;
    assert_eq!(off.json["embedder"], "off");
    let recs = app.get("/api/v1/recommendations", Some(&admin)).await;
    assert_eq!(recs.json["available"], false);
}
