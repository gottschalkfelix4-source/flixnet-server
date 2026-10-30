//! Storage for the local AI: embedding models, item vectors, onboarding picks,
//! keywords and the viewing signals recommendations are built from.

use sqlx::{QueryBuilder, Sqlite};

use crate::{Db, Result, now_ms};

/// Id of an embedding model, registered on first use.
pub async fn model_id(db: &Db, model_key: &str, dimensions: i64) -> Result<i64> {
    if let Some((id,)) =
        sqlx::query_as::<_, (i64,)>("SELECT id FROM embedding_models WHERE model_key = ?")
            .bind(model_key)
            .fetch_optional(db.reader())
            .await?
    {
        return Ok(id);
    }
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO embedding_models (model_key, dimensions, created_at) VALUES (?, ?, ?)
         ON CONFLICT (model_key) DO UPDATE SET dimensions = excluded.dimensions
         RETURNING id",
    )
    .bind(model_key)
    .bind(dimensions)
    .bind(now_ms())
    .fetch_one(db.writer())
    .await?;
    Ok(id)
}

/// A registered model: (id, dimensions), without registering it.
pub async fn find_model(db: &Db, model_key: &str) -> Result<Option<(i64, i64)>> {
    Ok(
        sqlx::query_as("SELECT id, dimensions FROM embedding_models WHERE model_key = ?")
            .bind(model_key)
            .fetch_optional(db.reader())
            .await?,
    )
}

/// Everything about an item that goes into its embedding text.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct EmbeddingSource {
    pub id: i64,
    pub kind: String,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<i64>,
    pub overview: Option<String>,
    pub tagline: Option<String>,
    /// `|`-separated.
    pub genres: Option<String>,
    /// `|`-separated, in TMDB order.
    pub keywords: Option<String>,
    /// `|`-separated, top-billed first.
    pub cast: Option<String>,
    /// Directors and creators, `|`-separated.
    pub directors: Option<String>,
}

/// Movies and series with `id > after_id`, by id (one page of the embedding sweep).
pub async fn embedding_sources(db: &Db, after_id: i64, limit: i64) -> Result<Vec<EmbeddingSource>> {
    Ok(sqlx::query_as(
        "SELECT i.id, i.kind, i.title, i.original_title, i.year, i.overview, i.tagline,
                (SELECT group_concat(g.name, '|') FROM item_genres ig JOIN genres g ON g.id = ig.genre_id
                   WHERE ig.item_id = i.id) AS genres,
                (SELECT group_concat(k.keyword, '|') FROM
                   (SELECT keyword FROM item_keywords WHERE item_id = i.id ORDER BY ord LIMIT 20) k) AS keywords,
                (SELECT group_concat(c.name, '|') FROM
                   (SELECT p.name FROM credits cr JOIN people p ON p.id = cr.person_id
                    WHERE cr.item_id = i.id AND cr.role = 'actor' ORDER BY cr.ord LIMIT 6) c) AS \"cast\",
                (SELECT group_concat(d.name, '|') FROM
                   (SELECT p.name FROM credits cr JOIN people p ON p.id = cr.person_id
                    WHERE cr.item_id = i.id AND cr.role IN ('director', 'creator') ORDER BY cr.ord LIMIT 3) d) AS directors
         FROM items i
         WHERE i.kind IN ('movie', 'series') AND i.id > ?
         ORDER BY i.id LIMIT ?",
    )
    .bind(after_id)
    .bind(limit)
    .fetch_all(db.reader())
    .await?)
}

/// Stored text hashes of a model for the given items.
pub async fn text_hashes(db: &Db, model_id: i64, item_ids: &[i64]) -> Result<Vec<(i64, String)>> {
    if item_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut qb: QueryBuilder<Sqlite> =
        QueryBuilder::new("SELECT item_id, text_hash FROM item_embeddings WHERE model_id = ");
    qb.push_bind(model_id).push(" AND item_id IN (");
    {
        let mut ids = qb.separated(", ");
        for id in item_ids {
            ids.push_bind(*id);
        }
    }
    qb.push(")");
    Ok(qb.build_query_as().fetch_all(db.reader()).await?)
}

/// One computed vector, ready to store.
#[derive(Debug, Clone)]
pub struct NewEmbedding {
    pub item_id: i64,
    pub text_hash: String,
    /// Little-endian f32 values.
    pub vector: Vec<u8>,
}

