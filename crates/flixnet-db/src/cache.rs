//! Provider response cache.

use crate::{Db, Result, now_ms};

pub async fn get(db: &Db, provider: &str, key: &str) -> Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT body FROM provider_cache WHERE provider = ? AND key = ? AND expires_at > ?",
    )
    .bind(provider)
    .bind(key)
    .bind(now_ms())
    .fetch_optional(db.reader())
    .await?;
    Ok(row.map(|(b,)| b))
}

pub async fn put(db: &Db, provider: &str, key: &str, body: &str, ttl_ms: i64) -> Result<()> {
    sqlx::query(
        "INSERT INTO provider_cache (provider, key, body, expires_at) VALUES (?, ?, ?, ?)
         ON CONFLICT (provider, key) DO UPDATE SET body = excluded.body, expires_at = excluded.expires_at",
    )
    .bind(provider)
    .bind(key)
    .bind(body)
    .bind(now_ms() + ttl_ms)
    .execute(db.writer())
    .await?;
    Ok(())
}

pub async fn purge_expired(db: &Db) -> Result<u64> {
    let result = sqlx::query("DELETE FROM provider_cache WHERE expires_at <= ?")
        .bind(now_ms())
        .execute(db.writer())
        .await?;
    Ok(result.rows_affected())
}
