use flixnet_db::{Db, settings};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

async fn open_temp() -> (TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    (dir, db)
}

#[tokio::test]
async fn open_creates_file_and_applies_migrations() {
    let (dir, db) = open_temp().await;
    assert!(dir.path().join("flixnet.db").exists());

    let (applied,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM _sqlx_migrations WHERE success = 1")
            .fetch_one(db.reader())
            .await
            .unwrap();
    assert!(applied >= 1);

    let (mode,): (String,) = sqlx::query_as("PRAGMA journal_mode")
        .fetch_one(db.writer())
        .await
        .unwrap();
    assert_eq!(mode, "wal");

    let (fk,): (i64,) = sqlx::query_as("PRAGMA foreign_keys")
        .fetch_one(db.writer())
        .await
        .unwrap();
    assert_eq!(fk, 1);
    db.close().await;
}

#[tokio::test]
async fn reopening_is_idempotent_and_keeps_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flixnet.db");

    let db = Db::open(&path).await.unwrap();
    settings::set(&db, "server.name", "Wohnzimmer")
        .await
        .unwrap();
    db.close().await;

    let db = Db::open(&path).await.unwrap();
    let name: Option<String> = settings::get(&db, "server.name").await.unwrap();
    assert_eq!(name.as_deref(), Some("Wohnzimmer"));
    db.close().await;
}

#[tokio::test]
async fn reader_pool_is_read_only() {
    let (_dir, db) = open_temp().await;
    let result =
        sqlx::query("INSERT INTO settings (key, value_json, updated_at) VALUES ('x', '1', 0)")
            .execute(db.reader())
            .await;
    assert!(result.is_err(), "writes through the reader pool must fail");
    db.close().await;
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Example {
    enabled: bool,
    limit: u32,
}

#[tokio::test]
async fn settings_roundtrip_overwrite_and_delete() {
    let (_dir, db) = open_temp().await;

    let missing: Option<Example> = settings::get(&db, "example").await.unwrap();
    assert_eq!(missing, None);

    settings::set(
        &db,
        "example",
        &Example {
            enabled: true,
            limit: 3,
        },
    )
    .await
    .unwrap();
    settings::set(
        &db,
        "example",
        &Example {
            enabled: false,
            limit: 7,
        },
    )
    .await
    .unwrap();
    let value: Option<Example> = settings::get(&db, "example").await.unwrap();
    assert_eq!(
        value,
        Some(Example {
            enabled: false,
            limit: 7
        })
    );

    assert!(settings::delete(&db, "example").await.unwrap());
    assert!(!settings::delete(&db, "example").await.unwrap());
    db.close().await;
}

#[tokio::test]
async fn concurrent_writes_are_serialized_without_lock_errors() {
    let (_dir, db) = open_temp().await;

    let tasks: Vec<_> = (0..50)
        .map(|i| {
            let db = db.clone();
            tokio::spawn(async move { settings::set(&db, &format!("key.{i}"), &i).await })
        })
        .collect();
    for task in tasks {
        task.await.unwrap().unwrap();
    }

    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM settings")
        .fetch_one(db.reader())
        .await
        .unwrap();
    assert_eq!(count, 50);
    db.close().await;
}
