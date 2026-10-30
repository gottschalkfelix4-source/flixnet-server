//! Collections: automatic ones from a provider (TMDB `belongs_to_collection`) and
//! manual ones curated by an administrator.
//!
//! Members are stored by identity key (see `userdata::update_identity`), not by item
//! id, so they survive rescans and renames. Reading always resolves identities to the
//! items the viewer may see (library access, age limit) — see [`member_ids`].

use sqlx::{QueryBuilder, Sqlite};

use crate::browse::{ItemFilter, push_filters};
use crate::{Db, DbError, Result, now_ms};

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Collection {
    pub id: i64,
    /// `auto` | `manual`
    pub kind: String,
    pub provider: Option<String>,
    pub provider_id: Option<String>,
    pub name: String,
    pub sort_name: String,
    pub overview: Option<String>,
    pub poster_url: Option<String>,
    pub poster_blurhash: Option<String>,
    pub backdrop_url: Option<String>,
    pub backdrop_blurhash: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

macro_rules! collection_columns {
    () => {
        "id, kind, provider, provider_id, name, sort_name, overview, poster_url, poster_blurhash,
         backdrop_url, backdrop_blurhash, created_at, updated_at"
    };
}

pub async fn get(db: &Db, id: i64) -> Result<Option<Collection>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        collection_columns!(),
        " FROM collections WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db.reader())
    .await?)
}

/// Every collection in name order (visibility is decided by [`visible_stats`]).
pub async fn list(db: &Db) -> Result<Vec<Collection>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        collection_columns!(),
        " FROM collections ORDER BY sort_name, id"
    ))
    .fetch_all(db.reader())
    .await?)
}

pub async fn find_by_provider(
    db: &Db,
    provider: &str,
    provider_id: &str,
) -> Result<Option<Collection>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        collection_columns!(),
        " FROM collections WHERE provider = ? AND provider_id = ?"
    ))
    .bind(provider)
    .bind(provider_id)
    .fetch_optional(db.reader())
    .await?)
}

pub async fn create_manual(
    db: &Db,
    name: &str,
    sort_name: &str,
    overview: Option<&str>,
) -> Result<Collection> {
    let now = now_ms();
    Ok(sqlx::query_as(concat!(
        "INSERT INTO collections (kind, name, sort_name, overview, created_at, updated_at)
         VALUES ('manual', ?, ?, ?, ?, ?) RETURNING ",
        collection_columns!()
    ))
    .bind(name)
    .bind(sort_name)
    .bind(overview)
    .bind(now)
    .bind(now)
    .fetch_one(db.writer())
    .await?)
}

/// Renames / re-describes a collection. `overview: Some(None)` clears it.
pub async fn update(
    db: &Db,
    id: i64,
    name: Option<(&str, &str)>,
    overview: Option<Option<&str>>,
) -> Result<bool> {
    let (name, sort_name) = name.unzip();
    let done = sqlx::query(
        "UPDATE collections SET
            name = COALESCE(?, name),
            sort_name = COALESCE(?, sort_name),
            overview = CASE WHEN ? THEN ? ELSE overview END,
            updated_at = ?
         WHERE id = ?",
    )
    .bind(name)
    .bind(sort_name)
    .bind(overview.is_some())
    .bind(overview.flatten())
    .bind(now_ms())
    .bind(id)
    .execute(db.writer())
    .await?;
    Ok(done.rows_affected() > 0)
}

pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    let done = sqlx::query("DELETE FROM collections WHERE id = ?")
        .bind(id)
        .execute(db.writer())
        .await?;
    Ok(done.rows_affected() > 0)
}

/// A collection as a provider describes it, with its parts in release order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderCollection {
    pub provider: String,
    pub provider_id: String,
    pub name: String,
    pub sort_name: String,
    pub overview: Option<String>,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    /// Identity keys of all parts, in release order.
    pub parts: Vec<String>,
}

