//! Playlists: per user, optionally shared with every profile of the household.
//!
//! Regular playlists hold entries by identity key (like user data), so entries survive
//! rescans and renames; reading resolves them to the items the viewer may see. Smart
//! playlists store a filter (`smart_filter_json`) that is evaluated live.

use sqlx::{QueryBuilder, Sqlite};

use crate::browse::{ItemFilter, push_filters};
use crate::{Db, Result, now_ms};

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Playlist {
    pub id: i64,
    pub user_id: i64,
    pub owner_name: String,
    pub name: String,
    pub shared: bool,
    pub smart_filter_json: Option<String>,
    /// Stored entries (resolvable or not); 0 for smart playlists.
    pub entry_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

macro_rules! playlist_select {
    () => {
        "SELECT p.id, p.user_id, u.name AS owner_name, p.name, p.shared, p.smart_filter_json,
                (SELECT COUNT(*) FROM playlist_items pi WHERE pi.playlist_id = p.id) AS entry_count,
                p.created_at, p.updated_at
         FROM playlists p JOIN users u ON u.id = p.user_id"
    };
}

pub async fn get(db: &Db, id: i64) -> Result<Option<Playlist>> {
    Ok(
        sqlx::query_as(concat!(playlist_select!(), " WHERE p.id = ?"))
            .bind(id)
            .fetch_optional(db.reader())
            .await?,
    )
}

/// The user's own playlists plus those other profiles share, own ones first.
pub async fn list_for_user(db: &Db, user_id: i64) -> Result<Vec<Playlist>> {
    Ok(sqlx::query_as(concat!(
        playlist_select!(),
        " WHERE p.user_id = ?1 OR p.shared = 1
          ORDER BY p.user_id != ?1, p.name COLLATE NOCASE, p.id"
    ))
    .bind(user_id)
    .fetch_all(db.reader())
    .await?)
}

pub async fn create(
    db: &Db,
    user_id: i64,
    name: &str,
    shared: bool,
    smart_filter_json: Option<&str>,
) -> Result<Playlist> {
    let now = now_ms();
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO playlists (user_id, name, shared, smart_filter_json, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(user_id)
    .bind(name)
    .bind(shared)
    .bind(smart_filter_json)
    .bind(now)
    .bind(now)
    .fetch_one(db.writer())
    .await?;
    Ok(get(db, id).await?.expect("just inserted"))
}

/// Changes name, sharing and (for smart playlists) the filter; `None` keeps a value.
pub async fn update(
    db: &Db,
    id: i64,
    name: Option<&str>,
    shared: Option<bool>,
    smart_filter_json: Option<&str>,
) -> Result<bool> {
    let done = sqlx::query(
        "UPDATE playlists SET
            name = COALESCE(?, name),
            shared = COALESCE(?, shared),
            smart_filter_json = COALESCE(?, smart_filter_json),
            updated_at = ?
         WHERE id = ?",
    )
    .bind(name)
    .bind(shared)
    .bind(smart_filter_json)
    .bind(now_ms())
    .bind(id)
    .execute(db.writer())
    .await?;
    Ok(done.rows_affected() > 0)
}

pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    let done = sqlx::query("DELETE FROM playlists WHERE id = ?")
        .bind(id)
        .execute(db.writer())
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Appends entries (identity keys) in the given order; ones already in the playlist
/// stay where they are. Returns how many were added.
pub async fn add_entries(db: &Db, playlist_id: i64, identities: &[String]) -> Result<u64> {
    let now = now_ms();
    let mut tx = db.writer().begin().await?;
    let (mut next,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM playlist_items WHERE playlist_id = ?",
    )
    .bind(playlist_id)
    .fetch_one(&mut *tx)
    .await?;
    let mut added = 0;
    for identity in identities {
        let inserted = sqlx::query(
            "INSERT INTO playlist_items (playlist_id, identity_key, position, added_at)
             VALUES (?, ?, ?, ?) ON CONFLICT (playlist_id, identity_key) DO NOTHING",
        )
        .bind(playlist_id)
        .bind(identity)
        .bind(next)
        .bind(now)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        added += inserted;
        next += inserted as i64;
    }
    sqlx::query("UPDATE playlists SET updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(playlist_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(added)
}

pub async fn remove_entry(db: &Db, playlist_id: i64, entry_id: i64) -> Result<bool> {
    let done = sqlx::query("DELETE FROM playlist_items WHERE playlist_id = ? AND id = ?")
        .bind(playlist_id)
        .bind(entry_id)
        .execute(db.writer())
        .await?;
    Ok(done.rows_affected() > 0)
}

/// All entry ids in playlist order (including entries that currently resolve to nothing).
pub async fn entry_ids(db: &Db, playlist_id: i64) -> Result<Vec<i64>> {
    let rows: Vec<(i64,)> =
        sqlx::query_as("SELECT id FROM playlist_items WHERE playlist_id = ? ORDER BY position, id")
            .bind(playlist_id)
            .fetch_all(db.reader())
            .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Rewrites positions so entries follow `order` (which must list every entry once).
pub async fn set_order(db: &Db, playlist_id: i64, order: &[i64]) -> Result<()> {
    let mut tx = db.writer().begin().await?;
    for (position, entry) in order.iter().enumerate() {
        sqlx::query("UPDATE playlist_items SET position = ? WHERE playlist_id = ? AND id = ?")
            .bind(position as i64)
            .bind(playlist_id)
            .bind(entry)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE playlists SET updated_at = ? WHERE id = ?")
        .bind(now_ms())
        .bind(playlist_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Entries the viewer may see, in playlist order: (entry id, item id). An identity with
/// several copies resolves to one of them.
pub async fn resolve(db: &Db, playlist_id: i64, filter: &ItemFilter) -> Result<Vec<(i64, i64)>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT pi.id, MIN(i.id) FROM playlist_items pi
         JOIN items i ON i.identity_key = pi.identity_key",
    );
    push_filters(&mut qb, filter);
    qb.push(" AND pi.playlist_id = ").push_bind(playlist_id);
    qb.push(" GROUP BY pi.id ORDER BY MIN(pi.position), pi.id");
    Ok(qb.build_query_as().fetch_all(db.reader()).await?)
}

/// Stable identities of items, in the order given: (item id, identity key). Unknown
/// ids are skipped.
pub async fn item_identities(db: &Db, item_ids: &[i64]) -> Result<Vec<(i64, String)>> {
    if item_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT id, COALESCE(identity_key, content_key) FROM items WHERE id IN (",
    );
    {
        let mut ids = qb.separated(", ");
        for id in item_ids {
            ids.push_bind(*id);
        }
    }
    qb.push(")");
    let rows: Vec<(i64, String)> = qb.build_query_as().fetch_all(db.reader()).await?;
    let by_id: std::collections::HashMap<i64, String> = rows.into_iter().collect();
    Ok(item_ids
        .iter()
        .filter_map(|id| by_id.get(id).map(|k| (*id, k.clone())))
        .collect())
}
