//! File system watching: changes in library folders trigger a (debounced) scan.
//!
//! Watchers are best effort — network shares and Unraid user shares often do not
//! deliver events — so the periodic rescan task stays the safety net.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use flixnet_core::jobs::{JobQueue, priority};
use flixnet_db::{Db, libraries};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

use crate::jobs::queue_scan;

#[derive(Clone)]
pub struct LibraryWatcher {
    inner: Arc<Mutex<Inner>>,
    events: mpsc::UnboundedSender<PathBuf>,
}

struct Inner {
    watcher: Option<RecommendedWatcher>,
    /// Library folder → library id.
    roots: Vec<(PathBuf, i64)>,
}

impl LibraryWatcher {
    /// Starts the debounce task. Call [`LibraryWatcher::sync`] to (re)load the folders.
    pub fn start(queue: JobQueue, debounce: Duration) -> Self {
        let (events, mut receiver) = mpsc::unbounded_channel::<PathBuf>();
        let inner = Arc::new(Mutex::new(Inner {
            watcher: None,
            roots: Vec::new(),
        }));

        let roots_of = inner.clone();
        tokio::spawn(async move {
            // library id → deadline of its pending scan
            let mut pending: HashMap<i64, tokio::time::Instant> = HashMap::new();
            loop {
                let next_deadline = pending.values().min().copied();
                tokio::select! {
                    path = receiver.recv() => {
                        let Some(path) = path else { break };
                        let library = roots_of
                            .lock()
                            .expect("watcher lock")
                            .roots
                            .iter()
                            .find(|(root, _)| path.starts_with(root))
                            .map(|(_, id)| *id);
                        if let Some(id) = library {
                            // Every new event pushes the scan back: wait for quiet.
                            pending.insert(id, tokio::time::Instant::now() + debounce);
                        }
                    }
                    () = async {
                        match next_deadline {
                            Some(deadline) => tokio::time::sleep_until(deadline).await,
                            None => std::future::pending().await,
                        }
                    } => {
                        let now = tokio::time::Instant::now();
                        let due: Vec<i64> = pending.iter().filter(|(_, d)| **d <= now).map(|(id, _)| *id).collect();
                        for id in due {
                            pending.remove(&id);
                            tracing::debug!(library = id, "folder changed; queueing scan");
                            if let Err(err) = queue_scan(&queue, id, priority::NEW).await {
                                tracing::warn!(%err, library = id, "could not queue scan after file change");
                            }
                        }
                    }
                }
            }
        });

        Self { inner, events }
    }

    /// Watches the current folders of all libraries (call after libraries change).
    pub async fn sync(&self, db: &Db) -> anyhow::Result<()> {
        let mut roots = Vec::new();
        for library in libraries::list(db).await? {
            for path in libraries::paths(db, library.id).await? {
                roots.push((PathBuf::from(path), library.id));
            }
        }

        let sender = self.events.clone();
        let mut watcher = notify::recommended_watcher(
            move |result: notify::Result<notify::Event>| match result {
                Ok(event) => {
                    if matches!(event.kind, EventKind::Access(_)) {
                        return;
                    }
                    for path in event.paths {
                        let _ = sender.send(path);
                    }
                }
                Err(err) => tracing::debug!(%err, "file watcher error"),
            },
        )?;
        let mut watched = 0;
        for (root, library) in &roots {
            match watcher.watch(root, RecursiveMode::Recursive) {
                Ok(()) => watched += 1,
                Err(err) => {
                    tracing::warn!(%err, library, path = %root.display(), "cannot watch folder; relying on periodic scans")
                }
            }
        }
        tracing::info!(folders = watched, "watching library folders");

        let mut inner = self.inner.lock().expect("watcher lock");
        inner.watcher = Some(watcher); // drops (stops) the previous watcher
        inner.roots = roots;
        Ok(())
    }
}
