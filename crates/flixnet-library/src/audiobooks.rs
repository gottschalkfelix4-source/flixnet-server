//! Audiobook libraries: one book per folder (or per `.m4b` file), its audio files are
//! the parts in path order.
//!
//! Folder names give a first title, author and series (`Autor/Reihe/01 - Titel`);
//! a background job then probes the parts (durations, embedded chapters) and reads
//! their tags (album, artist, narrator, series), which win over the folder names.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use flixnet_core::jobs::{JobContext, JobHandler, JobOptions, JobQueue, Pool, priority};
use flixnet_db::audiobooks::{self, BookMeta};
use flixnet_db::metadata::{self, NewImage};
use flixnet_db::{Db, DbError, items, libraries, media};
use flixnet_media::Ffprobe;
use flixnet_media::tags::Tags;
use flixnet_media::trickplay::{VideoSource, chapter_image_path, generate_chapter_images};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::assets::{self, extension};
use crate::scan::{MISSING_GRACE_MS, ScanReport, sort_title};

pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "m4a", "m4b", "aac", "ogg", "oga", "opus", "flac", "wav",
];
const BATCH: usize = 200;
/// Cover pictures next to the audio files, best first.
const COVER_NAMES: &[&str] = &["cover", "folder", "front", "poster"];

pub fn is_audio_file(path: &Path) -> bool {
    extension(path).is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.as_str()))
}

// ---- Folder names -------------------------------------------------------------------

/// What the path of a file says about its book.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BookSpec {
    /// Stable identity within the library (the book folder, or the m4b file).
    pub key: String,
    pub title: String,
    pub author: Option<String>,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub year: Option<i64>,
}

static DISC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(cd|disc|disk|teil|part|tape)[\s._-]*\d+$").expect("valid regex")
});
static INDEXED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:(?:band|teil|folge|buch|book|vol\.?|volume)\s*)?(\d{1,3}(?:[.,]\d)?)\s*[-–._:]\s*(.+)$",
    )
    .expect("valid regex")
});
static YEAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*[(\[](\d{4})[)\]]\s*$").expect("valid regex"));

/// `Titel (2019)` → (`Titel`, 2019).
fn strip_year(name: &str) -> (String, Option<i64>) {
    match YEAR.captures(name) {
        Some(c) => (
            name[..c.get(0).map_or(name.len(), |m| m.start())]
                .trim()
                .to_owned(),
            c[1].parse().ok(),
        ),
        None => (name.trim().to_owned(), None),
    }
}

/// A book folder or file name: `01 - Titel`, `Band 2 - Titel`, `Autor - Titel`,
/// `Titel (2019)`.
fn parse_name(name: &str) -> (String, Option<String>, Option<f64>, Option<i64>) {
    let (name, year) = strip_year(&name.replace('_', " "));
    if let Some(c) = INDEXED.captures(&name) {
        let index = c[1].replace(',', ".").parse().ok();
        return (c[2].trim().to_owned(), None, index, year);
    }
    if let Some((author, title)) = name.split_once(" - ")
        && !author.trim().is_empty()
        && !title.trim().is_empty()
    {
        return (
            title.trim().to_owned(),
            Some(author.trim().to_owned()),
            None,
            year,
        );
    }
    (name, None, None, year)
}

