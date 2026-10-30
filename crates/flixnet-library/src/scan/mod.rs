//! Library scanning: file system diff → items and media files → probe jobs.
//!
//! The scan itself is cheap and fast (no network, no ffprobe): it walks the folders,
//! compares size/mtime with the database, creates items from parsed names and queues
//! the expensive work (probing, later metadata and analysis) as separate jobs.
//! A rescan with no changes writes nothing and queues nothing.
//!
//! Music libraries are the exception to "names only": their items come from the
//! tags, which are read (lofty, no subprocess) for each batch of new or changed files
//! *before* the batch's write transaction starts.

mod mapping;
mod walk;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use flixnet_core::jobs::{JobOptions, JobQueue, priority};
use flixnet_db::music::{AudioTags, TagFields};
use flixnet_db::{Db, items, libraries, media, music};
use serde::Serialize;

pub use mapping::{normalize_key, sort_title};
pub use walk::{FoundFile, walk, walk_audio};

use crate::jobs::{ProbeFile, ProbePayload, queue_refresh};
use crate::music::tags::{EXPLICIT_AGE, chain, read_tags};
use crate::parse::{FileRole, classify};

/// Files missing for longer than this are removed (with their orphaned items).
pub const MISSING_GRACE_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// Rows written per transaction, so readers never wait long on the writer.
const BATCH: usize = 200;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ScanReport {
    pub added: usize,
    pub changed: usize,
    /// Files renamed or moved (they keep their item and history).
    pub moved: usize,
    pub unchanged: usize,
    pub restored: usize,
    pub missing: usize,
    /// Samples and other files that are deliberately not imported.
    pub skipped: usize,
    pub removed: u64,
    pub probes_queued: usize,
    pub refreshes_queued: usize,
    /// Library folders that could not be read (their files were left untouched).
    pub unreachable: Vec<String>,
}

