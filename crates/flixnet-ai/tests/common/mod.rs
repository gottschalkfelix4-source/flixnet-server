//! Shared helpers: the synthetic fixture catalogue (156 movies and series with
//! self-written German overviews, TMDB-style English keywords) loaded into a temp DB.
#![allow(dead_code)] // each test binary uses a different subset

use std::collections::HashMap;
use std::path::PathBuf;

use flixnet_ai::Ai;
use flixnet_core::config::{AiConfig, EmbedderOverride};
use flixnet_db::Db;
use flixnet_db::metadata::CreditInput;
use serde::Deserialize;
use tempfile::TempDir;

#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    pub k: String,
    pub t: String,
    #[serde(default)]
    pub o: Option<String>,
    pub y: i64,
    pub g: Vec<String>,
    pub r: f64,
    pub a: i64,
    pub d: String,
    pub kw: Vec<String>,
    pub dir: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GoldenQuery {
    pub lang: String,
    pub q: String,
    pub rel: Vec<String>,
}

pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

pub fn catalogue() -> Vec<Entry> {
    serde_json::from_str(&std::fs::read_to_string(fixture("catalogue.json")).unwrap()).unwrap()
}

pub fn golden() -> Vec<GoldenQuery> {
    serde_json::from_str(&std::fs::read_to_string(fixture("golden.json")).unwrap()).unwrap()
}

pub struct Catalogue {
    pub dir: TempDir,
    pub db: Db,
    pub library_id: i64,
    /// Title → item id.
    pub ids: HashMap<String, i64>,
    pub entries: Vec<Entry>,
}

/// A temp database holding the fixture catalogue in one library.
pub async fn load_catalogue() -> Catalogue {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    let (library_id,): (i64,) = sqlx::query_as(
        "INSERT INTO libraries (name, kind, created_at, updated_at) VALUES ('Katalog', 'movies', 0, 0) RETURNING id",
    )
    .fetch_one(db.writer())
    .await
    .unwrap();
    let entries = catalogue();
    let mut ids = HashMap::new();
    for (n, e) in entries.iter().enumerate() {
        let key = format!("fixture:{}:{n}", e.k);
        let (id,): (i64,) = sqlx::query_as(
            "INSERT INTO items (library_id, kind, title, sort_title, original_title, year, overview,
                                community_rating, age_rating, content_key, identity_key, match_state,
                                date_added, date_modified)
             VALUES (?, ?, ?, lower(?), ?, ?, ?, ?, ?, ?, ?, 'matched', ?, ?) RETURNING id",
        )
        .bind(library_id)
        .bind(&e.k)
        .bind(&e.t)
        .bind(&e.t)
        .bind(&e.o)
        .bind(e.y)
        .bind(&e.d)
        .bind(e.r)
        .bind(e.a)
        .bind(&key)
        .bind(&key)
        .bind(n as i64)
        .bind(n as i64)
        .fetch_one(db.writer())
        .await
        .unwrap();
        flixnet_db::metadata::set_genres(&db, id, &e.g)
            .await
            .unwrap();
        flixnet_db::ai::set_keywords(&db, id, &e.kw).await.unwrap();
        flixnet_db::metadata::set_credits(
            &db,
            id,
            &[CreditInput {
                name: e.dir.clone(),
                role: "director".into(),
                ..Default::default()
            }],
        )
        .await
        .unwrap();
        ids.insert(e.t.clone(), id);
    }
    Catalogue {
        dir,
        db,
        library_id,
        ids,
        entries,
    }
}

/// The AI service with the deterministic stub embedder.
pub fn stub_ai(cat: &Catalogue) -> Ai {
    Ai::with_models_dir(
        cat.db.clone(),
        AiConfig {
            embedder: EmbedderOverride::Stub,
            onnxruntime_path: None,
        },
        cat.dir.path().join("models"),
    )
}

/// The AI service with the real local model (downloaded into `FLIXNET_AI_MODELS`,
/// default `target/ai-models`, on first use).
pub async fn e5_ai(cat: &Catalogue) -> Ai {
    let models = std::env::var_os("FLIXNET_AI_MODELS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/ai-models")
        });
    let ai = Ai::with_models_dir(cat.db.clone(), AiConfig::default(), models);
    for artifact in ai.models().missing() {
        println!("downloading {} …", artifact.label);
        ai.models()
            .install(
                &flixnet_ai::models::download_client(),
                &artifact,
                |_, _| {},
                || false,
            )
            .await
            .unwrap();
    }
    ai
}

/// A user (household profile) row.
pub async fn user(db: &Db, name: &str) -> i64 {
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO users (name, is_admin, created_at, updated_at) VALUES (?, 0, 0, 0) RETURNING id",
    )
    .bind(name)
    .fetch_one(db.writer())
    .await
    .unwrap();
    id
}

/// Marks an item as watched by a user at a time.
pub async fn watched(db: &Db, user_id: i64, item_id: i64, at: i64) {
    sqlx::query(
        "INSERT INTO user_item_data (user_id, identity_key, played, play_count, last_played_at, updated_at)
         SELECT ?, identity_key, 1, 1, ?, ? FROM items WHERE id = ?
         ON CONFLICT (user_id, identity_key) DO UPDATE SET played = 1, last_played_at = excluded.last_played_at",
    )
    .bind(user_id)
    .bind(at)
    .bind(at)
    .bind(item_id)
    .execute(db.writer())
    .await
    .unwrap();
}
