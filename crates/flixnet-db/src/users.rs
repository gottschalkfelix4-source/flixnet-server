//! User accounts and household profiles.

use crate::{Db, DbError, Result, now_ms};

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub password_hash: Option<String>,
    pub pin_hash: Option<String>,
    pub is_admin: bool,
    pub is_hidden: bool,
    pub max_age_rating: Option<i64>,
    pub all_libraries: bool,
    pub created_at: i64,
}

/// Column list matching [`User`]; a macro so queries stay `&'static str` (sqlx 0.9
/// rejects runtime-built SQL as an injection guard).
macro_rules! user_columns {
    () => {
        "id, name, password_hash, pin_hash, is_admin, is_hidden, max_age_rating, all_libraries, created_at"
    };
}

pub struct NewUser<'a> {
    pub name: &'a str,
    pub password_hash: Option<&'a str>,
    pub pin_hash: Option<&'a str>,
    pub is_admin: bool,
    pub is_hidden: bool,
    pub max_age_rating: Option<i64>,
}

/// Fields an administrator can change. `None` leaves a field untouched; the inner
/// `Option` of nullable fields distinguishes "clear" (`Some(None)`) from "keep".
#[derive(Default)]
pub struct UserPatch<'a> {
    pub name: Option<&'a str>,
    pub is_admin: Option<bool>,
    pub is_hidden: Option<bool>,
    pub max_age_rating: Option<Option<i64>>,
    pub password_hash: Option<Option<&'a str>>,
    pub pin_hash: Option<Option<&'a str>>,
}

pub async fn count(db: &Db) -> Result<i64> {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(db.reader())
        .await?;
    Ok(count)
}

pub async fn admin_count(db: &Db) -> Result<i64> {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users WHERE is_admin = 1")
        .fetch_one(db.reader())
        .await?;
    Ok(count)
}

/// Creates the initial administrator, but only while no user exists yet.
/// Returns `None` if setup has already been completed (atomic, race-free).
pub async fn create_first_admin(db: &Db, name: &str, password_hash: &str) -> Result<Option<User>> {
    let now = now_ms();
    let user = sqlx::query_as(concat!(
        "INSERT INTO users (name, password_hash, is_admin, created_at, updated_at)
         SELECT ?, ?, 1, ?, ? WHERE NOT EXISTS (SELECT 1 FROM users)
         RETURNING ",
        user_columns!()
    ))
    .bind(name)
    .bind(password_hash)
    .bind(now)
    .bind(now)
    .fetch_optional(db.writer())
    .await?;
    Ok(user)
}

/// Creates a user; [`DbError::Conflict`] if the name is taken.
pub async fn create(db: &Db, new: &NewUser<'_>) -> Result<User> {
    create_in(db.writer(), new).await
}

pub(crate) async fn create_in<'e, E>(executor: E, new: &NewUser<'_>) -> Result<User>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let now = now_ms();
    sqlx::query_as(concat!(
        "INSERT INTO users (name, password_hash, pin_hash, is_admin, is_hidden, max_age_rating,
                            created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         RETURNING ",
        user_columns!()
    ))
    .bind(new.name)
    .bind(new.password_hash)
    .bind(new.pin_hash)
    .bind(new.is_admin)
    .bind(new.is_hidden)
    .bind(new.max_age_rating)
    .bind(now)
    .bind(now)
    .fetch_one(executor)
    .await
    .map_err(DbError::from_unique)
}

pub async fn list(db: &Db) -> Result<Vec<User>> {
    let users = sqlx::query_as(concat!(
        "SELECT ",
        user_columns!(),
        " FROM users ORDER BY name COLLATE NOCASE"
    ))
    .fetch_all(db.reader())
    .await?;
    Ok(users)
}

/// Case-insensitive lookup by name.
pub async fn find_by_name(db: &Db, name: &str) -> Result<Option<User>> {
    let user = sqlx::query_as(concat!(
        "SELECT ",
        user_columns!(),
        " FROM users WHERE name = ?"
    ))
    .bind(name)
    .fetch_optional(db.reader())
    .await?;
    Ok(user)
}

pub async fn get(db: &Db, id: i64) -> Result<Option<User>> {
    let user = sqlx::query_as(concat!(
        "SELECT ",
        user_columns!(),
        " FROM users WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db.reader())
    .await?;
    Ok(user)
}

/// Applies a patch; returns the updated user (`None` if it does not exist).
pub async fn update(db: &Db, id: i64, patch: &UserPatch<'_>) -> Result<Option<User>> {
    // COALESCE keeps the old value when a parameter is NULL; the *_set flags make
    // "set to NULL" expressible for nullable columns.
    let user = sqlx::query_as(concat!(
        "UPDATE users SET
            name           = COALESCE(?, name),
            is_admin       = COALESCE(?, is_admin),
            is_hidden      = COALESCE(?, is_hidden),
            max_age_rating = CASE WHEN ? THEN ? ELSE max_age_rating END,
            password_hash  = CASE WHEN ? THEN ? ELSE password_hash END,
            pin_hash       = CASE WHEN ? THEN ? ELSE pin_hash END,
            updated_at     = ?
         WHERE id = ?
         RETURNING ",
        user_columns!()
    ))
    .bind(patch.name)
    .bind(patch.is_admin)
    .bind(patch.is_hidden)
    .bind(patch.max_age_rating.is_some())
    .bind(patch.max_age_rating.flatten())
    .bind(patch.password_hash.is_some())
    .bind(patch.password_hash.flatten())
    .bind(patch.pin_hash.is_some())
    .bind(patch.pin_hash.flatten())
    .bind(now_ms())
    .bind(id)
    .fetch_optional(db.writer())
    .await
    .map_err(DbError::from_unique)?;
    Ok(user)
}

pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    let result = sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(id)
        .execute(db.writer())
        .await?;
    Ok(result.rows_affected() > 0)
}
