//! File system diff for libraries that are not films or series (photos, audiobooks):
//! the same walk-and-compare as the video scan, with a different file filter and
//! without rename detection (these libraries keep no history on single files).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use flixnet_db::{Db, media};
use walkdir::WalkDir;

use crate::scan::FoundFile;

/// Every file below `root` that `accept` takes. Hidden folders and NAS metadata
/// folders are skipped. Fails only if `root` itself is unreadable.
pub fn walk_matching(root: &Path, accept: fn(&Path) -> bool) -> std::io::Result<Vec<FoundFile>> {
    std::fs::read_dir(root)?;
    let mut found = Vec::new();
    let walker = WalkDir::new(root)
        .follow_links(true)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            entry.depth() == 0
                || !(name.starts_with('.')
                    || name.eq_ignore_ascii_case("@eaDir")
                    || name.eq_ignore_ascii_case("#recycle")
                    || name.eq_ignore_ascii_case("$RECYCLE.BIN"))
        });
    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(%err, "skipping unreadable path while scanning");
                continue;
            }
        };
        if !entry.file_type().is_file() || !accept(entry.path()) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let mtime_ms = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as i64);
        found.push(FoundFile {
            path: entry.path().to_path_buf(),
            relative: entry
                .path()
                .strip_prefix(root)
                .unwrap_or(entry.path())
                .to_path_buf(),
            size: metadata.len(),
            mtime_ms,
        });
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(found)
}

/// Lowercase extension of a path.
pub fn extension(path: &Path) -> Option<String> {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
}

/// What changed on disk since the last scan.
#[derive(Debug, Default)]
pub struct Diff {
    pub new_files: Vec<FoundFile>,
    /// (file id, item id, file) of files whose size or modification time changed.
    pub changed: Vec<(i64, Option<i64>, FoundFile)>,
    pub restored: Vec<i64>,
    pub missing: Vec<i64>,
    pub unchanged: usize,
    pub unreachable: Vec<String>,
}

/// Walks the library folders and compares them with the database (reads only).
pub async fn diff(
    db: &Db,
    library_id: i64,
    roots: &[String],
    accept: fn(&Path) -> bool,
) -> anyhow::Result<Diff> {
    let mut out = Diff::default();
    let mut found = Vec::new();
    let mut reachable: Vec<PathBuf> = Vec::new();
    for root in roots {
        let root_path = PathBuf::from(root);
        let walked = tokio::task::spawn_blocking({
            let root_path = root_path.clone();
            move || walk_matching(&root_path, accept)
        })
        .await?;
        match walked {
            Ok(files) => {
                reachable.push(root_path);
                found.extend(files);
            }
            Err(err) => {
                tracing::warn!(%root, %err, "library folder unreachable; its files are left untouched");
                out.unreachable.push(root.clone());
            }
        }
    }
    let existing: HashMap<String, media::FileState> = media::file_states(db, library_id)
        .await?
        .into_iter()
        .map(|f| (f.path.clone(), f))
        .collect();
    let mut seen = HashSet::new();
    for file in found {
        let key = file.path.to_string_lossy().into_owned();
        seen.insert(key.clone());
        match existing.get(&key) {
            None => out.new_files.push(file),
            Some(state) if state.size != file.size as i64 || state.mtime != file.mtime_ms => {
                let item_id = media::get_file(db, state.id).await?.and_then(|f| f.item_id);
                out.changed.push((state.id, item_id, file));
            }
            Some(state) if state.missing_since.is_some() => out.restored.push(state.id),
            Some(_) => out.unchanged += 1,
        }
    }
    out.missing = existing
        .values()
        .filter(|f| !seen.contains(&f.path) && f.missing_since.is_none())
        .filter(|f| {
            reachable
                .iter()
                .any(|root| Path::new(&f.path).starts_with(root))
        })
        .map(|f| f.id)
        .collect();
    Ok(out)
}

/// Marks restored and missing files (one short transaction).
pub async fn apply_presence(db: &Db, diff: &Diff) -> anyhow::Result<()> {
    if diff.restored.is_empty() && diff.missing.is_empty() {
        return Ok(());
    }
    let now = flixnet_db::now_ms();
    let mut tx = db.writer().begin().await?;
    for id in &diff.restored {
        media::mark_present(&mut tx, *id).await?;
    }
    for id in &diff.missing {
        media::mark_missing(&mut tx, *id, now).await?;
    }
    tx.commit().await?;
    Ok(())
}
