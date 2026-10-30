//! Browsing queries: filtered, sorted, paged item lists for library grids and search.
//!
//! Queries are assembled with `QueryBuilder` from *static* SQL fragments only; every
//! user-supplied value is bound as a parameter.

use sqlx::{QueryBuilder, Sqlite};

use crate::{Db, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    #[default]
    Title,
    DateAdded,
    Year,
    Rating,
    PremiereDate,
    /// Search relevance (only with a search query).
    Relevance,
    /// Season/episode number (children of a series or season).
    Index,
}

/// Parental controls of a profile (Jugendschutz): what it must not see.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Restriction {
    /// Maximum age rating (`None` = no age limit).
    pub max_age: Option<i64>,
    /// With an age limit: content without an age rating stays visible (default: hidden).
    pub allow_unrated: bool,
    /// Hidden genres; a series' genres apply to its seasons and episodes.
    pub blocked_genres: Vec<String>,
}

impl Restriction {
    /// Restriction from the stored profile columns (`blocked_genres_json` is a JSON
    /// array of names; anything unreadable counts as no blocked genre).
    pub fn from_profile(
        max_age: Option<i64>,
        allow_unrated: bool,
        blocked_genres_json: Option<&str>,
    ) -> Self {
        Self {
            max_age,
            allow_unrated,
            blocked_genres: blocked_genres_json
                .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok())
                .unwrap_or_default(),
        }
    }

    /// Whether anything is hidden at all.
    pub fn is_restricted(&self) -> bool {
        self.max_age.is_some() || !self.blocked_genres.is_empty()
    }

    /// Whether `self` hides at least everything `other` hides, i.e. switching from a
    /// profile with `other` to one with `self` does not widen what can be watched.
    pub fn at_least_as_strict_as(&self, other: &Restriction) -> bool {
        let age_ok = match (self.max_age, other.max_age) {
            (_, None) => true,
            (None, Some(_)) => false,
            (Some(mine), Some(theirs)) => {
                mine <= theirs && (!self.allow_unrated || other.allow_unrated)
            }
        };
        age_ok
            && other.blocked_genres.iter().all(|g| {
                self.blocked_genres
                    .iter()
                    .any(|mine| mine.eq_ignore_ascii_case(g))
            })
    }
}

/// An age limit alone (the profile's other options at their defaults).
impl From<Option<i64>> for Restriction {
    fn from(max_age: Option<i64>) -> Self {
        Self {
            max_age,
            ..Default::default()
        }
    }
}

