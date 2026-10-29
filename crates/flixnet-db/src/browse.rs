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
    /// Content age limit of the viewer; unrated content is hidden when set.
    pub max_age: Option<i64>,
    /// The viewer, for watched/favorite state on cards and filters.
    pub user_id: Option<i64>,
    pub is_played: Option<bool>,
    pub is_favorite: Option<bool>,
    pub sort: Sort,
    pub descending: bool,
    pub limit: i64,
    pub offset: i64,
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
        // Episodes and seasons inherit the series rating; unrated content is hidden.
        qb.push(
            " AND COALESCE(i.age_rating, (SELECT s.age_rating FROM items s WHERE s.id = i.series_id)) <= ",
        )
        .push_bind(max_age);
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

/// Genres used in the given libraries, with counts.
pub async fn genres(db: &Db, library_ids: &[i64]) -> Result<Vec<(String, i64)>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT g.name, COUNT(*) AS n FROM genres g
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

/// Is a single item visible under the given restrictions?
pub async fn is_visible(
    db: &Db,
    item_id: i64,
    library_ids: &[i64],
    max_age: Option<i64>,
) -> Result<bool> {
    let filter = ItemFilter {
        library_ids: library_ids.to_vec(),
        max_age,
        limit: 1,
        ..Default::default()
    };
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