pub async fn scan_library(
    db: &Db,
    queue: &JobQueue,
    library_id: i64,
) -> anyhow::Result<ScanReport> {
    let library = libraries::get(db, library_id)
        .await?
        .with_context(|| format!("library {library_id} does not exist"))?;
    let roots = libraries::paths(db, library_id).await?;
    let mut report = ScanReport::default();
    let is_music = library.kind == "music";

    // 1. Walk every folder (blocking I/O off the async runtime).
    let mut found: Vec<FoundFile> = Vec::new();
    let mut reachable: Vec<PathBuf> = Vec::new();
    for root in &roots {
        let root_path = PathBuf::from(root);
        let walked = tokio::task::spawn_blocking({
            let root_path = root_path.clone();
            move || {
                if is_music {
                    walk_audio(&root_path)
                } else {
                    walk(&root_path)
                }
            }
        })
        .await?;
        match walked {
            Ok(files) => {
                reachable.push(root_path);
                found.extend(files);
            }
            Err(err) => {
                tracing::warn!(library = %library.name, %root, %err, "library folder unreachable; its files are left untouched");
                report.unreachable.push(root.clone());
            }
        }
    }

    // 2. Diff against the database.
    let existing: HashMap<String, media::FileState> = media::file_states(db, library_id)
        .await?
        .into_iter()
        .map(|f| (f.path.clone(), f))
        .collect();

    let mut new_files = Vec::new();
    let mut changed = Vec::new();
    let mut restored = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for file in found {
        let key = file.path.to_string_lossy().into_owned();
        seen.insert(key.clone());
        match existing.get(&key) {
            None => new_files.push(file),
            Some(state) if state.size != file.size as i64 || state.mtime != file.mtime_ms => {
                changed.push((state.id, file));
            }
            Some(state) if state.missing_since.is_some() => restored.push(state.id),
            Some(_) => report.unchanged += 1,
        }
    }
    let now = flixnet_db::now_ms();
    // Gone (or already missing) files under readable folders are move candidates.
    let gone: Vec<&media::FileState> = existing
        .values()
        .filter(|f| !seen.contains(&f.path))
        .filter(|f| {
            reachable
                .iter()
                .any(|root| Path::new(&f.path).starts_with(root))
        })
        .collect();

    // Renames/moves keep size and modification time: pair a gone file with a new one
    // that has exactly the same (size, mtime), as long as the pairing is unambiguous.
    let mut moves: Vec<(i64, FoundFile)> = Vec::new();
    {
        let mut by_signature: HashMap<(i64, i64), Vec<i64>> = HashMap::new();
        for f in &gone {
            by_signature
                .entry((f.size, f.mtime))
                .or_default()
                .push(f.id);
        }
        let mut new_by_signature: HashMap<(i64, i64), usize> = HashMap::new();
        for f in &new_files {
            *new_by_signature
                .entry((f.size as i64, f.mtime_ms))
                .or_default() += 1;
        }
        new_files.retain(|f| {
            let signature = (f.size as i64, f.mtime_ms);
            match by_signature.get(&signature) {
                Some(ids) if ids.len() == 1 && new_by_signature.get(&signature) == Some(&1) => {
                    moves.push((ids[0], f.clone()));
                    false
                }
                _ => true,
            }
        });
    }
    let moved_ids: std::collections::HashSet<i64> = moves.iter().map(|(id, _)| *id).collect();
    let missing: Vec<i64> = gone
        .iter()
        .filter(|f| f.missing_since.is_none() && !moved_ids.contains(&f.id))
        .map(|f| f.id)
        .collect();

    // 3. Write in short batches.
    if !moves.is_empty() {
        let mut tx = db.writer().begin().await?;
        for (id, file) in &moves {
            let path = file.path.to_string_lossy();
            let relative = file.relative.to_string_lossy().replace('\\', "/");
            media::move_file(&mut tx, *id, &path, &relative).await?;
        }
        tx.commit().await?;
        report.moved = moves.len();
    }
    let mut to_probe: Vec<(i64, i64)> = Vec::new(); // (file id, priority)
    let mut to_refresh: Vec<i64> = Vec::new(); // items that got new files
    let mut item_ids: HashMap<String, i64> = HashMap::new();
    for chunk in new_files.chunks(BATCH) {
        // Music: tags first (file I/O), then the short write transaction.
        let tags = if is_music {
            read_chunk_tags(chunk.iter()).await?
        } else {
            Vec::new()
        };
        let mut tx = db.writer().begin().await?;
        for (i, file) in chunk.iter().enumerate() {
            let imported = match tags.get(i) {
                Some(Some(tags)) => {
                    let (file_id, chain) =
                        import_track(&mut tx, library_id, file, tags, &mut item_ids, None).await?;
                    Some((file_id, chain.to_vec()))
                }
                Some(None) => None,
                None => {
                    import_file(&mut tx, library_id, &library.kind, file, &mut item_ids).await?
                }
            };
            match imported {
                Some((file_id, chain)) => {
                    report.added += 1;
                    to_probe.push((file_id, priority::NEW));
                    to_refresh.extend(chain);
                }
                None => report.skipped += 1,
            }
        }
        tx.commit().await?;
    }
    for chunk in changed.chunks(BATCH) {
        // Re-tagged music files may now belong to another album or artist.
        let tags = if is_music {
            read_chunk_tags(chunk.iter().map(|(_, f)| f)).await?
        } else {
            Vec::new()
        };
        let mut tx = db.writer().begin().await?;
        for (i, (id, file)) in chunk.iter().enumerate() {
            media::mark_changed(&mut tx, *id, file.size as i64, file.mtime_ms).await?;
            if let Some(Some(tags)) = tags.get(i) {
                let (_, chain) =
                    import_track(&mut tx, library_id, file, tags, &mut item_ids, Some(*id)).await?;
                to_refresh.extend(chain);
            }
            report.changed += 1;
            to_probe.push((*id, priority::NEW));
        }
        tx.commit().await?;
    }
    to_refresh.sort_unstable();
    to_refresh.dedup();
    if !restored.is_empty() || !missing.is_empty() {
        let mut tx = db.writer().begin().await?;
        for id in &restored {
            media::mark_present(&mut tx, *id).await?;
        }
        for id in &missing {
            media::mark_missing(&mut tx, *id, now).await?;
        }
        tx.commit().await?;
    }
    report.restored = restored.len();
    report.missing = missing.len();

    // 4. Expired missing files go away, then items nothing points to anymore.
    let removed_files =
        media::delete_missing_before(db, library_id, now - MISSING_GRACE_MS).await?;
    if removed_files > 0 || report.changed > 0 {
        report.removed = items::prune_orphans(db, library_id).await?;
    }

    // 5. Queue probes (deduplicated per file).
    for (file_id, prio) in to_probe {
        let queued = queue
            .enqueue::<ProbeFile>(
                &ProbePayload { file_id },
                JobOptions::priority(prio).dedupe(format!("probe:{file_id}")),
            )
            .await?;
        if queued.is_some() {
            report.probes_queued += 1;
        }
    }
    // 6. Metadata (NFO, artwork, later providers) for items that gained files.
    for item_id in to_refresh {
        if queue_refresh(queue, item_id, priority::NEW)
            .await?
            .is_some()
        {
            report.refreshes_queued += 1;
        }
    }

    libraries::mark_scanned(db, library_id).await?;
    tracing::info!(library = %library.name, ?report, "library scanned");
    Ok(report)
}