/// Creates or updates an automatic collection and makes `member` (the identity of the
/// item that belongs to it) a member, together with all known parts. The member is
/// removed from other automatic collections of the same provider. One transaction;
/// call it after all provider requests are done.
pub async fn sync_provider(db: &Db, c: &ProviderCollection, member: &str) -> Result<i64> {
    let now = now_ms();
    let mut tx = db.writer().begin().await?;
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO collections (kind, provider, provider_id, name, sort_name, overview,
                                  poster_url, backdrop_url, created_at, updated_at)
         VALUES ('auto', ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (provider, provider_id) DO UPDATE SET
             name = excluded.name,
             sort_name = excluded.sort_name,
             overview = COALESCE(excluded.overview, collections.overview),
             poster_blurhash = CASE WHEN excluded.poster_url IS collections.poster_url
                                    THEN collections.poster_blurhash END,
             backdrop_blurhash = CASE WHEN excluded.backdrop_url IS collections.backdrop_url
                                      THEN collections.backdrop_blurhash END,
             poster_url = excluded.poster_url,
             backdrop_url = excluded.backdrop_url,
             updated_at = excluded.updated_at
         RETURNING id",
    )
    .bind(&c.provider)
    .bind(&c.provider_id)
    .bind(&c.name)
    .bind(&c.sort_name)
    .bind(&c.overview)
    .bind(&c.poster_url)
    .bind(&c.backdrop_url)
    .bind(now)
    .bind(now)
    .fetch_one(&mut *tx)
    .await
    .map_err(DbError::from_unique)?;

    sqlx::query(
        "DELETE FROM collection_items WHERE identity_key = ? AND collection_id != ?
           AND collection_id IN (SELECT id FROM collections WHERE kind = 'auto' AND provider = ?)",
    )
    .bind(member)
    .bind(id)
    .bind(&c.provider)
    .execute(&mut *tx)
    .await?;

    let mut parts: Vec<&str> = c.parts.iter().map(String::as_str).collect();
    if !parts.contains(&member) {
        parts.push(member);
    }
    for (ord, identity) in parts.iter().enumerate() {
        sqlx::query(
            "INSERT INTO collection_items (collection_id, identity_key, ord, added_at)
             VALUES (?, ?, ?, ?)
             ON CONFLICT (collection_id, identity_key) DO UPDATE SET ord = excluded.ord",
        )
        .bind(id)
        .bind(identity)
        .bind(ord as i64)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(id)
}

/// The item no longer belongs to any automatic collection of this provider.
pub async fn leave_provider_collections(db: &Db, provider: &str, member: &str) -> Result<u64> {
    let done = sqlx::query(
        "DELETE FROM collection_items WHERE identity_key = ?
           AND collection_id IN (SELECT id FROM collections WHERE kind = 'auto' AND provider = ?)",
    )
    .bind(member)
    .bind(provider)
    .execute(db.writer())
    .await?;
    Ok(done.rows_affected())
}

/// Adds members (identity keys) at the end; existing members are kept where they are.
pub async fn add_members(db: &Db, id: i64, identities: &[String]) -> Result<u64> {
    let now = now_ms();
    let mut tx = db.writer().begin().await?;
    let (next,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(MAX(ord) + 1, 0) FROM collection_items WHERE collection_id = ?",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let mut added = 0;
    for (i, identity) in identities.iter().enumerate() {
        added += sqlx::query(
            "INSERT INTO collection_items (collection_id, identity_key, ord, added_at)
             VALUES (?, ?, ?, ?) ON CONFLICT (collection_id, identity_key) DO NOTHING",
        )
        .bind(id)
        .bind(identity)
        .bind(next + i as i64)
        .bind(now)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    sqlx::query("UPDATE collections SET updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(added)
}

pub async fn remove_member(db: &Db, id: i64, identity: &str) -> Result<bool> {
    let done =
        sqlx::query("DELETE FROM collection_items WHERE collection_id = ? AND identity_key = ?")
            .bind(id)
            .bind(identity)
            .execute(db.writer())
            .await?;
    Ok(done.rows_affected() > 0)
}

/// Identity keys of all members, in stored order (resolvable or not).
pub async fn member_identities(db: &Db, id: i64) -> Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT identity_key FROM collection_items WHERE collection_id = ? ORDER BY ord, identity_key",
    )
    .bind(id)
    .fetch_all(db.reader())
    .await?;
    Ok(rows.into_iter().map(|(k,)| k).collect())
}

/// Stores the blurhash computed when the artwork was first served.
pub async fn set_blurhash(db: &Db, id: i64, kind: &str, blurhash: &str) -> Result<()> {
    let sql = if kind == "backdrop" {
        "UPDATE collections SET backdrop_blurhash = ? WHERE id = ?"
    } else {
        "UPDATE collections SET poster_blurhash = ? WHERE id = ?"
    };
    sqlx::query(sql)
        .bind(blurhash)
        .bind(id)
        .execute(db.writer())
        .await?;
    Ok(())
}

/// Per collection: how many members the viewer may see, how many of those they have
/// watched, and one visible member (for fallback artwork). Collections without a
/// visible member are absent.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct VisibleStats {
    pub collection_id: i64,
    pub item_count: i64,
    pub played_count: i64,
    pub first_item_id: i64,
}

/// `filter` carries the viewer's restrictions (`library_ids`, `max_age`); `user_id`
/// is the viewer for the watched counts.
pub async fn visible_stats(
    db: &Db,
    filter: &ItemFilter,
    user_id: i64,
    only: Option<i64>,
) -> Result<Vec<VisibleStats>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT ci.collection_id,
                COUNT(DISTINCT i.identity_key) AS item_count,
                COUNT(DISTINCT CASE WHEN ud.played = 1 THEN i.identity_key END) AS played_count,
                MIN(i.id) AS first_item_id
         FROM collection_items ci
         JOIN items i ON i.identity_key = ci.identity_key
         LEFT JOIN user_item_data ud ON ud.identity_key = i.identity_key AND ud.user_id = ",
    );
    qb.push_bind(user_id);
    push_filters(&mut qb, filter);
    if let Some(id) = only {
        qb.push(" AND ci.collection_id = ").push_bind(id);
    }
    qb.push(" GROUP BY ci.collection_id");
    Ok(qb.build_query_as().fetch_all(db.reader()).await?)
}