impl From<&Restriction> for Restriction {
    fn from(r: &Restriction) -> Self {
        r.clone()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ItemFilter {
    /// Libraries to look in (the caller restricts this to what the user may see).
    pub library_ids: Vec<i64>,
    pub kinds: Vec<String>,
    pub parent_id: Option<i64>,
    pub series_id: Option<i64>,
    /// Free text; turned into a safe FTS5 prefix query.
    pub search: Option<String>,
    pub genre: Option<String>,
    pub year_from: Option<i64>,
    pub year_to: Option<i64>,
    /// `a`–`z`, or `#` for everything else.
    pub letter: Option<char>,
    /// Content age limit of the viewer; unrated content is hidden when set (unless
    /// `allow_unrated`). Set all three parental fields with [`ItemFilter::restrict`].
    pub max_age: Option<i64>,
    /// With `max_age`: unrated content stays visible.
    pub allow_unrated: bool,
    /// Genres hidden from the viewer (parental controls).
    pub blocked_genres: Vec<String>,
    /// The viewer, for watched/favorite state on cards and filters.
    pub user_id: Option<i64>,
    pub is_played: Option<bool>,
    pub is_favorite: Option<bool>,
    pub sort: Sort,
    pub descending: bool,
    pub limit: i64,
    pub offset: i64,
    /// Any of these genres (smart playlists).
    pub genres: Vec<String>,
    /// Minimum community rating (0–10).
    pub min_rating: Option<f64>,
    /// Only these items (empty = no restriction).
    pub ids: Vec<i64>,
    /// Never these items.
    pub exclude_ids: Vec<i64>,
    /// Any of these years (empty = no restriction).
    pub years: Vec<i64>,
    /// Items any of these people are credited on.
    pub person_ids: Vec<i64>,
    /// Only items with (`true`) or without (`false`) a resume point of the viewer.
    pub is_resumable: Option<bool>,
    /// Runtime bounds in milliseconds (metadata runtime, else the main file's duration).
    pub min_runtime_ms: Option<i64>,
    pub max_runtime_ms: Option<i64>,
}

/// One row of a grid: enough to draw a poster card.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct ItemSummary {
    pub id: i64,
    pub library_id: i64,
    pub kind: String,
    pub parent_id: Option<i64>,
    pub series_id: Option<i64>,
    pub title: String,
    /// Title of the series (seasons and episodes).
    pub series_title: Option<String>,
    pub year: Option<i64>,
    pub index_number: Option<i64>,
    pub index_end: Option<i64>,
    pub parent_index_number: Option<i64>,
    pub premiere_date: Option<String>,
    pub runtime_ms: Option<i64>,
    pub community_rating: Option<f64>,
    pub official_rating: Option<String>,
    pub overview: Option<String>,
    pub date_added: i64,
    /// Best poster (thumb for episodes) and backdrop.
    pub poster_id: Option<i64>,
    pub poster_blurhash: Option<String>,
    pub backdrop_id: Option<i64>,
    /// Seasons of a series / episodes of a season.
    pub child_count: i64,
    /// The viewer's state (false/0 without a user).
    pub played: bool,
    pub favorite: bool,
    pub position_ms: i64,
}

/// FTS5 query from free text: each word quoted, the last one as a prefix.
/// `matr reload` → `"matr" "reload"*`. Returns `None` if nothing searchable remains.
pub fn fts_query(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(8)
        .map(|w| format!("\"{}\"", w.replace('"', "")))
        .collect();
    if words.is_empty() {
        return None;
    }
    Some(format!("{}*", words.join(" ")))
}

/// `FROM items i` plus, for searches, a join with the FTS index (`f.rank` = bm25).
fn push_from(qb: &mut QueryBuilder<Sqlite>, f: &ItemFilter) {
    qb.push(" FROM items i");
    if let Some(search) = &f.search {
        qb.push(
            " JOIN (SELECT rowid AS fid, bm25(items_fts, 10.0, 5.0, 1.0) AS rank
                    FROM items_fts WHERE items_fts MATCH ",
        )
        .push_bind(search.clone())
        .push(") f ON f.fid = i.id");
    }
}

/// `WHERE …` over `items i` for everything in the filter except the search (which
/// needs the FTS join from `push_from`). Also used by collections and playlists.
pub(crate) fn push_filters(qb: &mut QueryBuilder<Sqlite>, f: &ItemFilter) {
    qb.push(" WHERE i.library_id IN (");
    {
        let mut ids = qb.separated(", ");
        for id in &f.library_ids {
            ids.push_bind(*id);
        }
        if f.library_ids.is_empty() {
            ids.push("NULL");
        }
    }
    qb.push(")");
    if !f.kinds.is_empty() {
        qb.push(" AND i.kind IN (");
        let mut kinds = qb.separated(", ");
        for kind in &f.kinds {
            kinds.push_bind(kind.clone());
        }
        qb.push(")");
    }
    if let Some(parent) = f.parent_id {
        qb.push(" AND i.parent_id = ").push_bind(parent);
    }
    if let Some(series) = f.series_id {
        qb.push(" AND i.series_id = ").push_bind(series);
    }
    if let Some(genre) = &f.genre {
        // Driven from the genre (indexed), not checked row by row.
        qb.push(
            " AND i.id IN (SELECT ig.item_id FROM item_genres ig
                          WHERE ig.genre_id = (SELECT g.id FROM genres g WHERE g.name = ",
        )
        .push_bind(genre.clone())
        .push("))");
    }
    if !f.genres.is_empty() {
        qb.push(
            " AND i.id IN (SELECT ig.item_id FROM item_genres ig JOIN genres g ON g.id = ig.genre_id
                          WHERE g.name IN (",
        );
        let mut names = qb.separated(", ");
        for genre in &f.genres {
            names.push_bind(genre.clone());
        }
        qb.push("))");
    }
    if !f.ids.is_empty() {
        qb.push(" AND i.id IN (");
        let mut ids = qb.separated(", ");
        for id in &f.ids {
            ids.push_bind(*id);
        }
        qb.push(")");
    }
    if !f.exclude_ids.is_empty() {
        qb.push(" AND i.id NOT IN (");
        let mut ids = qb.separated(", ");
        for id in &f.exclude_ids {
            ids.push_bind(*id);
        }
        qb.push(")");
    }
    if !f.years.is_empty() {
        qb.push(" AND i.year IN (");
        let mut years = qb.separated(", ");
        for year in &f.years {
            years.push_bind(*year);
        }
        qb.push(")");
    }
    if !f.person_ids.is_empty() {
        qb.push(" AND i.id IN (SELECT c.item_id FROM credits c WHERE c.person_id IN (");
        let mut people = qb.separated(", ");
        for person in &f.person_ids {
            people.push_bind(*person);
        }
        qb.push("))");
    }
    if let (Some(user), Some(resumable)) = (f.user_id, f.is_resumable) {
        qb.push(if resumable { " AND " } else { " AND NOT " })
            .push("EXISTS (SELECT 1 FROM user_item_data ud WHERE ud.user_id = ")
            .push_bind(user)
            .push(" AND ud.identity_key = i.identity_key AND ud.position_ms > 0)");
    }
    if let Some(min) = f.min_rating {
        qb.push(" AND i.community_rating >= ").push_bind(min);
    }
    if f.min_runtime_ms.is_some() || f.max_runtime_ms.is_some() {
        let runtime = " COALESCE(i.runtime_ms, (SELECT MAX(rf.duration_ms) FROM media_files rf
                          WHERE rf.item_id = i.id AND rf.role = 'main'))";
        if let Some(min) = f.min_runtime_ms {
            qb.push(" AND").push(runtime).push(" >= ").push_bind(min);
        }
        if let Some(max) = f.max_runtime_ms {
            qb.push(" AND").push(runtime).push(" <= ").push_bind(max);
        }
    }
    if let Some(from) = f.year_from {
        qb.push(" AND i.year >= ").push_bind(from);
    }
    if let Some(to) = f.year_to {
        qb.push(" AND i.year <= ").push_bind(to);
    }
    match f.letter {
        Some('#') => {
            qb.push(" AND NOT (substr(i.sort_title, 1, 1) BETWEEN 'a' AND 'z')");
        }
        Some(letter) if letter.is_ascii_alphabetic() => {
            qb.push(" AND substr(i.sort_title, 1, 1) = ")
                .push_bind(letter.to_ascii_lowercase().to_string());
        }
        _ => {}
    }
    if let (Some(user), Some(played)) = (f.user_id, f.is_played) {
        qb.push(if played { " AND " } else { " AND NOT " })
            .push("EXISTS (SELECT 1 FROM user_item_data ud WHERE ud.user_id = ")
            .push_bind(user)
            .push(" AND ud.identity_key = i.identity_key AND ud.played = 1)");
    }
    if let (Some(user), Some(true)) = (f.user_id, f.is_favorite) {
        qb.push(" AND EXISTS (SELECT 1 FROM user_item_data ud WHERE ud.user_id = ")
            .push_bind(user)
            .push(" AND ud.identity_key = i.identity_key AND ud.favorite = 1)");
    }
    if let Some(max_age) = f.max_age {
        // Episodes and seasons inherit the series rating. Unrated content counts as
        // age 0 when allowed, otherwise as older than any limit.
        qb.push(
            " AND COALESCE(i.age_rating, (SELECT s.age_rating FROM items s WHERE s.id = i.series_id), ",
        )
        .push_bind(if f.allow_unrated { 0_i64 } else { 1000 })
        .push(") <= ")
        .push_bind(max_age);
    }
    if !f.blocked_genres.is_empty() {
        // The item's own genres and those of its series (seasons and episodes rarely
        // carry genres of their own).
        qb.push(
            " AND NOT EXISTS (SELECT 1 FROM item_genres bg JOIN genres g ON g.id = bg.genre_id
                              WHERE (bg.item_id = i.id OR bg.item_id = i.series_id)
                                AND g.name IN (",
        );
        let mut names = qb.separated(", ");
        for genre in &f.blocked_genres {
            names.push_bind(genre.clone());
        }
        qb.push("))");
    }
}

impl ItemFilter {
    /// Applies a profile's parental controls.
    pub fn restrict(&mut self, r: &Restriction) {
        self.max_age = r.max_age;
        self.allow_unrated = r.allow_unrated;
        self.blocked_genres = r.blocked_genres.clone();
    }
}

fn push_order(qb: &mut QueryBuilder<Sqlite>, f: &ItemFilter) {
    let dir = if f.descending { " DESC" } else { " ASC" };
    let nulls_last = |qb: &mut QueryBuilder<Sqlite>, column: &'static str| {
        qb.push(" ORDER BY (")
            .push(column)
            .push(" IS NULL), ")
            .push(column)
            .push(dir);
    };
    match f.sort {
        Sort::Title => {
            qb.push(" ORDER BY i.sort_title").push(dir);
        }
        // NOT NULL column: plain order so the (library, kind, date_added) index applies.
        Sort::DateAdded => {
            qb.push(" ORDER BY i.date_added").push(dir);
        }
        Sort::Year => nulls_last(qb, "i.year"),
        Sort::Rating => nulls_last(qb, "i.community_rating"),
        Sort::PremiereDate => nulls_last(qb, "i.premiere_date"),
        Sort::Index => {
            qb.push(" ORDER BY (i.parent_index_number IS NULL), i.parent_index_number")
                .push(dir)
                .push(", (i.index_number IS NULL), i.index_number")
                .push(dir);
        }
        Sort::Relevance => {
            // bm25: lower is better.
            qb.push(if f.search.is_some() {
                " ORDER BY f.rank"
            } else {
                " ORDER BY i.sort_title"
            });
        }
    }
    // Stable tie-breaker. Only episode lists need season/episode order; for everything
    // else `id` keeps the (library, kind, sort_title) index usable (rowid is its tail).
    let episodes = f.parent_id.is_some()
        || f.series_id.is_some()
        || f.kinds.iter().any(|k| k == "episode" || k == "season");
    qb.push(if episodes {
        ", i.parent_index_number, i.index_number, i.sort_title, i.id"
    } else {
        ", i.id"
    });
}

/// Two phases: first the ids of the requested page (cheap columns only, so sorting
/// 50k rows stays fast), then the card details for just those rows.
pub async fn list(db: &Db, f: &ItemFilter) -> Result<(Vec<ItemSummary>, i64)> {
    let mut page: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT i.id");
    push_from(&mut page, f);
    push_filters(&mut page, f);
    push_order(&mut page, f);
    page.push(" LIMIT ")
        .push_bind(f.limit)
        .push(" OFFSET ")
        .push_bind(f.offset);
    let ids: Vec<(i64,)> = page.build_query_as().fetch_all(db.reader()).await?;

    let mut count: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT COUNT(*)");
    push_from(&mut count, f);
    push_filters(&mut count, f);
    let (total,): (i64,) = count.build_query_as().fetch_one(db.reader()).await?;

    if ids.is_empty() {
        return Ok((Vec::new(), total));
    }
    let mut rows = summaries(
        db,
        &ids.iter().map(|(id,)| *id).collect::<Vec<_>>(),
        f.user_id,
    )
    .await?;
    // Restore the page order.
    let position: std::collections::HashMap<i64, usize> = ids
        .iter()
        .enumerate()
        .map(|(pos, (id,))| (*id, pos))
        .collect();
    rows.sort_by_key(|r| position.get(&r.id).copied().unwrap_or(usize::MAX));
    Ok((rows, total))
}

/// Card details (poster, backdrop, child count, the viewer's state) for specific items.
pub async fn summaries(db: &Db, ids: &[i64], user_id: Option<i64>) -> Result<Vec<ItemSummary>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT i.id, i.library_id, i.kind, i.parent_id, i.series_id, i.title,
                (SELECT s.title FROM items s WHERE s.id = i.series_id AND s.id != i.id) AS series_title,
                i.year,
                i.index_number, i.index_end, i.parent_index_number, i.premiere_date,
                -- Unmatched items have no runtime from metadata; the probed file has one.
                COALESCE(i.runtime_ms, (SELECT MAX(f.duration_ms) FROM media_files f
                   WHERE f.item_id = i.id AND f.role = 'main')) AS runtime_ms,
                i.community_rating, i.official_rating, i.overview, i.date_added,
                (SELECT im.id FROM images im WHERE im.item_id = i.id
                   AND im.kind = CASE i.kind WHEN 'episode' THEN 'thumb' ELSE 'poster' END
                   ORDER BY CASE im.source WHEN 'local' THEN 0 WHEN 'nfo' THEN 1 ELSE 2 END, im.score DESC, im.id
                   LIMIT 1) AS poster_id,
                (SELECT im.blurhash FROM images im WHERE im.item_id = i.id
                   AND im.kind = CASE i.kind WHEN 'episode' THEN 'thumb' ELSE 'poster' END
                   ORDER BY CASE im.source WHEN 'local' THEN 0 WHEN 'nfo' THEN 1 ELSE 2 END, im.score DESC, im.id
                   LIMIT 1) AS poster_blurhash,
                (SELECT im.id FROM images im WHERE im.item_id = i.id AND im.kind = 'backdrop'
                   ORDER BY CASE im.source WHEN 'local' THEN 0 WHEN 'nfo' THEN 1 ELSE 2 END, im.score DESC, im.id
                   LIMIT 1) AS backdrop_id,
                (SELECT COUNT(*) FROM items c WHERE c.parent_id = i.id) AS child_count,
                COALESCE(ud.played, 0) AS played,
                COALESCE(ud.favorite, 0) AS favorite,
                COALESCE(ud.position_ms, 0) AS position_ms
         FROM items i
         LEFT JOIN user_item_data ud ON ud.identity_key = i.identity_key AND ud.user_id = ",
    );
    qb.push_bind(user_id.unwrap_or(-1));
    qb.push(" WHERE i.id IN (");
    {
        let mut list = qb.separated(", ");
        for id in ids {
            list.push_bind(*id);
        }
    }
    qb.push(")");
    Ok(qb
        .build_query_as::<ItemSummary>()
        .fetch_all(db.reader())
        .await?)
}

