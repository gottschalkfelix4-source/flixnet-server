//! Operations data: notification targets, Trakt connections and single sign-on
//! identities.

use crate::{Db, DbError, Result, now_ms};

// ---- Notification targets -----------------------------------------------------------

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct NotificationTarget {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub enabled: bool,
    pub config_json: String,
    pub events_json: String,
    pub created_at: i64,
    pub updated_at: i64,
}

macro_rules! target_columns {
    () => {
        "id, name, kind, enabled, config_json, events_json, created_at, updated_at"
    };
}

pub struct NewTarget<'a> {
    pub name: &'a str,
    pub kind: &'a str,
    pub enabled: bool,
    pub config_json: &'a str,
    pub events_json: &'a str,
}

pub async fn list_targets(db: &Db) -> Result<Vec<NotificationTarget>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        target_columns!(),
        " FROM notification_targets ORDER BY name COLLATE NOCASE, id"
    ))
    .fetch_all(db.reader())
    .await?)
}

pub async fn get_target(db: &Db, id: i64) -> Result<Option<NotificationTarget>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        target_columns!(),
        " FROM notification_targets WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db.reader())
    .await?)
}

pub async fn create_target(db: &Db, new: &NewTarget<'_>) -> Result<NotificationTarget> {
    let now = now_ms();
    Ok(sqlx::query_as(concat!(
        "INSERT INTO notification_targets
             (name, kind, enabled, config_json, events_json, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING ",
        target_columns!()
    ))
    .bind(new.name)
    .bind(new.kind)
    .bind(new.enabled)
    .bind(new.config_json)
    .bind(new.events_json)
    .bind(now)
    .bind(now)
    .fetch_one(db.writer())
    .await?)
}

/// Replaces name, state, configuration and events (the kind never changes).
pub async fn update_target(
    db: &Db,
    id: i64,
    new: &NewTarget<'_>,
) -> Result<Option<NotificationTarget>> {
    Ok(sqlx::query_as(concat!(
        "UPDATE notification_targets SET name = ?, enabled = ?, config_json = ?, events_json = ?,
                updated_at = ?
         WHERE id = ? RETURNING ",
        target_columns!()
    ))
    .bind(new.name)
    .bind(new.enabled)
    .bind(new.config_json)
    .bind(new.events_json)
    .bind(now_ms())
    .bind(id)
    .fetch_optional(db.writer())
    .await?)
}

pub async fn delete_target(db: &Db, id: i64) -> Result<bool> {
    let result = sqlx::query("DELETE FROM notification_targets WHERE id = ?")
        .bind(id)
        .execute(db.writer())
        .await?;
    Ok(result.rows_affected() > 0)
}

// ---- Trakt --------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct TraktAccount {
    pub user_id: i64,
    pub username: Option<String>,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub scrobble: bool,
    pub sync_ratings: bool,
    pub last_sync_at: Option<i64>,
    pub last_sync_report: Option<String>,
}