/// Visible members of a collection (one item per identity), in release order.
pub async fn member_ids(db: &Db, id: i64, filter: &ItemFilter) -> Result<Vec<i64>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT MIN(i.id) AS id FROM collection_items ci
         JOIN items i ON i.identity_key = ci.identity_key",
    );
    push_filters(&mut qb, filter);
    qb.push(" AND ci.collection_id = ").push_bind(id);
    qb.push(
        " GROUP BY ci.identity_key
          ORDER BY COALESCE(MIN(i.premiere_date), CAST(MIN(i.year) AS TEXT)) IS NULL,
                   COALESCE(MIN(i.premiere_date), CAST(MIN(i.year) AS TEXT)),
                   MIN(ci.ord), MIN(i.sort_title)",
    );
    let rows: Vec<(i64,)> = qb.build_query_as().fetch_all(db.reader()).await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Collections the item (by its identity) is a member of.
pub async fn of_item(db: &Db, item_id: i64) -> Result<Vec<i64>> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT ci.collection_id FROM collection_items ci
         JOIN items i ON i.identity_key = ci.identity_key
         JOIN collections c ON c.id = ci.collection_id
         WHERE i.id = ? ORDER BY c.sort_name, c.id",
    )
    .bind(item_id)
    .fetch_all(db.reader())
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Provider of theme collections (`kind = 'theme'`), proposed by the LLM.
pub const THEME_PROVIDER: &str = "llm";

/// A theme over the catalogue ("Zeitreisen", "Heist-Filme") with its members.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThemeCollection {
    /// Stable key of the theme (lowercase, from its name).
    pub slug: String,
    pub name: String,
    pub sort_name: String,
    pub overview: Option<String>,
    /// Identity keys of the members.
    pub members: Vec<String>,
}

/// Replaces all theme collections with `themes` in one transaction: existing themes
/// (same slug) keep their id, their members are replaced, themes not proposed again
/// are deleted. Returns the ids in input order.
pub async fn sync_themes(db: &Db, themes: &[ThemeCollection]) -> Result<Vec<i64>> {
    let now = now_ms();
    let mut tx = db.writer().begin().await?;
    let mut ids = Vec::with_capacity(themes.len());
    for theme in themes {
        let (id,): (i64,) = sqlx::query_as(
            "INSERT INTO collections (kind, provider, provider_id, name, sort_name, overview,
                                      created_at, updated_at)
             VALUES ('theme', ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (provider, provider_id) DO UPDATE SET
                 name = excluded.name,
                 sort_name = excluded.sort_name,
                 overview = excluded.overview,
                 updated_at = excluded.updated_at
             RETURNING id",
        )
        .bind(THEME_PROVIDER)
        .bind(&theme.slug)
        .bind(&theme.name)
        .bind(&theme.sort_name)
        .bind(&theme.overview)
        .bind(now)
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(DbError::from_unique)?;
        sqlx::query("DELETE FROM collection_items WHERE collection_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        for (ord, identity) in theme.members.iter().enumerate() {
            sqlx::query(
                "INSERT INTO collection_items (collection_id, identity_key, ord, added_at)
                 VALUES (?, ?, ?, ?) ON CONFLICT (collection_id, identity_key) DO NOTHING",
            )
            .bind(id)
            .bind(identity)
            .bind(ord as i64)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        ids.push(id);
    }
    let existing: Vec<(i64,)> =
        sqlx::query_as("SELECT id FROM collections WHERE kind = 'theme' AND provider = ?")
            .bind(THEME_PROVIDER)
            .fetch_all(&mut *tx)
            .await?;
    for (id,) in existing {
        if !ids.contains(&id) {
            sqlx::query("DELETE FROM collections WHERE id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(ids)
}

/// A movie or series of the whole catalogue, as the theme proposer sees it.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct CatalogEntry {
    pub id: i64,
    pub identity_key: String,
    pub kind: String,
    pub title: String,
    pub year: Option<i64>,
    pub overview: Option<String>,
    /// Comma-separated genre names.
    pub genres: Option<String>,
}

/// Movies and series of all libraries, one per identity, by title.
pub async fn catalog(db: &Db) -> Result<Vec<CatalogEntry>> {
    Ok(sqlx::query_as(
        "SELECT i.id, i.identity_key, i.kind, i.title, i.year, i.overview,
                (SELECT GROUP_CONCAT(g.name, ', ') FROM item_genres ig
                   JOIN genres g ON g.id = ig.genre_id WHERE ig.item_id = i.id) AS genres
         FROM items i
         WHERE i.kind IN ('movie', 'series') AND i.identity_key IS NOT NULL
           AND i.id = (SELECT MIN(o.id) FROM items o WHERE o.identity_key = i.identity_key)
         ORDER BY i.sort_title, i.id",
    )
    .fetch_all(db.reader())
    .await?)
}
