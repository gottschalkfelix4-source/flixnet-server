//! Typed key/value settings stored as JSON.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Db, Result, now_ms};

/// Reads a setting; `None` if it has never been set.
pub async fn get<T: DeserializeOwned>(db: &Db, key: &str) -> Result<Option<T>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT value_json FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(db.reader())
        .await?;
    row.map(|(json,)| serde_json::from_str(&json))
        .transpose()
        .map_err(Into::into)
}

/// Creates or replaces a setting.
pub async fn set<T: Serialize + ?Sized>(db: &Db, key: &str, value: &T) -> Result<()> {
    let json = serde_json::to_string(value)?;
    sqlx::query(
        "INSERT INTO settings (key, value_json, updated_at) VALUES (?, ?, ?)
         ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json,
                                         updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(json)
    .bind(now_ms())
    .execute(db.writer())
    .await?;
    Ok(())
}

/// Removes a setting; returns whether it existed.
pub async fn delete(db: &Db, key: &str) -> Result<bool> {
    let result = sqlx::query("DELETE FROM settings WHERE key = ?")
        .bind(key)
        .execute(db.writer())
        .await?;
    Ok(result.rows_affected() > 0)
}
