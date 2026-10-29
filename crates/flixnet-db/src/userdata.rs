//! Per-user state of items (watched, progress, favorites), keyed by stable identity.

use crate::{Db, Result, now_ms};

/// Recomputes an item's identity from its provider ids (and those of its series).
/// Call after provider ids change; series updates cascade to seasons and episodes.
pub async fn update_identity(db: &Db, item_id: i64) -> Result<()> {
    #[derive(sqlx::FromRow)]
    struct Row {
        kind: String,
        content_key: String,
        series_id: Option<i64>,
        index_number: Option<i64>,
        parent_index_number: Option<i64>,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT kind, content_key, series_id, index_number, parent_index_number
         FROM items WHERE id = ?",
    )
    .bind(item_id)
    .fetch_optional(db.reader())
    .await?;
    let Some(Row {
        kind,
        content_key,
        series_id,
        index_number: index,
        parent_index_number: parent_index,
    }) = row
    else {
        return Ok(());
    };

    let identity = match kind.as_str() {
        "movie" => provider_identity(db, item_id, &[("tmdb", "tmdb:movie:"), ("imdb", "imdb:")])
            .await?
            .unwrap_or(content_key),
        "series" => series_identity(db, item_id).await?.unwrap_or(content_key),
        "season" => match (series_id, index) {
            (Some(series), Some(n)) => match series_identity(db, series).await? {
                Some(base) => format!("{base}:s{n}"),
                None => content_key,
            },
            _ => content_key,
        },
        _ => match (series_id, parent_index, index) {
            (Some(series), Some(season), Some(episode)) => match series_identity(db, series).await?
            {
                Some(base) => format!("{base}:s{season}e{episode}"),
                None => content_key,
            },
            _ => content_key,
        },
    };
    let (old,): (Option<String>,) = sqlx::query_as("SELECT identity_key FROM items WHERE id = ?")
        .bind(item_id)
        .fetch_one(db.reader())
        .await?;
    if old.as_deref() != Some(identity.as_str()) {
        let mut tx = db.writer().begin().await?;
        sqlx::query("UPDATE items SET identity_key = ? WHERE id = ?")
            .bind(&identity)
            .bind(item_id)
            .execute(&mut *tx)
            .await?;
        // Carry existing user data over to the new identity (unless the user already
        // has data under it, e.g. from another copy of the same movie).
        if let Some(old) = old {
            sqlx::query(
                "UPDATE OR IGNORE user_item_data SET identity_key = ? WHERE identity_key = ?",
            )
            .bind(&identity)
            .bind(&old)
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM user_item_data WHERE identity_key = ?")
                .bind(&old)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
    }

    if kind == "series" {
        let children: Vec<(i64,)> =
            sqlx::query_as("SELECT id FROM items WHERE series_id = ? AND id != ?")
                .bind(item_id)
                .bind(item_id)
                .fetch_all(db.reader())
                .await?;
        for (child,) in children {
            Box::pin(update_identity(db, child)).await?;
        }
    }
    Ok(())
}

async fn series_identity(db: &Db, series_id: i64) -> Result<Option<String>> {
    provider_identity(
        db,
        series_id,
        &[("tmdb", "tmdb:tv:"), ("tvdb", "tvdb:"), ("imdb", "imdb:")],
    )
    .await
}

async fn provider_identity(
    db: &Db,
    item_id: i64,
    order: &[(&str, &str)],
) -> Result<Option<String>> {
    let ids: Vec<(String, String)> =
        sqlx::query_as("SELECT provider, value FROM provider_ids WHERE item_id = ?")
            .bind(item_id)
            .fetch_all(db.reader())
            .await?;
    Ok(order.iter().find_map(|(provider, prefix)| {
        ids.iter()
            .find(|(p, _)| p == provider)
            .map(|(_, value)| format!("{prefix}{value}"))
    }))
}

pub async fn identity_key(db: &Db, item_id: i64) -> Result<Option<String>> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT identity_key FROM items WHERE id = ?")
            .bind(item_id)
            .fetch_optional(db.reader())
            .await?;
    Ok(row.and_then(|(k,)| k))
}

#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct UserData {
    pub played: bool,
    pub play_count: i64,
    pub position_ms: i64,
    pub last_played_at: Option<i64>,
    pub favorite: bool,
    pub rating: Option<f64>,
}

pub async fn get(db: &Db, user_id: i64, item_id: i64) -> Result<UserData> {
    let row = sqlx::query_as(
        "SELECT ud.played, ud.play_count, ud.position_ms, ud.last_played_at, ud.favorite, ud.rating
         FROM user_item_data ud JOIN items i ON i.identity_key = ud.identity_key
         WHERE ud.user_id = ? AND i.id = ?",
    )
    .bind(user_id)
    .bind(item_id)
    .fetch_optional(db.reader())
    .await?;
    Ok(row.unwrap_or_default())
}

async fn ensure_row(db: &Db, user_id: i64, item_id: i64) -> Result<Option<String>> {
    let identity: Option<(String,)> =
        sqlx::query_as("SELECT COALESCE(identity_key, content_key) FROM items WHERE id = ?")
            .bind(item_id)
            .fetch_optional(db.reader())
            .await?;
    let Some((identity,)) = identity else {
        return Ok(None);
    };
    sqlx::query(
        "INSERT INTO user_item_data (user_id, identity_key, updated_at) VALUES (?, ?, ?)
         ON CONFLICT (user_id, identity_key) DO NOTHING",
    )
    .bind(user_id)
    .bind(&identity)
    .bind(now_ms())
    .execute(db.writer())
    .await?;
    Ok(Some(identity))
}

/// Marks an item (and, for series/seasons, all their episodes) watched or unwatched.
pub async fn set_played(db: &Db, user_id: i64, item_id: i64, played: bool) -> Result<bool> {
    let targets: Vec<(i64,)> = sqlx::query_as(
        "SELECT id FROM items WHERE id = ?1
         UNION SELECT id FROM items WHERE kind = 'episode' AND (series_id = ?1 OR parent_id = ?1)",
    )
    .bind(item_id)
    .fetch_all(db.reader())
    .await?;
    if targets.is_empty() {
        return Ok(false);
    }
    let now = now_ms();
    for (id,) in targets {
        let Some(identity) = ensure_row(db, user_id, id).await? else {
            continue;
        };
        sqlx::query(
            "UPDATE user_item_data SET
                played = ?, position_ms = 0,
                play_count = CASE WHEN ? AND played = 0 THEN play_count + 1 ELSE play_count END,
                last_played_at = CASE WHEN ? THEN ? ELSE last_played_at END,
                updated_at = ?
             WHERE user_id = ? AND identity_key = ?",
        )
        .bind(played)
        .bind(played)
        .bind(played)
        .bind(now)
        .bind(now)
        .bind(user_id)
        .bind(&identity)
        .execute(db.writer())
        .await?;
    }
    Ok(true)
}

pub async fn set_favorite(db: &Db, user_id: i64, item_id: i64, favorite: bool) -> Result<bool> {
    let Some(identity) = ensure_row(db, user_id, item_id).await? else {
        return Ok(false);
    };
    sqlx::query("UPDATE user_item_data SET favorite = ?, updated_at = ? WHERE user_id = ? AND identity_key = ?")
        .bind(favorite)
        .bind(now_ms())
        .bind(user_id)
        .bind(&identity)
        .execute(db.writer())
        .await?;
    Ok(true)
}