/// The book a file belongs to, from its path relative to the library folder.
///
/// * `Buch.m4b`, `Autor/Buch.m4b`: every m4b file is a book of its own.
/// * `Autor/Titel/01.mp3`, `Autor/Reihe/02 - Titel/CD1/01.mp3`, `Titel/01.mp3`.
pub fn book_spec(relative: &Path) -> BookSpec {
    let mut folders: Vec<String> = relative
        .parent()
        .map(|p| {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let stem = relative
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let single_file = folders.is_empty() || extension(relative).as_deref() == Some("m4b");
    if single_file {
        let (title, author, index, year) = parse_name(&stem);
        let key = relative
            .with_extension("")
            .to_string_lossy()
            .replace('\\', "/");
        let mut spec = BookSpec {
            key,
            title,
            author: author.or_else(|| folders.last().cloned()),
            series: None,
            series_index: index,
            year,
        };
        if folders.len() >= 2 && spec.series_index.is_some() {
            // `Autor/Reihe/03 - Titel.m4b`
            spec.series = folders.last().cloned();
            spec.author = Some(folders[folders.len() - 2].clone());
        }
        return spec;
    }
    while folders.len() > 1 && folders.last().is_some_and(|f| DISC.is_match(f)) {
        folders.pop();
    }
    let book = folders.last().cloned().unwrap_or_default();
    let (title, author, index, year) = parse_name(&book);
    let mut spec = BookSpec {
        key: folders.join("/"),
        title,
        author,
        series: None,
        series_index: index,
        year,
    };
    let parents = &folders[..folders.len() - 1];
    match parents {
        [] => {}
        [parent] if spec.series_index.is_some() => spec.series = Some(parent.clone()),
        [parent] => {
            spec.author.get_or_insert_with(|| parent.clone());
        }
        [.., author, series] => {
            spec.author.get_or_insert_with(|| author.clone());
            spec.series = Some(series.clone());
        }
    }
    spec
}

// ---- Tags ---------------------------------------------------------------------------

fn first<'a>(tags: &'a Tags, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| tags.get(*k))
        .map(String::as_str)
        .filter(|v| !v.is_empty())
}

/// Details from the tags of a book's first part; folder names fill the gaps.
pub fn merge_tags(spec: &BookMeta, tags: &Tags, single_file: bool) -> (BookMeta, Option<String>) {
    let title = first(tags, &["album"])
        .or_else(|| single_file.then(|| first(tags, &["title"])).flatten())
        .map(str::to_owned);
    let meta = BookMeta {
        author: first(tags, &["album_artist", "artist", "author", "writer"])
            .map(str::to_owned)
            .or_else(|| spec.author.clone()),
        narrator: first(tags, &["narrator", "narratedby", "composer", "performer"])
            .map(str::to_owned)
            .or_else(|| spec.narrator.clone()),
        series_name: first(tags, &["series", "mvnm", "show"])
            .map(str::to_owned)
            .or_else(|| spec.series_name.clone()),
        series_index: first(tags, &["series-part", "series_part", "mvin"])
            .and_then(|v| v.split('/').next())
            .and_then(|v| v.trim().replace(',', ".").parse().ok())
            .or(spec.series_index),
    };
    (meta, title)
}

/// The year in a `date`/`year` tag (`2019`, `2019-05-02`).
fn tag_year(tags: &Tags) -> Option<i64> {
    let value = first(tags, &["date", "year", "originaldate"])?;
    value
        .get(..4)
        .and_then(|y| y.parse().ok())
        .filter(|y| (1000..=2999).contains(y))
}

// ---- Chapters -----------------------------------------------------------------------

/// One part of a book as the chapter list needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct PartInfo {
    pub file_id: i64,
    pub duration_ms: i64,
    /// The part's title (tag or file name).
    pub title: String,
    /// Chapters embedded in the file: (start, end, title), relative to the file.
    pub chapters: Vec<(i64, i64, Option<String>)>,
}

/// A chapter of the whole book.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BookChapter {
    pub title: String,
    /// Position in the whole book.
    pub start_ms: i64,
    pub end_ms: i64,
    /// Index of the part (in `parts`) the chapter starts in.
    pub part: usize,
}