/// Tags of a batch of audio files, read on a blocking thread. `None` for files that
/// are not imported (macOS `._` resource forks and other hidden files).
async fn read_chunk_tags<'a>(
    files: impl Iterator<Item = &'a FoundFile>,
) -> anyhow::Result<Vec<Option<AudioTags>>> {
    let files: Vec<(PathBuf, PathBuf)> = files
        .map(|f| (f.path.clone(), f.relative.clone()))
        .collect();
    Ok(tokio::task::spawn_blocking(move || {
        let mut tags: Vec<Option<AudioTags>> = files
            .iter()
            .map(|(path, relative)| {
                let hidden = relative
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'));
                (!hidden).then(|| {
                    let mut tags = read_tags(path, relative);
                    tags.has_lyrics |= crate::music::lyrics::has_sidecar(path);
                    tags
                })
            })
            .collect();
        mark_compilations(&files, &mut tags);
        tags
    })
    .await?)
}

/// Compilations without album artist or compilation flag (common in MP3 collections,
/// where ffmpeg and some taggers write the flag as an unknown `TXXX`): one album in
/// one folder with several track artists is a compilation, not one album per artist.
fn mark_compilations(files: &[(PathBuf, PathBuf)], tags: &mut [Option<AudioTags>]) {
    let mut artists: HashMap<(PathBuf, String), Vec<String>> = HashMap::new();
    let key = |path: &Path, t: &AudioTags| {
        (
            path.parent().map(Path::to_path_buf).unwrap_or_default(),
            t.album.as_deref().map(normalize_key).unwrap_or_default(),
        )
    };
    for ((path, _), t) in files.iter().zip(tags.iter()) {
        if let Some(t) = t
            && t.album_artist.is_none()
            && !t.compilation
            && let Some(artist) = &t.artist
        {
            // "A feat. B" on an album of A is still A's album.
            let lower = artist.to_lowercase();
            let primary = [" feat", " ft.", " featuring", ", "]
                .iter()
                .filter_map(|sep| lower.find(sep))
                .min()
                .map_or(lower.as_str(), |cut| &lower[..cut])
                .trim()
                .to_owned();
            let seen = artists.entry(key(path, t)).or_default();
            if !seen.contains(&primary) {
                seen.push(primary);
            }
        }
    }
    for ((path, _), t) in files.iter().zip(tags.iter_mut()) {
        if let Some(t) = t
            && t.album_artist.is_none()
            && artists.get(&key(path, t)).is_some_and(|a| a.len() > 1)
        {
            t.compilation = true;
        }
    }
}

