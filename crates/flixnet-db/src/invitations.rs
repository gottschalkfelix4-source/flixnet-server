//! Single-use invitation links.

use crate::users::{self, NewUser, User};
use crate::{Db, DbError, Result, now_ms};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Invitation {
    pub id: i64,
    pub note: Option<String>,
    pub max_age_rating: Option<i64>,
    pub created_by: Option<i64>,
    pub created_at: i64,
    pub expires_at: i64,
    pub used_at: Option<i64>,
    pub used_by: Option<i64>,
}

macro_rules! invitation_columns {
    () => {
        "id, note, max_age_rating, created_by, created_at, expires_at, used_at, used_by"
    };
}

pub struct NewInvitation<'a> {
    pub code_hash: &'a str,
    pub note: Option<&'a str>,
    pub max_age_rating: Option<i64>,
    pub created_by: i64,
    pub expires_at: i64,
}

pub async fn create(db: &Db, new: &NewInvitation<'_>) -> Result<Invitation> {
    let invitation = sqlx::query_as(concat!(
        "INSERT INTO invitations (code_hash, note, max_age_rating, created_by, created_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?) RETURNING ",
        invitation_columns!()
    ))
    .bind(new.code_hash)
    .bind(new.note)
    .bind(new.max_age_rating)
    .bind(new.created_by)
    .bind(now_ms())
    .bind(new.expires_at)
    .fetch_one(db.writer())
    .await?;
    Ok(invitation)
}

pub async fn list(db: &Db) -> Result<Vec<Invitation>> {
    let invitations = sqlx::query_as(concat!(
        "SELECT ",
        invitation_columns!(),
        " FROM invitations ORDER BY created_at DESC"
    ))
    .fetch_all(db.reader())
    .await?;
    Ok(invitations)
}

pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    let result = sqlx::query("DELETE FROM invitations WHERE id = ?")
        .bind(id)
        .execute(db.writer())
        .await?;
    Ok(result.rows_affected() > 0)
}

/// An unused, unexpired invitation for this code hash.
pub async fn find_valid(db: &Db, code_hash: &str) -> Result<Option<Invitation>> {
    let invitation = sqlx::query_as(concat!(
        "SELECT ",
        invitation_columns!(),
        " FROM invitations WHERE code_hash = ? AND used_at IS NULL AND expires_at > ?"
    ))
    .bind(code_hash)
    .bind(now_ms())
    .fetch_optional(db.reader())
    .await?;
    Ok(invitation)
}

#[derive(Debug)]
pub enum AcceptError {
    /// Unknown, expired or already used.
    Invalid,
    /// The chosen user name is taken.
    NameTaken,
    Db(DbError),
}

impl From<DbError> for AcceptError {
    fn from(err: DbError) -> Self {
        AcceptError::Db(err)
    }
}

impl From<sqlx::Error> for AcceptError {
    fn from(err: sqlx::Error) -> Self {
        AcceptError::Db(err.into())
    }
}

/// Redeems an invitation and creates the new user in one transaction, so a code can
/// never create two accounts and a taken name leaves the invitation usable.
pub async fn accept(
    db: &Db,
    code_hash: &str,
    name: &str,
    password_hash: &str,
) -> std::result::Result<User, AcceptError> {
    let now = now_ms();
    let mut tx = db.writer().begin().await?;

    let claimed: Option<(i64, Option<i64>)> = sqlx::query_as(
        "UPDATE invitations SET used_at = ?
         WHERE code_hash = ? AND used_at IS NULL AND expires_at > ?
         RETURNING id, max_age_rating",
    )
    .bind(now)
    .bind(code_hash)
    .bind(now)
    .fetch_optional(&mut *tx)
    .await?;
    let (invitation_id, max_age_rating) = claimed.ok_or(AcceptError::Invalid)?;

    let user = users::create_in(
        &mut *tx,
        &NewUser {
            name,
            password_hash: Some(password_hash),
            pin_hash: None,
            is_admin: false,
            is_hidden: false,
            max_age_rating,
        },
    )
    .await
    .map_err(|err| match err {
        DbError::Conflict => AcceptError::NameTaken,
        other => AcceptError::Db(other),
    })?;

    sqlx::query("UPDATE invitations SET used_by = ? WHERE id = ?")
        .bind(user.id)
        .bind(invitation_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(user)
}