/// Chapters across all parts: a part's embedded chapters (m4b) shifted to the book
/// position, otherwise the part itself is a chapter. Returns them with the start of
/// each part and the total duration.
pub fn book_chapters(parts: &[PartInfo]) -> (Vec<BookChapter>, Vec<i64>, i64) {
    let mut chapters = Vec::new();
    let mut offsets = Vec::with_capacity(parts.len());
    let mut offset = 0;
    for (index, part) in parts.iter().enumerate() {
        offsets.push(offset);
        let duration = part.duration_ms.max(0);
        let embedded: Vec<_> = part
            .chapters
            .iter()
            .filter(|(start, end, _)| *end > *start && (duration == 0 || *start < duration))
            .collect();
        if embedded.is_empty() {
            chapters.push(BookChapter {
                title: part.title.clone(),
                start_ms: offset,
                end_ms: offset + duration,
                part: index,
            });
        } else {
            for (start, end, title) in embedded {
                let end = if duration > 0 {
                    (*end).min(duration)
                } else {
                    *end
                };
                chapters.push(BookChapter {
                    title: title
                        .clone()
                        .filter(|t| !t.trim().is_empty())
                        .unwrap_or_else(|| format!("Kapitel {}", chapters.len() + 1)),
                    start_ms: offset + start,
                    end_ms: offset + end,
                    part: index,
                });
            }
        }
        offset += duration;
    }
    (chapters, offsets, offset)
}

// ---- Scan ---------------------------------------------------------------------------

/// Scans an audiobook library: books from folders and m4b files.
pub async fn scan_audiobooks(
    db: &Db,
    queue: &JobQueue,
    library_id: i64,
) -> anyhow::Result<ScanReport> {
    let roots = libraries::paths(db, library_id).await?;
    let diff = assets::diff(db, library_id, &roots, is_audio_file).await?;
    let mut report = ScanReport {
        unchanged: diff.unchanged,
        unreachable: diff.unreachable.clone(),
        ..Default::default()
    };
    let mut books: BTreeMap<String, i64> = BTreeMap::new();
    for chunk in diff.new_files.chunks(BATCH) {
        let mut tx = db.writer().begin().await?;
        for file in chunk {
            let spec = book_spec(&file.relative);
            let book_id = match books.get(&spec.key) {
                Some(id) => *id,
                None => {
                    let id = items::upsert(
                        &mut tx,
                        &items::NewItem {
                            library_id,
                            kind: "audiobook",
                            parent_id: None,
                            series_id: None,
                            title: &spec.title,
                            sort_title: &sort_title(&spec.title),
                            year: spec.year,
                            index_number: None,
                            index_end: None,
                            parent_index_number: None,
                            premiere_date: None,
                            content_key: &format!("book:{}", spec.key),
                        },
                    )
                    .await?;
                    audiobooks::init_meta(
                        &mut tx,
                        id,
                        &BookMeta {
                            author: spec.author.clone(),
                            narrator: None,
                            series_name: spec.series.clone(),
                            series_index: spec.series_index,
                        },
                    )
                    .await?;
                    books.insert(spec.key.clone(), id);
                    id
                }
            };
            let path = file.path.to_string_lossy();
            let relative = file.relative.to_string_lossy().replace('\\', "/");
            media::insert_file(
                &mut tx,
                &media::NewFile {
                    library_id,
                    item_id: Some(book_id),
                    path: &path,
                    relative_path: &relative,
                    size: file.size as i64,
                    mtime: file.mtime_ms,
                    role: "main",
                    extra_kind: None,
                    part: None,
                    parsed_json: None,
                },
            )
            .await?;
            report.added += 1;
        }
        for id in books.values() {
            audiobooks::renumber_parts(&mut tx, *id).await?;
        }
        tx.commit().await?;
    }
    let mut touched: Vec<i64> = books.values().copied().collect();
    if !diff.changed.is_empty() {
        let mut tx = db.writer().begin().await?;
        for (file_id, item_id, file) in &diff.changed {
            media::mark_changed(&mut tx, *file_id, file.size as i64, file.mtime_ms).await?;
            touched.extend(item_id);
            report.changed += 1;
        }
        tx.commit().await?;
    }
    assets::apply_presence(db, &diff).await?;
    report.restored = diff.restored.len();
    report.missing = diff.missing.len();

    let now = flixnet_db::now_ms();
    let removed_files =
        media::delete_missing_before(db, library_id, now - MISSING_GRACE_MS).await?;
    if removed_files > 0 || report.changed > 0 {
        report.removed = audiobooks::prune(db, library_id).await?;
    }
    touched.sort_unstable();
    touched.dedup();
    for book in touched {
        if queue_analyze(queue, book).await?.is_some() {
            report.probes_queued += 1;
        }
    }
    libraries::mark_scanned(db, library_id).await?;
    tracing::info!(library = library_id, ?report, "audiobook library scanned");
    Ok(report)
}

