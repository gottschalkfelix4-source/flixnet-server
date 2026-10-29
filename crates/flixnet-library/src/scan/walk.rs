//! File system walk (blocking; run it on a blocking thread).

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use walkdir::WalkDir;

use crate::parse::is_video_file;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundFile {
    pub path: PathBuf,
    /// Relative to the library folder; drives all name parsing.
    pub relative: PathBuf,
    pub size: u64,
    pub mtime_ms: i64,
}

/// Every video file below `root`. Hidden folders and NAS metadata folders are skipped.
/// Fails only if `root` itself is unreadable (a sub-folder error is logged and skipped).
pub fn walk(root: &Path) -> std::io::Result<Vec<FoundFile>> {
    // An unreadable root must not look like "all files deleted".
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
        if !entry.file_type().is_file() || !is_video_file(entry.path()) {
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
        let relative = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .to_path_buf();
        found.push(FoundFile {
            path: entry.path().to_path_buf(),
            relative,
            size: metadata.len(),
            mtime_ms,
        });
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(found)
}
