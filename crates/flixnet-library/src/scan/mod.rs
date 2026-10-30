//! Library scanning: file system diff → items and media files → probe jobs.
//!
//! The scan itself is cheap and fast (no network, no ffprobe): it walks the folders,
//! compares size/mtime with the database, creates items from parsed names and queues
//! the expensive work (probing, later metadata and analysis) as separate jobs.
//! A rescan with no changes writes nothing and queues nothing.

mod mapping;
mod walk;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use flixnet_core::jobs::{JobOptions, JobQueue, priority};
use flixnet_db::{Db, items, libraries, media};
use serde::Serialize;

pub use mapping::{normalize_key, sort_title};
pub use walk::{FoundFile, walk};

use crate::jobs::{ProbeFile, ProbePayload, queue_refresh};
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
    match library.kind.as_str() {
        "photos" => return crate::photos::scan_photos(db, queue, library_id).await,
        "audiobooks" => return crate::audiobooks::scan_audiobooks(db, queue, library_id).await,
        _ => {}
    }
    let roots = libraries::paths(db, library_id).await?;
    let mut report = ScanReport::default();

    // 1. Walk every folder (blocking I/O off the async runtime).
    let mut found: Vec<FoundFile> = Vec::new();
    let mut reachable: Vec<PathBuf> = Vec::new();
    for root in &roots {
        let root_path = PathBuf::from(root);
        let walked = tokio::task::spawn_blocking({
            let root_path = root_path.clone();
            move || walk(&root_path)
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
        let mut tx = db.writer().begin().await?;
        for file in chunk {
            match import_file(&mut tx, library_id, &library.kind, file, &mut item_ids).await? {
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
    to_refresh.sort_unstable();
    to_refresh.dedup();
    for chunk in changed.chunks(BATCH) {
        let mut tx = db.writer().begin().await?;
        for (id, file) in chunk {
            media::mark_changed(&mut tx, *id, file.size as i64, file.mtime_ms).await?;
            report.changed += 1;
            to_probe.push((*id, priority::NEW));
        }
        tx.commit().await?;
    }
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