// ---- Analysis -----------------------------------------------------------------------

/// Probes the parts of a book and applies their tags and cover.
pub struct AnalyzeBook {
    pub ffprobe: Ffprobe,
    pub ffmpeg: PathBuf,
    /// `<config_dir>/cache`.
    pub cache_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookPayload {
    pub item_id: i64,
}

pub async fn queue_analyze(queue: &JobQueue, item_id: i64) -> Result<Option<i64>, DbError> {
    queue
        .enqueue::<AnalyzeBook>(
            &BookPayload { item_id },
            JobOptions::priority(priority::NEW).dedupe(format!("audiobook:{item_id}")),
        )
        .await
}

impl JobHandler for AnalyzeBook {
    const KIND: &'static str = "audiobooks.analyze";
    const POOL: Pool = Pool::Probe;
    type Payload = BookPayload;

    async fn run(&self, ctx: JobContext, payload: BookPayload) -> anyhow::Result<()> {
        analyze_book(
            ctx.db(),
            &self.ffprobe,
            &self.ffmpeg,
            &self.cache_dir,
            payload.item_id,
            &ctx.cancel_token(),
        )
        .await
    }
}

/// Files of a book in playback order (present main files).
pub async fn parts(db: &Db, item_id: i64) -> anyhow::Result<Vec<media::MediaFile>> {
    let mut files: Vec<media::MediaFile> = media::files_for_item(db, item_id)
        .await?
        .into_iter()
        .filter(|f| f.missing_since.is_none())
        .collect();
    files.sort_by(|a, b| {
        (a.part.unwrap_or(i64::MAX), &a.relative_path)
            .cmp(&(b.part.unwrap_or(i64::MAX), &b.relative_path))
    });
    Ok(files)
}

/// Probe → tags → details → cover. ffprobe and ffmpeg run before each short write.
pub async fn analyze_book(
    db: &Db,
    ffprobe: &Ffprobe,
    ffmpeg: &Path,
    cache_dir: &Path,
    item_id: i64,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    let Some(item) = items::get(db, item_id).await? else {
        return Ok(()); // removed meanwhile
    };
    let files = parts(db, item_id).await?;
    for file in &files {
        if file.probed_at.is_none() {
            crate::jobs::probe_file(db, ffprobe, file.id).await?;
        }
    }
    let files = parts(db, item_id).await?;
    let Some(first_file) = files.first() else {
        return Ok(());
    };
    let tags = match ffprobe.tags(Path::new(&first_file.path)).await {
        Ok(tags) => tags,
        Err(err) => {
            tracing::warn!(book = item_id, %err, "reading audiobook tags failed");
            Tags::new()
        }
    };
    let single_file = files.len() == 1;
    let current = audiobooks::meta(db, item_id).await?;
    let (meta, title) = merge_tags(&current, &tags, single_file);
    audiobooks::set_meta(db, item_id, &meta).await?;
    let runtime: i64 = files.iter().filter_map(|f| f.duration_ms).sum();
    let title = title.filter(|t| *t != item.title);
    let sort = title.as_deref().map(sort_title);
    let overview = first(&tags, &["description", "synopsis", "comment", "desc"]).map(str::to_owned);
    audiobooks::set_details(
        db,
        item_id,
        title.as_deref(),
        sort.as_deref(),
        tag_year(&tags),
        overview.as_deref(),
        (runtime > 0).then_some(runtime),
    )
    .await?;

    // Cover: a picture in the book folder, else the one embedded in the first part.
    let folder = Path::new(&first_file.path).parent().map(Path::to_path_buf);
    let cover = match folder {
        Some(folder) => tokio::task::spawn_blocking(move || folder_cover(&folder)).await?,
        None => None,
    };
    let cover = match cover {
        Some(path) => Some(path),
        None => embedded_cover(db, ffmpeg, cache_dir, first_file, cancel).await?,
    };
    if let Some(path) = cover {
        metadata::replace_images(
            db,
            item_id,
            "local",
            &[NewImage {
                kind: "poster".to_owned(),
                path: Some(path.to_string_lossy().into_owned()),
                score: 1.0,
                ..Default::default()
            }],
        )
        .await?;
    }
    Ok(())
}

/// `cover.jpg`, `folder.png`, … or else the only picture in the folder.
fn folder_cover(folder: &Path) -> Option<PathBuf> {
    let mut pictures: Vec<PathBuf> = std::fs::read_dir(folder)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && crate::photos::is_photo_file(p))
        .collect();
    pictures.sort();
    let named = |p: &PathBuf| {
        let stem = p
            .file_stem()
            .map(|s| s.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        COVER_NAMES.iter().position(|n| *n == stem)
    };
    pictures
        .iter()
        .filter_map(|p| named(p).map(|rank| (rank, p)))
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, p)| p.clone())
        .or_else(|| (pictures.len() == 1).then(|| pictures[0].clone()))
}

