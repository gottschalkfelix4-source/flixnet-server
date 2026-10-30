//! DVR: recording rules, planning, conflicts and the recorder.
//!
//! * [`plan`] turns rules and the guide into scheduled recordings and marks
//!   conflicts against tuner limits ([`conflicts`]).
//! * [`Dvr`] runs recordings: every few seconds it starts what is due — from the
//!   database, so recordings scheduled before a restart run after it — and finishes
//!   what is over. A recording joins the channel's shared upstream as a pinned
//!   session (copied streams: the broadcast as sent) and appends its segments to the
//!   recording file; segments still in the timeshift buffer from before the start are
//!   taken too. A recording that was running when the server stopped resumes into a
//!   new part file ([`naming::part_path`]).
//! * Finished recordings get an NFO and are announced through the hook, which makes
//!   the library scan them; rules with "keep last N" delete older ones.

pub mod conflicts;
pub mod naming;
pub mod plan;
pub mod rules;

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use flixnet_db::dvr::{self as db_dvr, Recording};
use flixnet_db::{Db, now_ms};
use flixnet_media::decision::{AudioPlan, PlayMethod, PlaybackPlan, StreamAction, VideoPlan};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Notify, watch};

use crate::config;
use crate::stream::{LiveError, LiveTvManager, WatchRequest};
use crate::tuner::{is_stream_url, stream_source};
use naming::RecordingMeta;

/// Called once a recording finished (or recordings were deleted): the library scans
/// the recordings folder.
pub type FinishedHook =
    Arc<dyn Fn(Option<Recording>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Tunables; tests shorten them.
#[derive(Debug, Clone)]
pub struct DvrSettings {
    /// How often due recordings are checked.
    pub tick: Duration,
    /// How often the planner runs on its own.
    pub plan_every: Duration,
    /// Wait before trying again when no tuner or no stream is available.
    pub retry: Duration,
}

impl Default for DvrSettings {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(2),
            plan_every: Duration::from_secs(5 * 60),
            retry: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// A user stopped it: finish with what is there.
    User,
    /// The server shuts down: leave it "recording" so it resumes after the restart.
    Shutdown,
}

struct Active {
    stop: watch::Sender<Option<Stop>>,
    /// Taken while someone waits for the recorder to stop (the entry stays, so the
    /// recording is not started again meanwhile).
    task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Clone)]
pub struct Dvr {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    live: LiveTvManager,
    default_root: PathBuf,
    settings: DvrSettings,
    hook: Mutex<Option<FinishedHook>>,
    active: Mutex<HashMap<i64, Active>>,
    wake: Notify,
    replan: std::sync::atomic::AtomicBool,
    shutdown: watch::Sender<bool>,
}

/// The plan of a recording: the broadcast as sent (both streams copied).
fn copy_plan() -> PlaybackPlan {
    PlaybackPlan {
        method: PlayMethod::Remux,
        video: Some(VideoPlan {
            stream_index: 0,
            action: StreamAction::Copy,
            codec: String::new(),
            width: 0,
            height: 0,
            bitrate: None,
            tonemap: false,
        }),
        audio: Some(AudioPlan {
            stream_index: 1,
            action: StreamAction::Copy,
            codec: String::new(),
            channels: 2,
        }),
        subtitle: None,
        reasons: vec![],
    }
}

pub fn meta_of(r: &Recording) -> RecordingMeta {
    RecordingMeta {
        title: r.title.clone(),
        subtitle: r.subtitle.clone(),
        description: r.description.clone(),
        categories: r.category_list(),
        season: r.season,
        episode: r.episode,
        year: r.year,
        rating: r.rating.clone(),
        start_ms: r.start_ms,
        series_rule: r.rule_kind.as_deref() == Some("series"),
        utc_offset_min: r.utc_offset_min,
    }
}

/// The name of the first part without a part suffix (`X - part1.ts` → `X.ts`).
fn base_of(first: &Path) -> PathBuf {
    let stem = first
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match stem.strip_suffix(" - part1") {
        Some(base) => first.with_file_name(format!("{base}.ts")),
        None => first.to_owned(),
    }
}

/// All files of a recording (parts and NFOs).
pub fn files_of(r: &Recording) -> Vec<PathBuf> {
    let Some(path) = r.path.as_deref().map(PathBuf::from) else {
        return Vec::new();
    };
    let base = base_of(&path);
    let parts = r.parts.max(1) as u32;
    let mut files = Vec::new();
    for part in 1..=parts {
        let file = naming::file_of_part(&base, part, parts);
        files.push(file.with_extension("nfo"));
        files.push(file);
    }
    files
}

