//! Descriptive metadata: item fields, artwork, genres, studios and credits.
//!
//! Fields carry a lock list on the item. Values from NFO files and manual edits are
//! locked; provider data (TMDB, TVDB) only fills fields that are not locked.

use crate::{Db, DbError, Result, now_ms};

/// Descriptive fields; `None` leaves a field untouched.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ItemFields {
    pub title: Option<String>,
    pub original_title: Option<String>,
    pub sort_title: Option<String>,
    pub year: Option<i64>,
    pub overview: Option<String>,
    pub tagline: Option<String>,
    pub runtime_ms: Option<i64>,
    pub premiere_date: Option<String>,
    pub official_rating: Option<String>,
    pub age_rating: Option<i64>,
    pub community_rating: Option<f64>,
}

impl ItemFields {
    /// Names of the fields that carry a value.
    pub fn present(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        let mut push = |set: bool, name| {
            if set {
                out.push(name);
            }
        };
        push(self.title.is_some(), "title");
        push(self.original_title.is_some(), "original_title");
        push(self.sort_title.is_some(), "sort_title");
        push(self.year.is_some(), "year");
        push(self.overview.is_some(), "overview");
        push(self.tagline.is_some(), "tagline");
        push(self.runtime_ms.is_some(), "runtime_ms");
        push(self.premiere_date.is_some(), "premiere_date");
        push(self.official_rating.is_some(), "official_rating");
        push(self.age_rating.is_some(), "age_rating");
        push(self.community_rating.is_some(), "community_rating");
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authority {
    /// NFO file or manual edit: always written, and locked afterwards.
    Locking,
    /// Provider data: only fills fields that are not locked.
    Provider,
}

/// Writes descriptive fields according to the lock rules. Returns the fields written.
pub async fn apply_fields(
    db: &Db,
    item_id: i64,
    fields: &ItemFields,
    authority: Authority,
) -> Result<Vec<&'static str>> {
    let mut tx = db.writer().begin().await?;
    let (locked_json,): (String,) = sqlx::query_as("SELECT locked_fields FROM items WHERE id = ?")
        .bind(item_id)
        .fetch_one(&mut *tx)
        .await?;
    let mut locked: Vec<String> = serde_json::from_str(&locked_json).unwrap_or_default();

    let allowed = |name: &str| authority == Authority::Locking || !locked.iter().any(|l| l == name);
    let write = |name: &'static str, present: bool| present && allowed(name);

    let flags = [
        write("title", fields.title.is_some()),
        write("original_title", fields.original_title.is_some()),
        write("sort_title", fields.sort_title.is_some()),
        write("year", fields.year.is_some()),
        write("overview", fields.overview.is_some()),
        write("tagline", fields.tagline.is_some()),
        write("runtime_ms", fields.runtime_ms.is_some()),
        write("premiere_date", fields.premiere_date.is_some()),
        write("official_rating", fields.official_rating.is_some()),
        write("age_rating", fields.age_rating.is_some()),
        write("community_rating", fields.community_rating.is_some()),
    ];

    sqlx::query(
        "UPDATE items SET
            title            = CASE WHEN ? THEN ? ELSE title END,
            original_title   = CASE WHEN ? THEN ? ELSE original_title END,
            sort_title       = CASE WHEN ? THEN ? ELSE sort_title END,
            year             = CASE WHEN ? THEN ? ELSE year END,
            overview         = CASE WHEN ? THEN ? ELSE overview END,
            tagline          = CASE WHEN ? THEN ? ELSE tagline END,
            runtime_ms       = CASE WHEN ? THEN ? ELSE runtime_ms END,
            premiere_date    = CASE WHEN ? THEN ? ELSE premiere_date END,
            official_rating  = CASE WHEN ? THEN ? ELSE official_rating END,
            age_rating       = CASE WHEN ? THEN ? ELSE age_rating END,
            community_rating = CASE WHEN ? THEN ? ELSE community_rating END,
            date_modified    = ?
         WHERE id = ?",
    )
    .bind(flags[0])
    .bind(&fields.title)
    .bind(flags[1])
    .bind(&fields.original_title)
    .bind(flags[2])
    .bind(&fields.sort_title)
    .bind(flags[3])
    .bind(fields.year)
    .bind(flags[4])
    .bind(&fields.overview)
    .bind(flags[5])
    .bind(&fields.tagline)
    .bind(flags[6])
    .bind(fields.runtime_ms)
    .bind(flags[7])
    .bind(&fields.premiere_date)
    .bind(flags[8])
    .bind(&fields.official_rating)
    .bind(flags[9])
    .bind(fields.age_rating)
    .bind(flags[10])
    .bind(fields.community_rating)
    .bind(now_ms())
    .bind(item_id)
    .execute(&mut *tx)
    .await?;

    let present = fields.present();
    let written: Vec<&'static str> = present
        .iter()
        .zip(present.iter().map(|name| allowed(name)))
        .filter_map(|(name, ok)| ok.then_some(*name))
        .collect();
    if authority == Authority::Locking {
        for name in &written {
            if !locked.iter().any(|l| l == name) {
                locked.push((*name).to_owned());
            }
        }
        sqlx::query("UPDATE items SET locked_fields = ? WHERE id = ?")
            .bind(serde_json::to_string(&locked)?)
            .bind(item_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(written)
}

pub async fn locked_fields(db: &Db, item_id: i64) -> Result<Vec<String>> {
    let (json,): (String,) = sqlx::query_as("SELECT locked_fields FROM items WHERE id = ?")
        .bind(item_id)
        .fetch_one(db.reader())
        .await?;
    Ok(serde_json::from_str(&json).unwrap_or_default())
}

pub async fn mark_refreshed(db: &Db, item_id: i64) -> Result<()> {
    sqlx::query("UPDATE items SET metadata_refreshed_at = ? WHERE id = ?")
        .bind(now_ms())
        .bind(item_id)
        .execute(db.writer())
        .await?;
    Ok(())
}

/// Sets a provider id (overwrites the previous value for that provider).
pub async fn set_provider_id(db: &Db, item_id: i64, provider: &str, value: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO provider_ids (item_id, provider, value) VALUES (?, ?, ?)
         ON CONFLICT (item_id, provider) DO UPDATE SET value = excluded.value",
    )
    .bind(item_id)
    .bind(provider)
    .bind(value)
    .execute(db.writer())
    .await?;
    Ok(())
}

// ---- Genres and studios ---------------------------------------------------------------

async fn set_names(db: &Db, item_id: i64, names: &[String], table: &str) -> Result<()> {
    let (insert_name, find_id, delete_links, insert_link) = match table {
        "genres" => (
            "INSERT INTO genres (name) VALUES (?) ON CONFLICT (name) DO NOTHING",
            "SELECT id FROM genres WHERE name = ?",
            "DELETE FROM item_genres WHERE item_id = ?",
            "INSERT INTO item_genres (item_id, genre_id) VALUES (?, ?) ON CONFLICT DO NOTHING",
        ),
        _ => (
            "INSERT INTO studios (name) VALUES (?) ON CONFLICT (name) DO NOTHING",
            "SELECT id FROM studios WHERE name = ?",
            "DELETE FROM item_studios WHERE item_id = ?",
            "INSERT INTO item_studios (item_id, studio_id) VALUES (?, ?) ON CONFLICT DO NOTHING",
        ),
    };
    let mut tx = db.writer().begin().await?;
    sqlx::query(delete_links)
        .bind(item_id)
        .execute(&mut *tx)
        .await?;
    for name in names.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
        sqlx::query(insert_name)
            .bind(name)
            .execute(&mut *tx)
            .await?;
        let (id,): (i64,) = sqlx::query_as(find_id)
            .bind(name)
            .fetch_one(&mut *tx)
            .await?;
        sqlx::query(insert_link)
            .bind(item_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn set_genres(db: &Db, item_id: i64, names: &[String]) -> Result<()> {
    set_names(db, item_id, names, "genres").await
}

pub async fn set_studios(db: &Db, item_id: i64, names: &[String]) -> Result<()> {
    set_names(db, item_id, names, "studios").await
}

pub async fn genres(db: &Db, item_id: i64) -> Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT g.name FROM genres g JOIN item_genres ig ON ig.genre_id = g.id
         WHERE ig.item_id = ? ORDER BY g.name",
    )
    .bind(item_id)
    .fetch_all(db.reader())
    .await?;
    Ok(rows.into_iter().map(|(n,)| n).collect())
}

pub async fn studios(db: &Db, item_id: i64) -> Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT s.name FROM studios s JOIN item_studios i ON i.studio_id = s.id
         WHERE i.item_id = ? ORDER BY s.name",
    )
    .bind(item_id)
    .fetch_all(db.reader())
    .await?;
    Ok(rows.into_iter().map(|(n,)| n).collect())
}