/// Stores a batch of vectors in one short transaction (computed beforehand).
pub async fn upsert_embeddings(db: &Db, model_id: i64, rows: &[NewEmbedding]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let now = now_ms();
    let mut tx = db.writer().begin().await?;
    for row in rows {
        // The item may have been removed while its vector was computed.
        sqlx::query(
            "INSERT INTO item_embeddings (model_id, item_id, text_hash, vector, updated_at)
             SELECT ?, ?, ?, ?, ? WHERE EXISTS (SELECT 1 FROM items WHERE id = ?)
             ON CONFLICT (model_id, item_id) DO UPDATE SET
                 text_hash = excluded.text_hash, vector = excluded.vector,
                 updated_at = excluded.updated_at",
        )
        .bind(model_id)
        .bind(row.item_id)
        .bind(&row.text_hash)
        .bind(&row.vector)
        .bind(now)
        .bind(row.item_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Drops the vectors of every model except `keep` (after switching models).
pub async fn delete_other_models(db: &Db, keep: i64) -> Result<u64> {
    let result = sqlx::query("DELETE FROM item_embeddings WHERE model_id != ?")
        .bind(keep)
        .execute(db.writer())
        .await?;
    Ok(result.rows_affected())
}

/// Drops all vectors of a model (rebuild from scratch).
pub async fn delete_model_vectors(db: &Db, model_id: i64) -> Result<u64> {
    let result = sqlx::query("DELETE FROM item_embeddings WHERE model_id = ?")
        .bind(model_id)
        .execute(db.writer())
        .await?;
    Ok(result.rows_affected())
}

/// All vectors of a model: (item id, little-endian f32 bytes).
pub async fn vectors(db: &Db, model_id: i64) -> Result<Vec<(i64, Vec<u8>)>> {
    Ok(sqlx::query_as(
        "SELECT item_id, vector FROM item_embeddings WHERE model_id = ? ORDER BY item_id",
    )
    .bind(model_id)
    .fetch_all(db.reader())
    .await?)
}

/// (items with a vector of this model, movies and series in total).
pub async fn embedding_counts(db: &Db, model_id: Option<i64>) -> Result<(i64, i64)> {
    let (embedded, total): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM item_embeddings e JOIN items i ON i.id = e.item_id
                 WHERE e.model_id = ?),
                (SELECT COUNT(*) FROM items WHERE kind IN ('movie', 'series'))",
    )
    .bind(model_id.unwrap_or(-1))
    .fetch_one(db.reader())
    .await?;
    Ok((embedded, total))
}

/// Titles of items (for explanations).
pub async fn titles(db: &Db, item_ids: &[i64]) -> Result<Vec<(i64, String)>> {
    if item_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut qb: QueryBuilder<Sqlite> =
        QueryBuilder::new("SELECT id, title FROM items WHERE id IN (");
    {
        let mut ids = qb.separated(", ");
        for id in item_ids {
            ids.push_bind(*id);
        }
    }
    qb.push(")");
    Ok(qb.build_query_as().fetch_all(db.reader()).await?)
}

/// (item id, genre id) of every movie and series.
pub async fn item_genre_ids(db: &Db) -> Result<Vec<(i64, i64)>> {
    Ok(sqlx::query_as(
        "SELECT ig.item_id, ig.genre_id FROM item_genres ig JOIN items i ON i.id = ig.item_id
         WHERE i.kind IN ('movie', 'series')",
    )
    .fetch_all(db.reader())
    .await?)
}

// ---- Keywords ---------------------------------------------------------------------

/// Replaces an item's keywords.
pub async fn set_keywords(db: &Db, item_id: i64, keywords: &[String]) -> Result<()> {
    let mut tx = db.writer().begin().await?;
    sqlx::query("DELETE FROM item_keywords WHERE item_id = ?")
        .bind(item_id)
        .execute(&mut *tx)
        .await?;
    for (ord, keyword) in keywords.iter().enumerate() {
        sqlx::query("INSERT OR IGNORE INTO item_keywords (item_id, keyword, ord) VALUES (?, ?, ?)")
            .bind(item_id)
            .bind(keyword)
            .bind(ord as i64)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn keywords(db: &Db, item_id: i64) -> Result<Vec<String>> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT keyword FROM item_keywords WHERE item_id = ? ORDER BY ord")
            .bind(item_id)
            .fetch_all(db.reader())
            .await?;
    Ok(rows.into_iter().map(|(k,)| k).collect())
}

// ---- Onboarding -------------------------------------------------------------------

/// Replaces a profile's onboarding picks (by item; stored by identity).
pub async fn set_taste_seeds(db: &Db, user_id: i64, item_ids: &[i64]) -> Result<()> {
    let now = now_ms();
    let mut tx = db.writer().begin().await?;
    sqlx::query("DELETE FROM taste_seeds WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    for id in item_ids {
        sqlx::query(
            "INSERT OR IGNORE INTO taste_seeds (user_id, identity_key, created_at)
             SELECT ?, COALESCE(identity_key, content_key), ? FROM items WHERE id = ?",
        )
        .bind(user_id)
        .bind(now)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Onboarding picks of a profile: (item id, picked at).
pub async fn taste_seeds(db: &Db, user_id: i64) -> Result<Vec<(i64, i64)>> {
    Ok(sqlx::query_as(
        "SELECT item_id, created_at FROM (
             SELECT (SELECT i.id FROM items i WHERE i.identity_key = s.identity_key ORDER BY i.id LIMIT 1) AS item_id,
                    s.created_at
             FROM taste_seeds s WHERE s.user_id = ?)
         WHERE item_id IS NOT NULL",
    )
    .bind(user_id)
    .fetch_all(db.reader())
    .await?)
}

// ---- Viewing signals --------------------------------------------------------------

/// What a profile did with one title (episodes count for their series).
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct ViewingRow {
    /// Movie or series the activity belongs to.
    pub item_id: i64,
    pub played: bool,
    pub play_count: i64,
    pub favorite: bool,
    pub rating: Option<f64>,
    /// Resume position (0 = none).
    pub position_ms: i64,
    /// Share of the runtime reached in the watch history (0–1, 0 if unknown).
    pub watched_fraction: f64,
    /// Most recent activity (unix ms).
    pub at: i64,
}

/// A profile's viewing activity from `user_item_data` and the append-only
/// `watch_events`, one row per identity (several rows may share a series).
pub async fn viewing(db: &Db, user_id: i64) -> Result<Vec<ViewingRow>> {
    Ok(sqlx::query_as(
        "WITH events AS (
             SELECT identity_key, MAX(created_at) AS at, MAX(completed) AS completed,
                    MAX(CASE WHEN duration_ms > 0 THEN MIN(1.0, position_ms * 1.0 / duration_ms) ELSE 0 END) AS fraction
             FROM watch_events WHERE user_id = ?1 GROUP BY identity_key
         ),
         keys AS (
             SELECT identity_key FROM user_item_data WHERE user_id = ?1
             UNION SELECT identity_key FROM events
         )
         SELECT t.item_id,
                COALESCE(ud.played, 0) OR COALESCE(e.completed, 0) AS played,
                COALESCE(ud.play_count, 0) AS play_count,
                COALESCE(ud.favorite, 0) AS favorite,
                ud.rating,
                COALESCE(ud.position_ms, 0) AS position_ms,
                COALESCE(e.fraction, 0.0) AS watched_fraction,
                MAX(COALESCE(ud.last_played_at, 0), COALESCE(e.at, 0), COALESCE(ud.updated_at, 0)) AS at
         FROM keys k
         JOIN (SELECT identity_key, MIN(COALESCE(series_id, id)) AS item_id FROM items
               WHERE kind IN ('movie', 'episode', 'season', 'series') GROUP BY identity_key) t
              ON t.identity_key = k.identity_key
         LEFT JOIN user_item_data ud ON ud.user_id = ?1 AND ud.identity_key = k.identity_key
         LEFT JOIN events e ON e.identity_key = k.identity_key",
    )
    .bind(user_id)
    .fetch_all(db.reader())
    .await?)
}

/// Titles (movies/series) each profile has played or started: (user id, item id).
/// The household's co-watch signal and the "already seen" filter of group mode.
pub async fn household_watched(db: &Db) -> Result<Vec<(i64, i64)>> {
    Ok(sqlx::query_as(
        "SELECT DISTINCT ud.user_id, t.item_id FROM user_item_data ud
         JOIN (SELECT identity_key, MIN(COALESCE(series_id, id)) AS item_id FROM items
               WHERE kind IN ('movie', 'episode', 'season', 'series') GROUP BY identity_key) t
              ON t.identity_key = ud.identity_key
         WHERE ud.played = 1 OR ud.position_ms > 0
         UNION
         SELECT DISTINCT w.user_id, t.item_id FROM watch_events w
         JOIN (SELECT identity_key, MIN(COALESCE(series_id, id)) AS item_id FROM items
               WHERE kind IN ('movie', 'episode', 'season', 'series') GROUP BY identity_key) t
              ON t.identity_key = w.identity_key
         WHERE w.completed = 1",
    )
    .fetch_all(db.reader())
    .await?)
}