/// Jump-bar buckets: how many items start with each letter, in title order.
pub async fn letters(db: &Db, f: &ItemFilter) -> Result<Vec<(char, i64)>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT CASE WHEN substr(i.sort_title, 1, 1) BETWEEN 'a' AND 'z'
                     THEN substr(i.sort_title, 1, 1) ELSE '#' END AS letter,
                COUNT(*) AS n",
    );
    let mut unlettered = f.clone();
    unlettered.letter = None;
    push_from(&mut qb, &unlettered);
    push_filters(&mut qb, &unlettered);
    qb.push(" GROUP BY letter ORDER BY MIN(i.sort_title)");
    let rows: Vec<(String, i64)> = qb.build_query_as().fetch_all(db.reader()).await?;
    Ok(rows
        .into_iter()
        .filter_map(|(l, n)| Some((l.chars().next()?, n)))
        .collect())
}

/// Genres used in the given libraries, counting the items the viewer may see (genres
/// with nothing visible are left out).
pub async fn genres(
    db: &Db,
    library_ids: &[i64],
    restriction: impl Into<Restriction>,
) -> Result<Vec<(String, i64)>> {
    let mut filter = ItemFilter {
        library_ids: library_ids.to_vec(),
        ..Default::default()
    };
    filter.restrict(&restriction.into());
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT g.name, COUNT(*) AS n FROM genres g
         JOIN item_genres ig ON ig.genre_id = g.id
         JOIN items i ON i.id = ig.item_id",
    );
    push_filters(&mut qb, &filter);
    qb.push(" GROUP BY g.id ORDER BY g.name");
    Ok(qb.build_query_as().fetch_all(db.reader()).await?)
}