// ---- Credits -------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CreditInput {
    pub name: String,
    pub role: String,
    pub character: Option<String>,
    pub order: i64,
    pub tmdb_id: Option<i64>,
    pub image_url: Option<String>,
    pub image_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Credit {
    pub person_id: i64,
    pub name: String,
    pub role: String,
    pub character: Option<String>,
    pub ord: i64,
    pub image_url: Option<String>,
    pub image_path: Option<String>,
}

/// Replaces an item's credits. People are reused by TMDB id, else by exact name.
pub async fn set_credits(db: &Db, item_id: i64, credits: &[CreditInput]) -> Result<()> {
    let mut tx = db.writer().begin().await?;
    sqlx::query("DELETE FROM credits WHERE item_id = ?")
        .bind(item_id)
        .execute(&mut *tx)
        .await?;
    for credit in credits {
        let name = credit.name.trim();
        if name.is_empty() {
            continue;
        }
        let existing: Option<(i64,)> = match credit.tmdb_id {
            Some(tmdb) => {
                sqlx::query_as("SELECT id FROM people WHERE tmdb_id = ?")
                    .bind(tmdb)
                    .fetch_optional(&mut *tx)
                    .await?
            }
            None => {
                sqlx::query_as("SELECT id FROM people WHERE name = ? AND tmdb_id IS NULL")
                    .bind(name)
                    .fetch_optional(&mut *tx)
                    .await?
            }
        };
        let person_id = match existing {
            Some((id,)) => {
                sqlx::query(
                    "UPDATE people SET image_url = COALESCE(?, image_url),
                                       image_path = COALESCE(?, image_path) WHERE id = ?",
                )
                .bind(&credit.image_url)
                .bind(&credit.image_path)
                .bind(id)
                .execute(&mut *tx)
                .await?;
                id
            }
            None => {
                let (id,): (i64,) = sqlx::query_as(
                    "INSERT INTO people (name, sort_name, tmdb_id, image_url, image_path, created_at)
                     VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
                )
                .bind(name)
                .bind(person_sort_name(name))
                .bind(credit.tmdb_id)
                .bind(&credit.image_url)
                .bind(&credit.image_path)
                .bind(now_ms())
                .fetch_one(&mut *tx)
                .await
                .map_err(DbError::from_unique)?;
                id
            }
        };
        sqlx::query(
            "INSERT INTO credits (item_id, person_id, role, character, ord) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(item_id)
        .bind(person_id)
        .bind(&credit.role)
        .bind(&credit.character)
        .bind(credit.order)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// "Keanu Reeves" → "Reeves, Keanu".
fn person_sort_name(name: &str) -> String {
    match name.rsplit_once(' ') {
        Some((first, last)) => format!("{last}, {first}"),
        None => name.to_owned(),
    }
}

pub async fn credits(db: &Db, item_id: i64) -> Result<Vec<Credit>> {
    let rows = sqlx::query_as(
        "SELECT p.id AS person_id, p.name, c.role, c.character, c.ord, p.image_url, p.image_path
         FROM credits c JOIN people p ON p.id = c.person_id
         WHERE c.item_id = ?
         ORDER BY CASE c.role WHEN 'actor' THEN 0 WHEN 'director' THEN 1 WHEN 'creator' THEN 2
                              WHEN 'writer' THEN 3 ELSE 4 END, c.ord",
    )
    .bind(item_id)
    .fetch_all(db.reader())
    .await?;
    Ok(rows)
}

// ---- Images ----------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewImage {
    pub kind: String,
    pub path: Option<String>,
    pub url: Option<String>,
    pub language: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Image {
    pub id: i64,
    pub item_id: i64,
    pub kind: String,
    pub source: String,
    pub path: Option<String>,
    pub url: Option<String>,
    pub language: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub blurhash: Option<String>,
    pub score: f64,
}

/// Replaces all images of one source for an item.
pub async fn replace_images(
    db: &Db,
    item_id: i64,
    source: &str,
    images: &[NewImage],
) -> Result<()> {
    let mut tx = db.writer().begin().await?;
    sqlx::query("DELETE FROM images WHERE item_id = ? AND source = ?")
        .bind(item_id)
        .bind(source)
        .execute(&mut *tx)
        .await?;
    let now = now_ms();
    for image in images {
        sqlx::query(
            "INSERT INTO images (item_id, kind, source, path, url, language, width, height, score, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(item_id)
        .bind(&image.kind)
        .bind(source)
        .bind(&image.path)
        .bind(&image.url)
        .bind(&image.language)
        .bind(image.width)
        .bind(image.height)
        .bind(image.score)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// All images of an item, best first per kind (local files beat providers).
pub async fn images(db: &Db, item_id: i64) -> Result<Vec<Image>> {
    let rows = sqlx::query_as(
        "SELECT id, item_id, kind, source, path, url, language, width, height, blurhash, score
         FROM images WHERE item_id = ?
         ORDER BY kind, CASE source WHEN 'local' THEN 0 WHEN 'nfo' THEN 1 ELSE 2 END, score DESC, id",
    )
    .bind(item_id)
    .fetch_all(db.reader())
    .await?;
    Ok(rows)
}

pub async fn set_image_details(
    db: &Db,
    image_id: i64,
    width: i64,
    height: i64,
    blurhash: &str,
) -> Result<()> {
    sqlx::query("UPDATE images SET width = ?, height = ?, blurhash = ? WHERE id = ?")
        .bind(width)
        .bind(height)
        .bind(blurhash)
        .bind(image_id)
        .execute(db.writer())
        .await?;
    Ok(())
}