/// Creates (or finds) artist → album → track for an audio file and inserts the file,
/// or re-attaches an existing one (`existing`, after a re-tag). Returns the file id
/// and the artist and album ids (they get a metadata refresh; tracks do not).
async fn import_track(
    conn: &mut flixnet_db::SqliteConnection,
    library_id: i64,
    file: &FoundFile,
    tags: &AudioTags,
    item_ids: &mut HashMap<String, i64>,
    existing: Option<i64>,
) -> anyhow::Result<(i64, [i64; 2])> {
    let specs = chain(tags);
    let mut ids = [0_i64; 3];
    let mut parent_id = None;
    let mut artist_id = None;
    for (n, spec) in specs.iter().enumerate() {
        let id = match item_ids.get(&spec.key) {
            Some(id) => *id,
            None => {
                let sort = sort_title(&spec.title);
                let id = items::upsert(
                    conn,
                    &items::NewItem {
                        library_id,
                        kind: spec.kind,
                        parent_id,
                        series_id: artist_id,
                        title: &spec.title,
                        sort_title: &sort,
                        year: spec.year,
                        index_number: spec.index_number,
                        index_end: None,
                        parent_index_number: spec.parent_index_number,
                        premiere_date: None,
                        content_key: &spec.key,
                    },
                )
                .await?;
                for (provider, value) in &spec.provider_ids {
                    items::add_provider_id(conn, id, provider, value).await?;
                }
                item_ids.insert(spec.key.clone(), id);
                id
            }
        };
        if spec.kind == "artist" {
            artist_id = Some(id);
        }
        parent_id = Some(id);
        ids[n] = id;
    }
    // Tags are the source of truth for a track (the upsert keeps existing titles).
    let track = &specs[2];
    let sort = sort_title(&track.title);
    music::apply_tag_fields(
        conn,
        ids[2],
        &TagFields {
            title: &track.title,
            sort_title: &sort,
            year: track.year,
            index_number: track.index_number,
            parent_index_number: track.parent_index_number,
            age_rating: tags.explicit.then_some(EXPLICIT_AGE),
        },
    )
    .await?;
    music::set_genres(conn, ids[2], &tags.genres).await?;

    let file_id = match existing {
        Some(id) => {
            music::attach_file(conn, id, ids[2]).await?;
            id
        }
        None => {
            let path = file.path.to_string_lossy();
            let relative = file.relative.to_string_lossy().replace('\\', "/");
            media::insert_file(
                conn,
                &media::NewFile {
                    library_id,
                    item_id: Some(ids[2]),
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
            .await?
        }
    };
    music::store_tags(conn, file_id, tags).await?;
    Ok((file_id, [ids[0], ids[1]]))
}

/// Creates the item chain for a new file and inserts the file.
/// Returns the file id and the ids of the item chain; `None` = not imported.
async fn import_file(
    conn: &mut flixnet_db::SqliteConnection,
    library_id: i64,
    library_kind: &str,
    file: &FoundFile,
    item_ids: &mut HashMap<String, i64>,
) -> anyhow::Result<Option<(i64, Vec<i64>)>> {
    let role = classify(&file.relative);
    let (role_name, extra_kind) = match role {
        FileRole::Main => ("main", None),
        FileRole::Extra(kind) => (
            "extra",
            Some(
                serde_json::to_value(kind)?
                    .as_str()
                    .unwrap_or("other")
                    .to_owned(),
            ),
        ),
        FileRole::Sample | FileRole::Ignored => return Ok(None),
    };

    let (chain, parsed_json, part) = if library_kind == "shows" {
        let (mut chain, parsed) = mapping::episode(&file.relative);
        if role_name == "extra" {
            chain.truncate(1); // series extras attach to the series
        }
        (chain, serde_json::to_string(&parsed)?, None)
    } else {
        let (spec, parsed) = mapping::movie(&file.relative);
        let part = parsed.part.map(i64::from);
        (vec![spec], serde_json::to_string(&parsed)?, part)
    };

    // Upsert the chain top-down: series → season → episode (or just the movie).
    let mut series_id = None;
    let mut parent_id = None;
    let mut leaf_id = None;
    let mut chain_ids = Vec::with_capacity(chain.len());
    for spec in &chain {
        let id = match item_ids.get(&spec.key) {
            Some(id) => *id,
            None => {
                let sort = sort_title(&spec.title);
                let id = items::upsert(
                    conn,
                    &items::NewItem {
                        library_id,
                        kind: spec.kind,
                        parent_id,
                        series_id,
                        title: &spec.title,
                        sort_title: &sort,
                        year: spec.year,
                        index_number: spec.index_number,
                        index_end: spec.index_end,
                        parent_index_number: spec.parent_index_number,
                        premiere_date: spec.premiere_date.as_deref(),
                        content_key: &spec.key,
                    },
                )
                .await?;
                for (provider, value) in &spec.provider_ids {
                    items::add_provider_id(conn, id, provider, value).await?;
                }
                item_ids.insert(spec.key.clone(), id);
                id
            }
        };
        if spec.kind == "series" {
            series_id = Some(id);
        }
        parent_id = Some(id);
        leaf_id = Some(id);
        chain_ids.push(id);
    }

    let path = file.path.to_string_lossy();
    let relative = file.relative.to_string_lossy().replace('\\', "/");
    let file_id = media::insert_file(
        conn,
        &media::NewFile {
            library_id,
            item_id: leaf_id,
            path: &path,
            relative_path: &relative,
            size: file.size as i64,
            mtime: file.mtime_ms,
            role: role_name,
            extra_kind: extra_kind.as_deref(),
            part,
            parsed_json: Some(&parsed_json),
        },
    )
    .await?;
    Ok(Some((file_id, chain_ids)))
}