/// Genres used in the given libraries: (genre id, name, item count), by name.
pub async fn genres_with_ids(db: &Db, library_ids: &[i64]) -> Result<Vec<(i64, String, i64)>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT g.id, g.name, COUNT(*) AS n FROM genres g
         JOIN item_genres ig ON ig.genre_id = g.id
         JOIN items i ON i.id = ig.item_id
         WHERE i.library_id IN (",
    );
    {
        let mut ids = qb.separated(", ");
        for id in library_ids {
            ids.push_bind(*id);
        }
        if library_ids.is_empty() {
            ids.push("NULL");
        }
    }
    qb.push(") GROUP BY g.id ORDER BY g.name");
    Ok(qb.build_query_as().fetch_all(db.reader()).await?)
}

/// Name of a genre by id.
pub async fn genre_name(db: &Db, genre_id: i64) -> Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT name FROM genres WHERE id = ?")
        .bind(genre_id)
        .fetch_optional(db.reader())
        .await?;
    Ok(row.map(|(name,)| name))
}

/// A person credited on visible items.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct PersonRow {
    pub id: i64,
    pub name: String,
    pub image_url: Option<String>,
}

/// People credited on items in the given libraries, by name; optionally only names
/// containing `search` (case-insensitive). Returns one page plus the total.
pub async fn people(
    db: &Db,
    library_ids: &[i64],
    search: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<(Vec<PersonRow>, i64)> {
    let push_where = |qb: &mut QueryBuilder<Sqlite>| {
        qb.push(
            " FROM people p WHERE EXISTS (SELECT 1 FROM credits c JOIN items i ON i.id = c.item_id
               WHERE c.person_id = p.id AND i.library_id IN (",
        );
        {
            let mut ids = qb.separated(", ");
            for id in library_ids {
                ids.push_bind(*id);
            }
            if library_ids.is_empty() {
                ids.push("NULL");
            }
        }
        qb.push("))");
        if let Some(search) = search.filter(|s| !s.trim().is_empty()) {
            let pattern = format!(
                "%{}%",
                search
                    .trim()
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            );
            qb.push(" AND p.name LIKE ")
                .push_bind(pattern)
                .push(" ESCAPE '\\'");
        }
    };
    let mut page: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT p.id, p.name, p.image_url");
    push_where(&mut page);
    page.push(" ORDER BY p.name COLLATE NOCASE, p.id LIMIT ")
        .push_bind(limit)
        .push(" OFFSET ")
        .push_bind(offset);
    let rows: Vec<PersonRow> = page.build_query_as().fetch_all(db.reader()).await?;
    let mut count: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT COUNT(*)");
    push_where(&mut count);
    let (total,): (i64,) = count.build_query_as().fetch_one(db.reader()).await?;
    Ok((rows, total))
}

/// Is a single item visible under the given restrictions? `restriction` is a
/// profile's [`Restriction`] (or just an age limit).
pub async fn is_visible(
    db: &Db,
    item_id: i64,
    library_ids: &[i64],
    restriction: impl Into<Restriction>,
) -> Result<bool> {
    let mut filter = ItemFilter {
        library_ids: library_ids.to_vec(),
        limit: 1,
        ..Default::default()
    };
    filter.restrict(&restriction.into());
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT COUNT(*) FROM items i");
    push_filters(&mut qb, &filter);
    qb.push(" AND i.id = ").push_bind(item_id);
    let (n,): (i64,) = qb.build_query_as().fetch_one(db.reader()).await?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_queries_are_quoted_prefix_queries() {
        assert_eq!(fts_query("matr").as_deref(), Some("\"matr\"*"));
        assert_eq!(fts_query("der pate").as_deref(), Some("\"der\" \"pate\"*"));
        // Operators and quotes cannot break out.
        assert_eq!(
            fts_query("a\" OR x NEAR(").as_deref(),
            Some("\"a\" \"OR\" \"x\" \"NEAR\"*")
        );
        assert_eq!(fts_query("  -- "), None);
    }
}