/// The cover picture embedded in an audio file (an attached picture stream), saved
/// as a JPEG in the cache.
async fn embedded_cover(
    db: &Db,
    ffmpeg: &Path,
    cache_dir: &Path,
    file: &media::MediaFile,
    cancel: &CancellationToken,
) -> anyhow::Result<Option<PathBuf>> {
    let Some(cover) = media::streams(db, file.id)
        .await?
        .into_iter()
        .find(|s| s.kind == "video")
    else {
        return Ok(None);
    };
    let source = VideoSource {
        input: Path::new(&file.path),
        stream_index: cover.idx,
        tonemap: false,
    };
    let done = generate_chapter_images(
        ffmpeg,
        &source,
        (500, 500),
        &[(0, 0, 0)],
        cache_dir,
        file.id,
        cancel,
    )
    .await;
    match done {
        Ok(done) if done.contains(&0) => Ok(Some(chapter_image_path(cache_dir, file.id, 0))),
        Ok(_) => Ok(None),
        Err(err) => {
            tracing::debug!(file = file.id, %err, "no embedded cover");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(path: &str) -> BookSpec {
        book_spec(Path::new(path))
    }

    #[test]
    fn folders_name_the_book_author_and_series() {
        let s = spec("Andreas Eschbach/Das Jesus-Video (1998)/CD1/01 Track.mp3");
        assert_eq!(s.key, "Andreas Eschbach/Das Jesus-Video (1998)");
        assert_eq!(s.title, "Das Jesus-Video");
        assert_eq!(s.author.as_deref(), Some("Andreas Eschbach"));
        assert_eq!(s.year, Some(1998));
        assert_eq!(s.series, None);

        let s = spec("J.K. Rowling/Harry Potter/02 - Die Kammer des Schreckens/01.mp3");
        assert_eq!(s.title, "Die Kammer des Schreckens");
        assert_eq!(s.author.as_deref(), Some("J.K. Rowling"));
        assert_eq!(s.series.as_deref(), Some("Harry Potter"));
        assert_eq!(s.series_index, Some(2.0));

        let s = spec("Die drei Fragezeichen/Folge 125 - Feuermond/teil1.mp3");
        assert_eq!(s.title, "Feuermond");
        assert_eq!(s.series.as_deref(), Some("Die drei Fragezeichen"));
        assert_eq!(s.series_index, Some(125.0));
        assert_eq!(s.author, None);

        let s = spec("Sebastian Fitzek - Das Paket/01.mp3");
        assert_eq!(s.title, "Das Paket");
        assert_eq!(s.author.as_deref(), Some("Sebastian Fitzek"));

        // Disc folders below the book never become books of their own.
        assert_eq!(spec("Buch/CD 2/01.mp3").key, "Buch");
        assert_eq!(spec("Buch/Disc2/01.mp3").key, "Buch");
    }

    #[test]
    fn m4b_files_are_books_of_their_own() {
        let s = spec("Ursula K. Le Guin/Der Magier der Erdsee.m4b");
        assert_eq!(s.key, "Ursula K. Le Guin/Der Magier der Erdsee");
        assert_eq!(s.title, "Der Magier der Erdsee");
        assert_eq!(s.author.as_deref(), Some("Ursula K. Le Guin"));
        let other = spec("Ursula K. Le Guin/Die Gräber von Atuan.m4b");
        assert_ne!(s.key, other.key);

        let s = spec("Terry Pratchett - Die Farben der Magie.m4b");
        assert_eq!(s.title, "Die Farben der Magie");
        assert_eq!(s.author.as_deref(), Some("Terry Pratchett"));

        let s = spec("Autorin/Reihe/03 - Dritter Band.m4b");
        assert_eq!(s.title, "Dritter Band");
        assert_eq!(s.series.as_deref(), Some("Reihe"));
        assert_eq!(s.series_index, Some(3.0));
        assert_eq!(s.author.as_deref(), Some("Autorin"));
        assert!(is_audio_file(Path::new("x/Book.M4B")));
        assert!(!is_audio_file(Path::new("cover.jpg")));
    }

    #[test]
    fn tags_win_over_folder_names() {
        let folder = BookMeta {
            author: Some("Ordner-Autor".into()),
            series_name: Some("Ordner-Reihe".into()),
            series_index: Some(1.0),
            ..Default::default()
        };
        let tags: Tags = [
            ("album", "Das echte Buch"),
            ("artist", "Echte Autorin"),
            ("composer", "Der Sprecher"),
            ("mvin", "4/7"),
            ("date", "2019-05-02"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        let (meta, title) = merge_tags(&folder, &tags, false);
        assert_eq!(title.as_deref(), Some("Das echte Buch"));
        assert_eq!(meta.author.as_deref(), Some("Echte Autorin"));
        assert_eq!(meta.narrator.as_deref(), Some("Der Sprecher"));
        assert_eq!(meta.series_name.as_deref(), Some("Ordner-Reihe"));
        assert_eq!(meta.series_index, Some(4.0));
        assert_eq!(tag_year(&tags), Some(2019));

        // A part's track title is no book title; a single file's is.
        let only_title: Tags = [("title".to_owned(), "Kapitel 1".to_owned())].into();
        assert_eq!(merge_tags(&folder, &only_title, false).1, None);
        assert_eq!(
            merge_tags(&folder, &only_title, true).1.as_deref(),
            Some("Kapitel 1")
        );
        assert_eq!(merge_tags(&folder, &Tags::new(), false).0, folder);
    }

    #[test]
    fn chapters_span_all_parts() {
        let parts = [
            PartInfo {
                file_id: 1,
                duration_ms: 60_000,
                title: "Teil 1".into(),
                chapters: vec![],
            },
            PartInfo {
                file_id: 2,
                duration_ms: 120_000,
                title: "Teil 2".into(),
                // Embedded chapters; one without title, one past the end (clamped),
                // one empty (dropped).
                chapters: vec![
                    (0, 50_000, Some("Anfang".into())),
                    (50_000, 90_000, None),
                    (90_000, 130_000, Some("Ende".into())),
                    (90_000, 90_000, Some("leer".into())),
                ],
            },
            PartInfo {
                file_id: 3,
                duration_ms: 30_000,
                title: "Epilog".into(),
                chapters: vec![],
            },
        ];
        let (chapters, offsets, total) = book_chapters(&parts);
        assert_eq!(offsets, [0, 60_000, 180_000]);
        assert_eq!(total, 210_000);
        let titles: Vec<&str> = chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["Teil 1", "Anfang", "Kapitel 3", "Ende", "Epilog"]);
        assert_eq!(
            (chapters[1].start_ms, chapters[1].end_ms),
            (60_000, 110_000)
        );
        assert_eq!(
            (chapters[3].start_ms, chapters[3].end_ms),
            (150_000, 180_000)
        );
        assert_eq!(chapters[4].part, 2);
        assert_eq!(book_chapters(&[]), (vec![], vec![], 0));
    }
}
