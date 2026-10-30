//! A viewer's own history as the assistant sees it: watched, started or favorite
//! movies and series (series through their episodes), restricted to what the viewer
//! may see.

use sqlx::{QueryBuilder, Sqlite};

use crate::browse::{ItemFilter, push_filters};
use crate::{Db, Result};

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct HistoryItem {
    pub id: i64,
    pub kind: String,
    pub title: String,
    pub year: Option<i64>,
    pub favorite: bool,
    pub played: bool,
    pub last_played_at: Option<i64>,
}

/// Most recent first, favorites included even if never played. `filter` carries the
/// viewer's restrictions (its `kinds` are ignored).
pub async fn recent(
    db: &Db,
    user_id: i64,
    filter: &ItemFilter,
    limit: i64,
) -> Result<Vec<HistoryItem>> {
    let mut movies = filter.clone();
    movies.kinds = vec!["movie".to_owned(), "series".to_owned()];
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT i.id, i.kind, i.title, i.year, ud.favorite, ud.played, ud.last_played_at
         FROM items i JOIN user_item_data ud ON ud.identity_key = i.identity_key AND ud.user_id = ",
    );
    qb.push_bind(user_id);
    push_filters(&mut qb, &movies);
    qb.push(" AND (ud.played = 1 OR ud.favorite = 1 OR ud.position_ms > 0)");
    qb.push(" ORDER BY ud.last_played_at IS NULL, ud.last_played_at DESC, i.id LIMIT ")
        .push_bind(limit);
    let mut rows: Vec<HistoryItem> = qb.build_query_as().fetch_all(db.reader()).await?;

    let mut series = filter.clone();
    series.kinds = vec!["series".to_owned()];
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT i.id, i.kind, i.title, i.year, 0 AS favorite, MAX(ud.played) AS played,
                MAX(ud.last_played_at) AS last_played_at
         FROM items i
         JOIN items e ON e.series_id = i.id AND e.kind = 'episode'
         JOIN user_item_data ud ON ud.identity_key = e.identity_key AND ud.user_id = ",
    );
    qb.push_bind(user_id);
    push_filters(&mut qb, &series);
    qb.push(" AND (ud.played = 1 OR ud.position_ms > 0) GROUP BY i.id");
    let from_episodes: Vec<HistoryItem> = qb.build_query_as().fetch_all(db.reader()).await?;
    for s in from_episodes {
        match rows.iter_mut().find(|r| r.id == s.id) {
            Some(existing) => {
                existing.played |= s.played;
                existing.last_played_at = existing.last_played_at.max(s.last_played_at);
            }
            None => rows.push(s),
        }
    }
    rows.sort_by_key(|r| (std::cmp::Reverse(r.last_played_at), r.id));
    rows.truncate(limit.max(0) as usize);
    Ok(rows)
}

/// People credited on an item: (person id, name), cast order first.
pub async fn people_of(db: &Db, item_id: i64, limit: i64) -> Result<Vec<(i64, String)>> {
    Ok(sqlx::query_as(
        "SELECT p.id, p.name FROM credits c JOIN people p ON p.id = c.person_id
         WHERE c.item_id = ? GROUP BY p.id ORDER BY MIN(c.ord), p.id LIMIT ?",
    )
    .bind(item_id)
    .bind(limit)
    .fetch_all(db.reader())
    .await?)
}