pub async fn trakt_account(db: &Db, user_id: i64) -> Result<Option<TraktAccount>> {
    Ok(sqlx::query_as(
        "SELECT user_id, username, access_token, refresh_token, expires_at, scrobble,
                sync_ratings, last_sync_at, last_sync_report
         FROM trakt_accounts WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_optional(db.reader())
    .await?)
}

/// Stores (new or refreshed) tokens; options of an existing connection are kept.
pub async fn save_trakt_tokens(
    db: &Db,
    user_id: i64,
    access_token: &str,
    refresh_token: &str,
    expires_at: i64,
    username: Option<&str>,
) -> Result<()> {
    let now = now_ms();
    sqlx::query(
        "INSERT INTO trakt_accounts
             (user_id, username, access_token, refresh_token, expires_at, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (user_id) DO UPDATE SET
             username = COALESCE(excluded.username, trakt_accounts.username),
             access_token = excluded.access_token,
             refresh_token = excluded.refresh_token,
             expires_at = excluded.expires_at,
             updated_at = excluded.updated_at",
    )
    .bind(user_id)
    .bind(username)
    .bind(access_token)
    .bind(refresh_token)
    .bind(expires_at)
    .bind(now)
    .bind(now)
    .execute(db.writer())
    .await?;
    Ok(())
}

pub async fn set_trakt_options(
    db: &Db,
    user_id: i64,
    scrobble: Option<bool>,
    sync_ratings: Option<bool>,
) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE trakt_accounts SET scrobble = COALESCE(?, scrobble),
                sync_ratings = COALESCE(?, sync_ratings), updated_at = ?
         WHERE user_id = ?",
    )
    .bind(scrobble)
    .bind(sync_ratings)
    .bind(now_ms())
    .bind(user_id)
    .execute(db.writer())
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn set_trakt_synced(db: &Db, user_id: i64, at: i64, report_json: &str) -> Result<()> {
    sqlx::query(
        "UPDATE trakt_accounts SET last_sync_at = ?, last_sync_report = ?, updated_at = ?
         WHERE user_id = ?",
    )
    .bind(at)
    .bind(report_json)
    .bind(now_ms())
    .bind(user_id)
    .execute(db.writer())
    .await?;
    Ok(())
}

/// Users connected to Trakt.
pub async fn trakt_user_ids(db: &Db) -> Result<Vec<i64>> {
    let rows: Vec<(i64,)> = sqlx::query_as("SELECT user_id FROM trakt_accounts ORDER BY user_id")
        .fetch_all(db.reader())
        .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

pub async fn delete_trakt_account(db: &Db, user_id: i64) -> Result<bool> {
    let result = sqlx::query("DELETE FROM trakt_accounts WHERE user_id = ?")
        .bind(user_id)
        .execute(db.writer())
        .await?;
    Ok(result.rows_affected() > 0)
}

// ---- Single sign-on -----------------------------------------------------------------

/// The Flixnet user linked to an OIDC account (and records the sign-in).
pub async fn oidc_user(db: &Db, issuer: &str, subject: &str) -> Result<Option<i64>> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT user_id FROM oidc_identities WHERE issuer = ? AND subject = ?")
            .bind(issuer)
            .bind(subject)
            .fetch_optional(db.reader())
            .await?;
    if row.is_some() {
        sqlx::query(
            "UPDATE oidc_identities SET last_login_at = ? WHERE issuer = ? AND subject = ?",
        )
        .bind(now_ms())
        .bind(issuer)
        .bind(subject)
        .execute(db.writer())
        .await?;
    }
    Ok(row.map(|(id,)| id))
}

/// Links an OIDC account to a user; [`DbError::Conflict`] if it is linked already.
pub async fn link_oidc(
    db: &Db,
    user_id: i64,
    issuer: &str,
    subject: &str,
    email: Option<&str>,
) -> Result<()> {
    let now = now_ms();
    sqlx::query(
        "INSERT INTO oidc_identities (user_id, issuer, subject, email, created_at, last_login_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(issuer)
    .bind(subject)
    .bind(email)
    .bind(now)
    .bind(now)
    .execute(db.writer())
    .await
    .map_err(DbError::from_unique)?;
    Ok(())
}

/// Whether a user linked to this e-mail address exists (an earlier OIDC sign-in).
pub async fn oidc_user_by_email(db: &Db, email: &str) -> Result<Option<i64>> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT user_id FROM oidc_identities WHERE email = ? COLLATE NOCASE
         ORDER BY last_login_at DESC LIMIT 1",
    )
    .bind(email)
    .fetch_optional(db.reader())
    .await?;
    Ok(row.map(|(id,)| id))
}

// ---- Devices ------------------------------------------------------------------------

/// Whether this user has signed in on this device before.
pub async fn device_known(db: &Db, user_id: i64, device_uid: &str) -> Result<bool> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM devices WHERE user_id = ? AND device_uid = ?")
            .bind(user_id)
            .bind(device_uid)
            .fetch_optional(db.reader())
            .await?;
    Ok(row.is_some())
}
