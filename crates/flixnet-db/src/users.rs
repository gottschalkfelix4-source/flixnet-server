//! User accounts.

use crate::{Db, Result, now_ms};

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub password_hash: Option<String>,
    pub is_admin: bool,
    pub created_at: i64,
}

/// Column list matching [`User`]; a macro so queries stay `&'static str` (sqlx 0.9
/// rejects runtime-built SQL as an injection guard).
macro_rules! user_columns {
    () => {
        "id, name, password_hash, is_admin, created_at"
    };
}

pub async fn count(db: &Db) -> Result<i64> {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
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
