//! Online metadata providers: matching items to provider entries and applying data.

pub mod fanart;
pub mod tmdb;

use flixnet_db::metadata::{self, Authority, CreditInput, ItemFields, NewImage};
use flixnet_db::{Db, items, settings};
use serde::{Deserialize, Serialize};

use crate::nfo::age_from_certification;
use crate::scan::{normalize_key, sort_title};
pub use fanart::Fanart;
pub use tmdb::{Tmdb, TmdbConfig, TmdbError};

/// Admin-editable provider settings (stored as JSON setting `metadata`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MetadataSettings {
    pub tmdb_api_key: Option<String>,
    /// Optional: fanart.tv for curated logos, clear art and banners.
    pub fanart_api_key: Option<String>,
    /// Metadata language, e.g. `de-DE`.
    pub language: Option<String>,
    /// Country for age ratings, e.g. `DE`.
    pub country: Option<String>,
}

pub const SETTINGS_KEY: &str = "metadata";

pub async fn load_settings(db: &Db) -> anyhow::Result<MetadataSettings> {
    Ok(settings::get(db, SETTINGS_KEY).await?.unwrap_or_default())
}

/// A TMDB client if an API key is configured.
pub async fn tmdb_from_settings(db: &Db) -> anyhow::Result<Option<Tmdb>> {
    let settings = load_settings(db).await?;
    Ok(settings
        .tmdb_api_key
        .filter(|k| !k.trim().is_empty())
        .map(|key| {
            let mut config = TmdbConfig::new(key.trim());
            if let Some(language) = settings.language.filter(|l| !l.is_empty()) {
                config.language = language;
            }
            if let Some(country) = settings.country.filter(|c| !c.is_empty()) {
                config.country = country;
            }
            Tmdb::new(config, db.clone())
        }))
}

/// A fanart.tv client if an API key is configured.
pub async fn fanart_from_settings(db: &Db) -> anyhow::Result<Option<Fanart>> {
    let settings = load_settings(db).await?;
    let language = settings
        .language
        .clone()
        .unwrap_or_else(|| "de-DE".to_owned());
    Ok(settings
        .fanart_api_key
        .filter(|k| !k.trim().is_empty())
        .map(|key| Fanart::new(key.trim(), &language, db.clone())))
}

/// Adds fanart.tv artwork to a matched movie (by TMDB id) or series (by TVDB id).
pub async fn apply_fanart(db: &Db, fanart: &Fanart, item_id: i64) -> anyhow::Result<usize> {
    let Some(item) = items::get(db, item_id).await? else {
        return Ok(0);
    };
    let ids = items::provider_ids(db, item_id).await?;
    let id_of = |p: &str| {
        ids.iter()
            .find(|(provider, _)| provider == p)
            .and_then(|(_, v)| v.parse::<i64>().ok())
    };
    let images = match item.kind.as_str() {
        "movie" => match id_of("tmdb") {
            Some(id) => fanart.movie_images(id).await?,
            None => return Ok(0),
        },
        "series" => match id_of("tvdb") {
            Some(id) => fanart.series_images(id).await?,
            None => return Ok(0),
        },
        _ => return Ok(0),
    };
    metadata::replace_images(db, item_id, "fanart", &images).await?;
    Ok(images.len())
}

// ---- Matching -------------------------------------------------------------------------

/// Automatic matches below this confidence are left for manual review.
pub const AUTO_MATCH_THRESHOLD: f64 = 0.7;

fn year_of(date: Option<&str>) -> Option<i64> {
    date.and_then(|d| d.get(..4)?.parse().ok())
}