/// Deletes a recording's files (and the movie folder's NFO and empty folders).
pub async fn delete_files(r: &Recording) {
    for file in files_of(r) {
        if let Err(err) = tokio::fs::remove_file(&file).await
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %file.display(), %err, "could not delete recording file");
        }
    }
    if let Some(dir) = r.path.as_deref().and_then(|p| Path::new(p).parent()) {
        let _ = tokio::fs::remove_file(dir.join("movie.nfo")).await;
        // Empty folders up to the series folder.
        let mut dir = dir.to_owned();
        for _ in 0..2 {
            if tokio::fs::remove_dir(&dir).await.is_err() {
                break;
            }
            match dir.parent() {
                Some(parent) => dir = parent.to_owned(),
                None => break,
            }
        }
    }
}

impl Dvr {
    /// Starts the DVR (needs a Tokio runtime). `default_root` is the recordings
    /// folder when none is configured.
    pub fn start(
        db: Db,
        live: LiveTvManager,
        default_root: impl Into<PathBuf>,
        settings: DvrSettings,
    ) -> Self {
        let inner = Arc::new(Inner {
            db,
            live,
            default_root: default_root.into(),
            settings,
            hook: Mutex::default(),
            active: Mutex::default(),
            wake: Notify::new(),
            replan: std::sync::atomic::AtomicBool::new(true),
            shutdown: watch::channel(false).0,
        });
        tokio::spawn(run(inner.clone()));
        Self { inner }
    }

    pub fn set_hook(&self, hook: FinishedHook) {
        *self.inner.hook.lock().unwrap() = Some(hook);
    }

    /// The folder recordings are written to now.
    pub async fn recordings_root(&self) -> PathBuf {
        match config::load(&self.inner.db).await {
            Ok(c) => c.recordings_root(&self.inner.default_root),
            Err(_) => self.inner.default_root.clone(),
        }
    }

    /// Plans again soon (rules or tuners changed) and checks due recordings.
    pub fn replan(&self) {
        self.inner
            .replan
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.inner.wake.notify_one();
    }

    /// Whether the recorder runs this recording now.
    pub fn is_active(&self, id: i64) -> bool {
        self.inner.active.lock().unwrap().contains_key(&id)
    }

    /// Stops a running recording; it keeps what was recorded. Returns whether it ran.
    pub async fn stop_recording(&self, id: i64) -> bool {
        let task = {
            let mut active = self.inner.active.lock().unwrap();
            match active.get_mut(&id) {
                Some(a) => {
                    let _ = a.stop.send(Some(Stop::User));
                    a.task.take()
                }
                None => return false,
            }
        };
        if let Some(task) = task {
            let _ = task.await;
        }
        self.inner.active.lock().unwrap().remove(&id);
        true
    }

    /// Announces deleted recordings to the library.
    pub async fn notify_changed(&self) {
        let hook = self.inner.hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(None).await;
        }
    }

    /// Stops all recorders without finishing them (they resume after a restart).
    pub async fn shutdown(&self) {
        self.inner.shutdown.send_replace(true);
        let active: Vec<Active> = self
            .inner
            .active
            .lock()
            .unwrap()
            .drain()
            .map(|(_, a)| a)
            .collect();
        for a in &active {
            let _ = a.stop.send(Some(Stop::Shutdown));
        }
        for a in active {
            if let Some(task) = a.task {
                let _ = task.await;
            }
        }
    }
}

async fn run(inner: Arc<Inner>) {
    let mut shutdown = inner.shutdown.subscribe();
    let mut last_plan: Option<tokio::time::Instant> = None;
    loop {
        if *shutdown.borrow() {
            return;
        }
        let plan_due = inner
            .replan
            .swap(false, std::sync::atomic::Ordering::SeqCst)
            || last_plan.is_none_or(|t| t.elapsed() >= inner.settings.plan_every);
        if plan_due {
            last_plan = Some(tokio::time::Instant::now());
            if let Err(err) = plan::plan(&inner.db).await {
                tracing::warn!(%err, "planning recordings failed");
            }
        }
        if let Err(err) = tick(&inner).await {
            tracing::warn!(%err, "checking recordings failed");
        }
        tokio::select! {
            () = tokio::time::sleep(inner.settings.tick) => {}
            () = inner.wake.notified() => {}
            _ = shutdown.changed() => {}
        }
    }
}

