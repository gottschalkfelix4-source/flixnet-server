//! Local artwork files next to the media (Kodi/Plex/Jellyfin naming).

use std::path::{Path, PathBuf};

/// One artwork file and what it depicts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalArt {
    pub kind: &'static str,
    pub path: PathBuf,
}

const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "tbn"];

/// Base names per kind, most specific first.
const KINDS: &[(&str, &[&str])] = &[
    ("poster", &["poster", "folder", "cover", "default", "movie"]),
    ("backdrop", &["fanart", "backdrop", "background", "art"]),
    ("logo", &["clearlogo", "logo"]),
    ("banner", &["banner"]),
    ("thumb", &["landscape", "thumb"]),
];

fn find_image(dir: &Path, stem: &str) -> Option<PathBuf> {
    IMAGE_EXTENSIONS.iter().find_map(|ext| {
        let candidate = dir.join(format!("{stem}.{ext}"));
        candidate.is_file().then_some(candidate)
    })
}

/// Artwork for a movie (or series) folder. `media_stem` adds `<name>-poster.jpg` styles.
pub fn folder_art(dir: &Path, media_stem: Option<&str>) -> Vec<LocalArt> {
    let mut found = Vec::new();
    for (kind, names) in KINDS {
        let specific = media_stem
            .into_iter()
            .flat_map(|stem| names.iter().map(move |n| format!("{stem}-{n}")));
        let generic = names.iter().map(|n| (*n).to_owned());
        if let Some(path) = specific
            .chain(generic)
            .find_map(|stem| find_image(dir, &stem))
        {
            found.push(LocalArt { kind, path });
        }
    }
    found
}

/// Season artwork: `season01-poster.jpg` / `season-specials-poster.jpg` in the series
/// folder, or a poster inside the season's own folder.
pub fn season_art(series_dir: &Path, season_dir: Option<&Path>, season: i64) -> Vec<LocalArt> {
    let prefix = if season == 0 {
        "season-specials".to_owned()
    } else {
        format!("season{season:02}")
    };
    let mut found = Vec::new();
    for (kind, suffix) in [
        ("poster", "poster"),
        ("backdrop", "fanart"),
        ("banner", "banner"),
        ("thumb", "landscape"),
    ] {
        if let Some(path) = find_image(series_dir, &format!("{prefix}-{suffix}")) {
            found.push(LocalArt { kind, path });
        }
    }
    if !found.iter().any(|a| a.kind == "poster")
        && let Some(dir) = season_dir
        && let Some(path) = ["poster", "folder", "cover"]
            .iter()
            .find_map(|n| find_image(dir, n))
    {
        found.push(LocalArt {
            kind: "poster",
            path,
        });
    }
    found
}

/// Episode still: `<episode>-thumb.jpg` or `<episode>.jpg` next to the file.
pub fn episode_art(video: &Path) -> Vec<LocalArt> {
    let (Some(dir), Some(stem)) = (video.parent(), video.file_stem()) else {
        return Vec::new();
    };
    let stem = stem.to_string_lossy();
    [format!("{stem}-thumb"), stem.to_string()]
        .iter()
        .find_map(|name| find_image(dir, name))
        .map(|path| {
            vec![LocalArt {
                kind: "thumb",
                path,
            }]
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_kodi_and_plex_style_names() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "Movie (2020)-poster.jpg",
            "folder.jpg",
            "fanart.png",
            "clearlogo.png",
        ] {
            std::fs::write(dir.path().join(name), b"img").unwrap();
        }
        let art = folder_art(dir.path(), Some("Movie (2020)"));
        let kinds: Vec<_> = art
            .iter()
            .map(|a| {
                (
                    a.kind,
                    a.path.file_name().unwrap().to_string_lossy().into_owned(),
                )
            })
            .collect();
        assert_eq!(
            kinds,
            [
                ("poster", "Movie (2020)-poster.jpg".to_owned()),
                ("backdrop", "fanart.png".to_owned()),
                ("logo", "clearlogo.png".to_owned()),
            ]
        );
    }

    #[test]
    fn season_and_episode_art() {
        let series = tempfile::tempdir().unwrap();
        let season_dir = series.path().join("Staffel 2");
        std::fs::create_dir_all(&season_dir).unwrap();
        std::fs::write(series.path().join("season01-poster.jpg"), b"img").unwrap();
        std::fs::write(series.path().join("season-specials-poster.png"), b"img").unwrap();
        std::fs::write(season_dir.join("poster.jpg"), b"img").unwrap();
        std::fs::write(season_dir.join("Show.S02E01-thumb.jpg"), b"img").unwrap();

        assert_eq!(season_art(series.path(), None, 1)[0].kind, "poster");
        assert_eq!(season_art(series.path(), None, 0).len(), 1);
        let s2 = season_art(series.path(), Some(&season_dir), 2);
        assert!(
            s2[0].path.ends_with("Staffel 2/poster.jpg")
                || s2[0].path.ends_with("Staffel 2\\poster.jpg")
        );
        assert_eq!(episode_art(&season_dir.join("Show.S02E01.mkv")).len(), 1);
        assert!(episode_art(&season_dir.join("Show.S02E02.mkv")).is_empty());
    }
}
