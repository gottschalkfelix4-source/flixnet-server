//! Devices and their access tokens (one active token per device).

use crate::{Db, Result, now_ms};

pub struct NewSession<'a> {
    pub user_id: i64,
    pub device_uid: &'a str,
    pub client: &'a str,
    pub device_name: &'a str,
    /// SHA-256 (hex) of the token handed to the client.
    pub token_hash: &'a str,
}

/// An authenticated session resolved from a token.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Session {
    pub token_id: i64,
    pub user_id: i64,
    pub device_id: i64,
    pub last_used_at: i64,
    pub user_name: String,
    pub is_admin: bool,
    pub max_age_rating: Option<i64>,
    pub allow_unrated: bool,
    pub blocked_genres_json: Option<String>,
}

impl Session {
    /// The profile's parental controls.
    pub fn restriction(&self) -> crate::browse::Restriction {
        crate::browse::Restriction::from_profile(
            self.max_age_rating,
            self.allow_unrated,
            self.blocked_genres_json.as_deref(),
        )
    }
}

/// Registers (or refreshes) the device and issues a new token for it, revoking any
/// token the device held before. Returns the token id.
pub async fn create(db: &Db, s: &NewSession<'_>) -> Result<i64> {
    let now = now_ms();
    let mut tx = db.writer().begin().await?;

    let (device_id,): (i64,) = sqlx::query_as(
        "INSERT INTO devices (user_id, device_uid, client, name, created_at, last_seen)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT (user_id, device_uid) DO UPDATE
             SET client = excluded.client, name = excluded.name, last_seen = excluded.last_seen
         RETURNING id",
    )
    .bind(s.user_id)
    .bind(s.device_uid)
    .bind(s.client)
    .bind(s.device_name)
    .bind(now)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        "UPDATE access_tokens SET revoked_at = ? WHERE device_id = ? AND revoked_at IS NULL",
    )
    .bind(now)
    .bind(device_id)
    .execute(&mut *tx)
    .await?;

    let (token_id,): (i64,) = sqlx::query_as(
        "INSERT INTO access_tokens (token_hash, user_id, device_id, created_at, last_used_at)
         VALUES (?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(s.token_hash)
    .bind(s.user_id)
    .bind(device_id)
    .bind(now)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(token_id)
}

/// Resolves an active (not revoked) token.
pub async fn find_by_token_hash(db: &Db, token_hash: &str) -> Result<Option<Session>> {
    let session = sqlx::query_as(
        "SELECT t.id AS token_id, t.user_id, t.device_id, t.last_used_at,
                u.name AS user_name, u.is_admin, u.max_age_rating,
                u.allow_unrated, u.blocked_genres_json
         FROM access_tokens t JOIN users u ON u.id = t.user_id
         WHERE t.token_hash = ? AND t.revoked_at IS NULL",
    )
    .bind(token_hash)
    .fetch_optional(db.reader())
    .await?;
    Ok(session)
}

/// Records token use (callers throttle this; it is a write).
pub async fn touch(db: &Db, token_id: i64, device_id: i64) -> Result<()> {
    let now = now_ms();
    let mut tx = db.writer().begin().await?;
    sqlx::query("UPDATE access_tokens SET last_used_at = ? WHERE id = ?")
        .bind(now)
        .bind(token_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE devices SET last_seen = ? WHERE id = ?")
        .bind(now)
        .bind(device_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// A device of a user that currently holds an active token.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Device {
    pub id: i64,
    pub name: String,
    pub client: String,
    pub created_at: i64,
    pub last_seen: i64,
}

/// Devices with an active session, most recently used first.
pub async fn list_devices(db: &Db, user_id: i64) -> Result<Vec<Device>> {
    let devices = sqlx::query_as(
        "SELECT d.id, d.name, d.client, d.created_at, d.last_seen
         FROM devices d
         WHERE d.user_id = ?
           AND EXISTS (SELECT 1 FROM access_tokens t WHERE t.device_id = d.id AND t.revoked_at IS NULL)
         ORDER BY d.last_seen DESC",
    )
    .bind(user_id)
    .fetch_all(db.reader())
    .await?;
    Ok(devices)
}

/// Signs a device out (revokes its tokens). Scoped to the owner.
pub async fn revoke_device(db: &Db, user_id: i64, device_id: i64) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE access_tokens SET revoked_at = ?
         WHERE device_id = ? AND user_id = ? AND revoked_at IS NULL",
    )
    .bind(now_ms())
    .bind(device_id)
    .bind(user_id)
    .execute(db.writer())
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Revokes every token of a user, optionally keeping one (e.g. after a password change).
pub async fn revoke_all_for_user(db: &Db, user_id: i64, keep_token_id: Option<i64>) -> Result<()> {
    sqlx::query(
        "UPDATE access_tokens SET revoked_at = ?
         WHERE user_id = ? AND revoked_at IS NULL AND id IS NOT ?",
    )
    .bind(now_ms())
    .bind(user_id)
    .bind(keep_token_id)
    .execute(db.writer())
    .await?;
    Ok(())
}

pub async fn revoke(db: &Db, token_id: i64) -> Result<()> {
    sqlx::query("UPDATE access_tokens SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
        .bind(now_ms())
        .bind(token_id)
        .execute(db.writer())
        .await?;
    Ok(())
}