/// Starts due recordings and closes overdue ones.
async fn tick(inner: &Arc<Inner>) -> Result<(), flixnet_db::DbError> {
    let now = now_ms();
    // Recorders that ended on their own.
    inner
        .active
        .lock()
        .unwrap()
        .retain(|_, a| a.task.as_ref().is_none_or(|t| !t.is_finished()));

    for r in db_dvr::overdue(&inner.db, now).await? {
        if inner.active.lock().unwrap().contains_key(&r.id) {
            continue;
        }
        if r.status == "recording" && r.bytes > 0 {
            // The server stopped during the recording and came back after its end.
            db_dvr::finish(&inner.db, r.id, "completed", None).await?;
            finished(inner, r.id).await;
        } else {
            let reason = if r.conflict {
                "Nicht aufgenommen: kein Tuner frei."
            } else {
                "Nicht aufgenommen: Der Server lief zur Sendezeit nicht."
            };
            db_dvr::finish(&inner.db, r.id, "failed", Some(reason)).await?;
        }
    }

    for r in db_dvr::due(&inner.db, now).await? {
        let mut active = inner.active.lock().unwrap();
        // Checked under the lock `shutdown` drains with: no recorder starts after it.
        if *inner.shutdown.borrow() {
            return Ok(());
        }
        if active.contains_key(&r.id) {
            continue;
        }
        let (stop, stop_rx) = watch::channel(None);
        let id = r.id;
        let task = tokio::spawn(record(inner.clone(), r, stop_rx));
        active.insert(
            id,
            Active {
                stop,
                task: Some(task),
            },
        );
    }
    Ok(())
}

/// Runs one recording until its end (or a stop).
async fn record(inner: Arc<Inner>, r: Recording, mut stop: watch::Receiver<Option<Stop>>) {
    let id = r.id;
    match record_inner(&inner, r, &mut stop).await {
        Ok(Some(Stop::Shutdown)) => {
            tracing::info!(
                recording = id,
                "recording interrupted by shutdown; resumes after restart"
            );
        }
        Ok(_) => finished(&inner, id).await,
        Err(err) => {
            tracing::warn!(recording = id, "{err:#}");
            let message = format!("{err:#}");
            if let Err(err) = db_dvr::finish(&inner.db, id, "failed", Some(&message)).await {
                tracing::warn!(recording = id, %err, "could not record the failure");
            }
        }
    }
}

