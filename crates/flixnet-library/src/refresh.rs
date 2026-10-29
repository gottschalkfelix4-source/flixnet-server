//! Metadata refresh: local NFO files and artwork (providers follow in the next step).

use std::path::{Path, PathBuf};

use anyhow::Context;
use flixnet_db::metadata::{self, Authority, CreditInput, ItemFields, NewImage};
use flixnet_db::{Db, items, media};
use serde::Serialize;

use crate::artwork::{self, LocalArt};
use crate::nfo::{self, Nfo};
use crate::scan::sort_title;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LocalReport {
    pub nfo: Option<String>,
    pub fields: Vec<&'static str>,
    pub images: usize,
}

/// Where an item's files live, derived from one of its media files.
struct Location {
    /// The media file's folder.
    dir: PathBuf,
    /// The video file itself.
    file: PathBuf,
    /// Library-relative path of the file.
    relative: PathBuf,
    /// Library folder the file belongs to.
    root: PathBuf,
}

fn locate(file: &media::MediaFile) -> Location {
    let path = PathBuf::from(&file.path);
    let relative = PathBuf::from(&file.relative_path);
    // root = path minus the relative suffix
    let depth = relative.components().count();
    let mut root = path.clone();
    for _ in 0..depth {
        root.pop();
    }
    Location {
        dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
        file: path,
        relative,
        root,
    }
}

impl Location {
    /// First folder below the library root (series folder / movie folder), if any.
    fn top_folder(&self) -> Option<PathBuf> {
        let first = self.relative.components().next()?;
        (self.relative.components().count() > 1).then(|| self.root.join(first))
    }

    fn stem(&self) -> String {
        self.file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

async fn read_nfo(candidates: &[PathBuf]) -> Option<(PathBuf, Nfo)> {
    for path in candidates {
        if let Ok(bytes) = tokio::fs::read(path).await {
            let text = String::from_utf8_lossy(&bytes);
            if let Some(parsed) = nfo::parse(&text) {
                return Some((path.clone(), parsed));
            }
        }
    }
    None
}

/// Applies NFO data and local artwork to one item.
pub async fn refresh_local(db: &Db, item_id: i64) -> anyhow::Result<LocalReport> {
    let item = items::get(db, item_id)
        .await?
        .with_context(|| format!("item {item_id} does not exist"))?;
    let Some(file) = media::representative_file(db, item_id).await? else {
        return Ok(LocalReport::default()); // no present files (yet)
    };
    let loc = locate(&file);
    let stem = loc.stem();

    let (nfo_candidates, art): (Vec<PathBuf>, Vec<LocalArt>) = match item.kind.as_str() {
        "movie" => {
            // A movie directly in the library root must not pick up generic
            // folder.jpg/fanart.jpg that belong to the whole library.
            let generic_ok = loc.top_folder().is_some();
            let mut art = artwork::folder_art(&loc.dir, Some(&stem));
            if !generic_ok {
                art.retain(|a| {
                    a.path
                        .file_stem()
                        .is_some_and(|s| s.to_string_lossy().starts_with(&stem))
                });
            }
            let mut nfos = vec![loc.dir.join(format!("{stem}.nfo"))];
            if generic_ok {
                nfos.push(loc.dir.join("movie.nfo"));
            }
            (nfos, art)
        }
        "series" => match loc.top_folder() {
            Some(series_dir) => (
                vec![series_dir.join("tvshow.nfo")],
                artwork::folder_art(&series_dir, None),
            ),
            None => (Vec::new(), Vec::new()),
        },
        "season" => match loc.top_folder() {
            Some(series_dir) => {
                let season_dir = (loc.dir != series_dir).then_some(loc.dir.as_path());
                (
                    season_dir
                        .map(|d| vec![d.join("season.nfo")])
                        .unwrap_or_default(),
                    artwork::season_art(&series_dir, season_dir, item.index_number.unwrap_or(1)),
                )
            }
            None => (Vec::new(), Vec::new()),
        },
        _ => (
            vec![loc.dir.join(format!("{stem}.nfo"))],
            artwork::episode_art(&loc.file),
        ),
    };

    let mut report = LocalReport::default();
    if let Some((path, parsed)) = read_nfo(&nfo_candidates).await {
        report.nfo = Some(path.to_string_lossy().into_owned());
        report.fields = apply_nfo(db, item_id, &parsed).await?;
    }

    let images: Vec<NewImage> = art
        .into_iter()
        .map(|a| NewImage {
            kind: a.kind.to_owned(),
            path: Some(a.path.to_string_lossy().into_owned()),
            score: 10.0,
            ..Default::default()
        })
        .collect();
    report.images = images.len();
    metadata::replace_images(db, item_id, "local", &images).await?;
    metadata::mark_refreshed(db, item_id).await?;
    Ok(report)
}

async fn apply_nfo(db: &Db, item_id: i64, nfo: &Nfo) -> anyhow::Result<Vec<&'static str>> {
    let fields = ItemFields {
        title: nfo.title.clone(),
        original_title: nfo.original_title.clone(),
        sort_title: nfo
            .sort_title
            .as_deref()
            .or(nfo.title.as_deref())
            .map(sort_title),
        year: nfo.year.or_else(|| {
            nfo.premiered
                .as_deref()
                .and_then(|d| d.get(..4)?.parse().ok())
        }),
        overview: nfo.plot.clone(),
        tagline: nfo.tagline.clone(),
        runtime_ms: nfo.runtime_minutes.map(|m| m * 60_000),
        premiere_date: nfo.premiered.clone(),
        official_rating: nfo.mpaa.clone(),
        age_rating: nfo.mpaa.as_deref().and_then(nfo::age_from_certification),
        community_rating: nfo.rating,
    };
    let written = metadata::apply_fields(db, item_id, &fields, Authority::Locking).await?;

    for (provider, value) in [
        ("tmdb", &nfo.tmdb),
        ("imdb", &nfo.imdb),
        ("tvdb", &nfo.tvdb),
    ] {
        if let Some(value) = value {
            metadata::set_provider_id(db, item_id, provider, value).await?;
        }
    }
    if !nfo.genres.is_empty() {
        metadata::set_genres(db, item_id, &nfo.genres).await?;
    }
    if !nfo.studios.is_empty() {
        metadata::set_studios(db, item_id, &nfo.studios).await?;
    }

    let mut credits: Vec<CreditInput> = Vec::new();
    for (i, actor) in nfo.actors.iter().enumerate() {
        credits.push(CreditInput {
            name: actor.name.clone(),
            role: "actor".to_owned(),
            character: actor.role.clone(),
            order: actor.order.unwrap_or(i as i64),
            image_url: actor.thumb.clone().filter(|t| t.starts_with("http")),
            ..Default::default()
        });
    }
    for (role, names) in [("director", &nfo.directors), ("writer", &nfo.writers)] {
        for (i, name) in names.iter().enumerate() {
            credits.push(CreditInput {
                name: name.clone(),
                role: role.to_owned(),
                order: i as i64,
                ..Default::default()
            });
        }
    }
    // An NFO without people keeps whatever credits the item already has.
    if !credits.is_empty() {
        metadata::set_credits(db, item_id, &credits).await?;
    }
    Ok(written)
}
