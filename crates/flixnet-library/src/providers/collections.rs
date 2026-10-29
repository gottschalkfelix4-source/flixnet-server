//! Automatic collections from TMDB: a movie's `belongs_to_collection` becomes a
//! collection with all its parts, ordered by release date.

use flixnet_db::Db;
use flixnet_db::collections::{self, ProviderCollection};

use super::tmdb::{self, MovieDetails, Tmdb, TmdbError};
use crate::scan::sort_title;

/// Identity key a TMDB movie gets once matched (see `userdata::update_identity`).
pub fn movie_identity(tmdb_id: i64) -> String {
    format!("tmdb:movie:{tmdb_id}")
}

/// Puts the movie into its TMDB collection (creating or refreshing it), or takes it
/// out of automatic collections when TMDB lists none. Provider requests happen first;
/// the database is written in one short transaction afterwards.
pub async fn sync_movie(db: &Db, tmdb: &Tmdb, movie: &MovieDetails) -> anyhow::Result<()> {
    let member = movie_identity(movie.id);
    let Some(reference) = &movie.belongs_to_collection else {
        collections::leave_provider_collections(db, "tmdb", &member).await?;
        return Ok(());
    };
    let details = match tmdb.collection(reference.id).await {
        Ok(details) => Some(details),
        // Listed on the movie but gone at TMDB: keep what the movie tells us.
        Err(TmdbError::NotFound) => None,
        Err(err) => return Err(err.into()),
    };

    let mut parts: Vec<(Option<String>, String, i64)> = details
        .as_ref()
        .map(|d| {
            d.parts
                .iter()
                .map(|p| {
                    (
                        p.release_date.clone().filter(|r| !r.is_empty()),
                        p.title.clone(),
                        p.id,
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    // Release order; unreleased (undated) parts last.
    parts.sort_by(|a, b| (a.0.is_none(), &a.0, &a.1).cmp(&(b.0.is_none(), &b.0, &b.1)));

    let name = details
        .as_ref()
        .map(|d| d.name.trim().to_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| reference.name.trim().to_owned());
    if name.is_empty() {
        return Ok(());
    }
    let image = |own: Option<&Option<String>>, fallback: &Option<String>| {
        own.and_then(|p| p.as_deref())
            .or(fallback.as_deref())
            .filter(|p| !p.is_empty())
            .map(tmdb::image_url)
    };
    let collection = ProviderCollection {
        provider: "tmdb".to_owned(),
        provider_id: reference.id.to_string(),
        sort_name: sort_title(&name),
        name,
        overview: details
            .as_ref()
            .and_then(|d| d.overview.as_deref())
            .map(str::trim)
            .filter(|o| !o.is_empty())
            .map(str::to_owned),
        poster_url: image(
            details.as_ref().map(|d| &d.poster_path),
            &reference.poster_path,
        ),
        backdrop_url: image(
            details.as_ref().map(|d| &d.backdrop_path),
            &reference.backdrop_path,
        ),
        parts: parts
            .into_iter()
            .map(|(_, _, id)| movie_identity(id))
            .collect(),
    };
    collections::sync_provider(db, &collection, &member).await?;
    Ok(())
}