/// Copies segments into the part file; returns how it stopped (`None` = at the end).
async fn record_inner(
    inner: &Arc<Inner>,
    r: Recording,
    stop: &mut watch::Receiver<Option<Stop>>,
) -> anyhow::Result<Option<Stop>> {
    let db = &inner.db;
    let channel_id = r
        .channel_id
        .ok_or_else(|| anyhow::anyhow!("Den Sender gibt es nicht mehr."))?;
    let channel = flixnet_db::livetv::get_channel(db, channel_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Den Sender gibt es nicht mehr."))?;
    let host = flixnet_db::livetv::get_host(db, channel.tuner_host_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Den Tuner gibt es nicht mehr."))?;
    if !is_stream_url(&channel.stream_url) {
        anyhow::bail!("Der Sender hat keine abspielbare Adresse.");
    }
    let config = config::load(db).await?;
    let root = config.recordings_root(&inner.default_root);

    // The file: a new one, or the next part of an interrupted recording.
    let parts = (r.parts + 1) as u32;
    let first = match r.path.as_deref() {
        Some(path) if r.parts > 0 => base_of(Path::new(path)),
        _ => {
            let wanted = root.join(naming::relative_path(&meta_of(&r)));
            naming::unused(wanted, Path::exists)
        }
    };
    if parts == 2 {
        // The plain first part becomes "part1".
        let renamed = naming::part_path(&first, 1);
        if first.exists() {
            tokio::fs::rename(&first, &renamed).await?;
        }
    }
    let file = naming::file_of_part(&first, parts, parts);
    let first_file = naming::file_of_part(&first, 1, parts);
    if let Some(dir) = file.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    db_dvr::start_part(db, r.id, &first_file.to_string_lossy(), i64::from(parts)).await?;
    let mut out = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .await?;
    tracing::info!(recording = r.id, part = parts, path = %file.display(), "recording");

    let (record_start, record_end) = r.record_span();
    let mut bytes = r.bytes;
    let mut session: Option<String> = None;
    let mut last_seq: Option<u64> = None;
    let mut error: Option<String> = None;
    let mut reported = (bytes, error.clone());
    let mut next_join = tokio::time::Instant::now();
    let stopped = loop {
        if let Some(reason) = *stop.borrow() {
            break Some(reason);
        }
        if now_ms() >= record_end {
            break None;
        }
        if session.is_none() && tokio::time::Instant::now() >= next_join {
            let request = WatchRequest {
                user_id: 0,
                channel_id: channel.id,
                tuner_host_id: host.id,
                tuner_limit: host.tuner_count,
                source: stream_source(&host, &channel.stream_url),
                plan: copy_plan(),
            };
            let mut joined = inner.live.watch_pinned(request.clone()).await;
            if matches!(joined, Err(LiveError::TunersBusy)) && inner.live.preempt(host.id).await {
                joined = inner.live.watch_pinned(request).await;
            }
            match joined {
                Ok(ticket) => {
                    session = Some(ticket.session_id);
                    error = None;
                }
                Err(err) => {
                    error = Some(match err {
                        LiveError::TunersBusy => "Alle Tuner sind belegt.".to_owned(),
                        _ => "Der Sender lässt sich gerade nicht empfangen.".to_owned(),
                    });
                    tracing::warn!(recording = r.id, %err, "recording cannot reach its channel");
                    next_join = tokio::time::Instant::now() + inner.settings.retry;
                }
            }
        }
        if let Some(id) = &session {
            match inner.live.segments_since(id, last_seq, record_start) {
                Ok(segments) => {
                    for segment in segments {
                        if segment.start_ms >= record_end {
                            break;
                        }
                        match tokio::fs::read(&segment.path).await {
                            Ok(data) => {
                                out.write_all(&data).await?;
                                bytes += data.len() as i64;
                            }
                            Err(err) => {
                                tracing::debug!(%err, "segment gone before it was recorded")
                            }
                        }
                        last_seq = Some(segment.seq);
                    }
                }
                Err(err) => {
                    // The stream was given up: join again (a new upstream).
                    tracing::info!(recording = r.id, %err, "recording lost its stream; rejoining");
                    inner.live.stop(id).await;
                    session = None;
                    last_seq = None;
                    error = Some("Der Empfang wurde unterbrochen.".to_owned());
                }
            }
        }
        if reported != (bytes, error.clone()) {
            out.flush().await?;
            db_dvr::set_progress(db, r.id, bytes, error.as_deref()).await?;
            reported = (bytes, error.clone());
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(1)) => {}
            _ = stop.changed() => {}
        }
    };
    out.flush().await?;
    drop(out);
    if let Some(id) = &session {
        inner.live.stop(id).await;
    }
    db_dvr::set_progress(db, r.id, bytes, error.as_deref()).await?;
    if stopped == Some(Stop::Shutdown) {
        return Ok(stopped);
    }
    if bytes == 0 {
        anyhow::bail!(
            "{}",
            error.unwrap_or_else(|| "Es kamen keine Daten an.".to_owned())
        );
    }
    db_dvr::finish(db, r.id, "completed", None).await?;
    // Texts and age rating for the library.
    let meta = meta_of(&r);
    let nfo = naming::nfo(&meta, &r.channel_name);
    let nfo_files: Vec<PathBuf> = match naming::layout(&meta) {
        naming::Layout::Movie => first
            .parent()
            .map(|d| d.join("movie.nfo"))
            .into_iter()
            .collect(),
        naming::Layout::Episode => (1..=parts)
            .map(|p| naming::file_of_part(&first, p, parts).with_extension("nfo"))
            .collect(),
    };
    for path in nfo_files {
        tokio::fs::write(&path, &nfo).await?;
    }
    Ok(stopped)
}

/// After a recording finished: retention of its rule, then the hook.
async fn finished(inner: &Arc<Inner>, id: i64) {
    let recording = match db_dvr::get(&inner.db, id).await {
        Ok(Some(r)) => r,
        _ => return,
    };
    tracing::info!(
        recording = id,
        bytes = recording.bytes,
        parts = recording.parts,
        "recording finished"
    );
    if let Some(rule_id) = recording.rule_id
        && let Ok(Some(rule)) = db_dvr::get_rule(&inner.db, rule_id).await
        && let Some(keep) = rule.keep_last
    {
        match db_dvr::beyond_keep(&inner.db, rule_id, keep).await {
            Ok(old) => {
                for r in old {
                    tracing::info!(
                        recording = r.id,
                        rule = rule_id,
                        "recording removed (keep last {keep})"
                    );
                    delete_files(&r).await;
                    let _ = db_dvr::delete(&inner.db, r.id).await;
                }
            }
            Err(err) => tracing::warn!(%err, "retention failed"),
        }
    }
    let hook = inner.hook.lock().unwrap().clone();
    if let Some(hook) = hook {
        hook(Some(recording)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_names_round_trip() {
        let first = PathBuf::from("/rec/Filme/A (2026)/A (2026).ts");
        assert_eq!(base_of(&naming::part_path(&first, 1)), first);
        assert_eq!(base_of(&first), first);
    }
}