/// 0..~1.05: title similarity (best of localized/original), year agreement, a hint of popularity.
pub fn score(
    title: &str,
    year: Option<i64>,
    cand_title: &str,
    cand_original: &str,
    cand_year: Option<i64>,
    popularity: f64,
) -> f64 {
    let query = normalize_key(title);
    let similarity = |candidate: &str| {
        let candidate = normalize_key(candidate);
        if candidate.is_empty() {
            return 0.0;
        }
        if candidate == query || sort_title(&candidate) == sort_title(&query) {
            1.0
        } else {
            strsim::normalized_levenshtein(&query, &candidate)
        }
    };
    let title_score = similarity(cand_title).max(similarity(cand_original));
    let year_score = match (year, cand_year) {
        (Some(a), Some(b)) if a == b => 0.2,
        (Some(a), Some(b)) if (a - b).abs() == 1 => 0.1,
        (Some(_), Some(_)) => -0.4,
        _ => 0.0,
    };
    title_score * 0.8 + year_score + (popularity.max(0.0) + 1.0).log10().min(3.0) / 60.0
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Candidate {
    pub provider: &'static str,
    pub id: i64,
    pub title: String,
    pub original_title: String,
    pub year: Option<i64>,
    pub overview: Option<String>,
    pub poster_url: Option<String>,
    pub score: f64,
}

pub async fn movie_candidates(
    tmdb: &Tmdb,
    title: &str,
    year: Option<i64>,
) -> Result<Vec<Candidate>, TmdbError> {
    let mut out: Vec<Candidate> = tmdb
        .search_movie(title, year)
        .await?
        .into_iter()
        .map(|r| {
            let cand_year = year_of(r.release_date.as_deref());
            Candidate {
                provider: "tmdb",
                score: score(
                    title,
                    year,
                    &r.title,
                    &r.original_title,
                    cand_year,
                    r.popularity,
                ),
                id: r.id,
                title: r.title,
                original_title: r.original_title,
                year: cand_year,
                overview: r.overview,
                poster_url: r.poster_path.as_deref().map(tmdb::image_url),
            }
        })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(out)
}

pub async fn series_candidates(
    tmdb: &Tmdb,
    title: &str,
    year: Option<i64>,
) -> Result<Vec<Candidate>, TmdbError> {
    let mut out: Vec<Candidate> = tmdb
        .search_tv(title, year)
        .await?
        .into_iter()
        .map(|r| {
            let cand_year = year_of(r.first_air_date.as_deref());
            Candidate {
                provider: "tmdb",
                score: score(
                    title,
                    year,
                    &r.name,
                    &r.original_name,
                    cand_year,
                    r.popularity,
                ),
                id: r.id,
                title: r.name,
                original_title: r.original_name,
                year: cand_year,
                overview: r.overview,
                poster_url: r.poster_path.as_deref().map(tmdb::image_url),
            }
        })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MatchOutcome {
    pub tmdb_id: Option<i64>,
    pub confidence: f64,
    pub state: &'static str,
}

/// Finds the TMDB entry for a movie or series: provider ids first, then search.
pub async fn match_item(db: &Db, tmdb: &Tmdb, item: &items::Item) -> anyhow::Result<MatchOutcome> {
    let ids = items::provider_ids(db, item.id).await?;
    let id_of = |p: &str| {
        ids.iter()
            .find(|(provider, _)| provider == p)
            .map(|(_, v)| v.clone())
    };
    let is_movie = item.kind == "movie";

    if let Some(id) = id_of("tmdb").and_then(|v| v.parse().ok()) {
        return Ok(MatchOutcome {
            tmdb_id: Some(id),
            confidence: 1.0,
            state: "matched",
        });
    }
    for (provider, source) in [("imdb", "imdb_id"), ("tvdb", "tvdb_id")] {
        if let Some(external) = id_of(provider) {
            let found = tmdb.find(&external, source).await?;
            let id = if is_movie {
                found.movie_results.first().map(|r| r.id)
            } else {
                found.tv_results.first().map(|r| r.id)
            };
            if let Some(id) = id {
                return Ok(MatchOutcome {
                    tmdb_id: Some(id),
                    confidence: 1.0,
                    state: "matched",
                });
            }
        }
    }

    let candidates = if is_movie {
        movie_candidates(tmdb, &item.title, item.year).await?
    } else {
        series_candidates(tmdb, &item.title, item.year).await?
    };
    Ok(match candidates.first() {
        Some(best) if best.score >= AUTO_MATCH_THRESHOLD => MatchOutcome {
            tmdb_id: Some(best.id),
            confidence: best.score.min(1.0),
            state: "matched",
        },
        best => MatchOutcome {
            tmdb_id: None,
            confidence: best.map_or(0.0, |b| b.score.max(0.0)),
            state: "failed",
        },
    })
}

// ---- Applying provider data -----------------------------------------------------------------

fn non_empty(text: Option<String>) -> Option<String> {
    text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

/// Language-aware image ranking: metadata language > English > textless > others.
fn images(list: &[tmdb::ImageInfo], kind: &str, language: &str, keep: usize) -> Vec<NewImage> {
    let primary = language.split('-').next().unwrap_or("de");
    let weight = |lang: Option<&str>| match lang {
        Some(l) if l == primary => 3.0,
        // Logos/posters carry text: English beats textless; backdrops prefer textless.
        Some("en") if kind != "backdrop" => 2.0,
        None if kind == "backdrop" => 2.5,
        None => 1.0,
        Some("en") => 1.5,
        _ => 0.0,
    };
    let mut ranked: Vec<NewImage> = list
        .iter()
        .map(|i| NewImage {
            kind: kind.to_owned(),
            url: Some(tmdb::image_url(&i.file_path)),
            language: i.iso_639_1.clone(),
            width: i.width,
            height: i.height,
            score: weight(i.iso_639_1.as_deref()) * 10.0 + i.vote_average,
            ..Default::default()
        })
        .collect();
    ranked.sort_by(|a, b| b.score.total_cmp(&a.score));
    ranked.truncate(keep);
    ranked
}

fn certification_movie(details: &tmdb::MovieDetails, country: &str) -> Option<String> {
    [country, "US"].iter().find_map(|c| {
        details
            .release_dates
            .results
            .iter()
            .find(|r| r.iso_3166_1 == *c)?
            .release_dates
            .iter()
            .map(|d| d.certification.trim())
            .find(|c| !c.is_empty())
            .map(|cert| {
                if *c == "DE" {
                    format!("FSK {cert}")
                } else {
                    cert.to_owned()
                }
            })
    })
}

fn certification_tv(details: &tmdb::TvDetails, country: &str) -> Option<String> {
    [country, "US"].iter().find_map(|c| {
        details
            .content_ratings
            .results
            .iter()
            .find(|r| r.iso_3166_1 == *c && !r.rating.trim().is_empty())
            .map(|r| {
                if *c == "DE" {
                    format!("FSK {}", r.rating.trim())
                } else {
                    r.rating.trim().to_owned()
                }
            })
    })
}

fn profile(path: &Option<String>) -> Option<String> {
    path.as_deref().map(tmdb::image_url)
}

pub async fn apply_movie(db: &Db, tmdb: &Tmdb, item_id: i64, tmdb_id: i64) -> anyhow::Result<()> {
    let d = tmdb.movie(tmdb_id).await?;
    let config = tmdb.config();
    let rating = certification_movie(&d, &config.country);
    let fields = ItemFields {
        title: non_empty(Some(d.title.clone())),
        original_title: non_empty(Some(d.original_title.clone())),
        sort_title: non_empty(Some(sort_title(&d.title))),
        year: year_of(d.release_date.as_deref()),
        overview: non_empty(d.overview.clone()),
        tagline: non_empty(d.tagline.clone()),
        runtime_ms: d.runtime.filter(|r| *r > 0).map(|m| m * 60_000),
        premiere_date: non_empty(d.release_date.clone()),
        age_rating: rating.as_deref().and_then(age_from_certification),
        official_rating: rating,
        community_rating: d.vote_average.filter(|v| *v > 0.0),
    };
    metadata::apply_fields(db, item_id, &fields, Authority::Provider).await?;

    metadata::set_provider_id(db, item_id, "tmdb", &d.id.to_string()).await?;
    if let Some(imdb) = d
        .imdb_id
        .clone()
        .or(d.external_ids.imdb_id.clone())
        .filter(|i| !i.is_empty())
    {
        metadata::set_provider_id(db, item_id, "imdb", &imdb).await?;
    }
    set_lists_if_unset(db, item_id, &d.genres, &d.production_companies).await?;

    let mut credits = cast_credits(&d.credits.cast);
    for (i, member) in d
        .credits
        .crew
        .iter()
        .filter(|c| matches!(c.job.as_str(), "Director" | "Screenplay" | "Writer"))
        .enumerate()
    {
        credits.push(CreditInput {
            name: member.name.clone(),
            role: if member.job == "Director" {
                "director"
            } else {
                "writer"
            }
            .to_owned(),
            order: i as i64,
            tmdb_id: Some(member.id),
            image_url: profile(&member.profile_path),
            ..Default::default()
        });
    }
    set_credits_if_unset(db, item_id, &credits).await?;

    let mut art = images(&d.images.posters, "poster", &config.language, 5);
    art.extend(images(&d.images.backdrops, "backdrop", &config.language, 5));
    art.extend(images(&d.images.logos, "logo", &config.language, 3));
    metadata::replace_images(db, item_id, "tmdb", &art).await?;
    Ok(())
}

pub async fn apply_series(
    db: &Db,
    tmdb: &Tmdb,
    series_id: i64,
    tmdb_id: i64,
) -> anyhow::Result<()> {
    let d = tmdb.tv(tmdb_id).await?;
    let config = tmdb.config();
    let rating = certification_tv(&d, &config.country);
    let fields = ItemFields {
        title: non_empty(Some(d.name.clone())),
        original_title: non_empty(Some(d.original_name.clone())),
        sort_title: non_empty(Some(sort_title(&d.name))),
        year: year_of(d.first_air_date.as_deref()),
        overview: non_empty(d.overview.clone()),
        tagline: non_empty(d.tagline.clone()),
        runtime_ms: d
            .episode_run_time
            .first()
            .filter(|r| **r > 0)
            .map(|m| m * 60_000),
        premiere_date: non_empty(d.first_air_date.clone()),
        age_rating: rating.as_deref().and_then(age_from_certification),
        official_rating: rating,
        community_rating: d.vote_average.filter(|v| *v > 0.0),
    };
    metadata::apply_fields(db, series_id, &fields, Authority::Provider).await?;
    metadata::set_provider_id(db, series_id, "tmdb", &d.id.to_string()).await?;
    if let Some(imdb) = d.external_ids.imdb_id.clone().filter(|i| !i.is_empty()) {
        metadata::set_provider_id(db, series_id, "imdb", &imdb).await?;
    }
    if let Some(tvdb) = d.external_ids.tvdb_id {
        metadata::set_provider_id(db, series_id, "tvdb", &tvdb.to_string()).await?;
    }
    set_lists_if_unset(db, series_id, &d.genres, &d.networks).await?;

    let mut credits = cast_credits(&d.aggregate_credits.cast);
    for (i, creator) in d.created_by.iter().enumerate() {
        credits.push(CreditInput {
            name: creator.name.clone(),
            role: "creator".to_owned(),
            order: i as i64,
            tmdb_id: Some(creator.id),
            image_url: profile(&creator.profile_path),
            ..Default::default()
        });
    }
    set_credits_if_unset(db, series_id, &credits).await?;

    let mut art = images(&d.images.posters, "poster", &config.language, 5);
    art.extend(images(&d.images.backdrops, "backdrop", &config.language, 5));
    art.extend(images(&d.images.logos, "logo", &config.language, 3));
    metadata::replace_images(db, series_id, "tmdb", &art).await?;

    // Seasons and episodes present in the library.
    for season in items::children(db, series_id)
        .await?
        .into_iter()
        .filter(|c| c.kind == "season")
    {
        let Some(number) = season.index_number else {
            continue;
        };
        let details = match tmdb.season(tmdb_id, number).await {
            Ok(details) => details,
            Err(TmdbError::NotFound) => continue,
            Err(err) => return Err(err.into()),
        };
        let season_fields = ItemFields {
            title: non_empty(details.name.clone()),
            overview: non_empty(details.overview.clone()),
            premiere_date: non_empty(details.air_date.clone()),
            year: year_of(details.air_date.as_deref()),
            ..Default::default()
        };
        metadata::apply_fields(db, season.id, &season_fields, Authority::Provider).await?;
        if let Some(poster) = &details.poster_path {
            metadata::replace_images(
                db,
                season.id,
                "tmdb",
                &[NewImage {
                    kind: "poster".into(),
                    url: Some(tmdb::image_url(poster)),
                    score: 30.0,
                    ..Default::default()
                }],
            )
            .await?;
        }
        for episode in items::children(db, season.id)
            .await?
            .into_iter()
            .filter(|c| c.kind == "episode")
        {
            let Some(number) = episode.index_number else {
                continue;
            };
            let Some(ep) = details.episodes.iter().find(|e| e.episode_number == number) else {
                continue;
            };
            let fields = ItemFields {
                title: non_empty(ep.name.clone()),
                overview: non_empty(ep.overview.clone()),
                premiere_date: non_empty(ep.air_date.clone()),
                year: year_of(ep.air_date.as_deref()),
                runtime_ms: ep.runtime.filter(|r| *r > 0).map(|m| m * 60_000),
                community_rating: ep.vote_average.filter(|v| *v > 0.0),
                ..Default::default()
            };
            metadata::apply_fields(db, episode.id, &fields, Authority::Provider).await?;
            if let Some(still) = &ep.still_path {
                metadata::replace_images(
                    db,
                    episode.id,
                    "tmdb",
                    &[NewImage {
                        kind: "thumb".into(),
                        url: Some(tmdb::image_url(still)),
                        score: 30.0,
                        ..Default::default()
                    }],
                )
                .await?;
            }
        }
    }
    Ok(())
}

fn cast_credits(cast: &[tmdb::CastMember]) -> Vec<CreditInput> {
    cast.iter()
        .take(20)
        .map(|member| CreditInput {
            name: member.name.clone(),
            role: "actor".to_owned(),
            character: member
                .character
                .clone()
                .or_else(|| member.roles.first().and_then(|r| r.character.clone()))
                .filter(|c| !c.is_empty()),
            order: member.order,
            tmdb_id: Some(member.id),
            image_url: profile(&member.profile_path),
            ..Default::default()
        })
        .collect()
}

/// Provider genres/studios only when the item has none (an NFO list wins).
async fn set_lists_if_unset(
    db: &Db,
    item_id: i64,
    genres: &[tmdb::Named],
    studios: &[tmdb::Named],
) -> anyhow::Result<()> {
    if metadata::genres(db, item_id).await?.is_empty() {
        let names: Vec<String> = genres.iter().map(|g| g.name.clone()).collect();
        metadata::set_genres(db, item_id, &names).await?;
    }
    if metadata::studios(db, item_id).await?.is_empty() {
        let names: Vec<String> = studios.iter().map(|s| s.name.clone()).collect();
        metadata::set_studios(db, item_id, &names).await?;
    }
    Ok(())
}

async fn set_credits_if_unset(
    db: &Db,
    item_id: i64,
    credits: &[CreditInput],
) -> anyhow::Result<()> {
    if metadata::credits(db, item_id).await?.is_empty() && !credits.is_empty() {
        metadata::set_credits(db, item_id, credits).await?;
    }
    Ok(())
}

/// Matches (unless manual) and applies provider metadata to a movie or series.
pub async fn refresh_remote(
    db: &Db,
    tmdb: &Tmdb,
    item_id: i64,
) -> anyhow::Result<Option<MatchOutcome>> {
    let Some(item) = items::get(db, item_id).await? else {
        return Ok(None);
    };
    if item.kind != "movie" && item.kind != "series" {
        return Ok(None);
    }
    let outcome = match_item(db, tmdb, &item).await?;
    items::set_match(db, item_id, outcome.state, Some(outcome.confidence)).await?;
    if let Some(tmdb_id) = outcome.tmdb_id {
        if item.kind == "movie" {
            apply_movie(db, tmdb, item_id, tmdb_id).await?;
        } else {
            apply_series(db, tmdb, item_id, tmdb_id).await?;
        }
    }
    Ok(Some(outcome))
}

/// Manual identification: pins a TMDB id and applies its data.
pub async fn identify(db: &Db, tmdb: &Tmdb, item_id: i64, tmdb_id: i64) -> anyhow::Result<()> {
    let item = items::get(db, item_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("item not found"))?;
    metadata::set_provider_id(db, item_id, "tmdb", &tmdb_id.to_string()).await?;
    items::set_match(db, item_id, "manual", Some(1.0)).await?;
    if item.kind == "movie" {
        apply_movie(db, tmdb, item_id, tmdb_id).await
    } else {
        apply_series(db, tmdb, item_id, tmdb_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoring_prefers_title_and_year_matches() {
        let exact = score(
            "Der Pate",
            Some(1972),
            "Der Pate",
            "The Godfather",
            Some(1972),
            80.0,
        );
        let original = score(
            "The Godfather",
            Some(1972),
            "Der Pate",
            "The Godfather",
            Some(1972),
            80.0,
        );
        let sequel = score(
            "Der Pate",
            Some(1972),
            "Der Pate 2",
            "The Godfather Part II",
            Some(1974),
            60.0,
        );
        let wrong_year = score(
            "Der Pate",
            Some(1990),
            "Der Pate",
            "The Godfather",
            Some(1972),
            80.0,
        );
        assert!(exact >= AUTO_MATCH_THRESHOLD && original >= AUTO_MATCH_THRESHOLD);
        assert!(exact > sequel);
        assert!(sequel < AUTO_MATCH_THRESHOLD || sequel < exact - 0.2);
        assert!(wrong_year < exact);
        // Articles and punctuation do not matter.
        assert!(
            score(
                "Matrix",
                Some(1999),
                "The Matrix",
                "The Matrix",
                Some(1999),
                50.0
            ) >= AUTO_MATCH_THRESHOLD
        );
    }
}
