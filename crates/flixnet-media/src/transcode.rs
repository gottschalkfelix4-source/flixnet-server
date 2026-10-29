//! Transcode sessions: one directory and at most one ffmpeg process per session.
//!
//! * Segments are produced on demand. A request outside what the running ffmpeg will
//!   reach soon (a seek) restarts it at that segment; finished segments stay usable.
//! * When ffmpeg runs too far ahead of the player it is stopped; the next request
//!   past its output simply starts it again there.
//! * Sessions nobody asked for anything within the idle timeout are reaped: process
//!   killed, directory removed. Closing the player also stops its session.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use tokio::process::Child;

use crate::decision::PlaybackPlan;
use crate::hls::{self, FFMPEG_PLAYLIST, HlsJob, INIT_FILE, Segments};
use crate::process::command;

#[derive(Debug, thiserror::Error)]
pub enum TranscodeError {
    #[error("unknown or expired transcode session")]
    UnknownSession,
    #[error("segment {0} is outside the media")]
    OutOfRange(u32),
    #[error("could not start ffmpeg: {0}")]
    Spawn(std::io::Error),
    #[error("ffmpeg failed: {0}")]
    Failed(String),
    #[error("timed out waiting for segment {0}")]
    Timeout(u32),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// What a session plays.
#[derive(Debug, Clone)]
pub struct SessionSpec {
    pub user_id: i64,
    pub item_id: i64,
    pub file_id: i64,
    pub input: PathBuf,
    /// Fixed for transcodes, the keyframe index for copied video.
    pub segments: Segments,
    pub plan: PlaybackPlan,
}

/// Tunables; the defaults suit real use, tests shorten them.
#[derive(Debug, Clone)]
pub struct TranscodeSettings {
    /// Stop a session nobody touched for this long.
    pub idle_timeout: Duration,
    /// Wait at most this long for one segment.
    pub segment_timeout: Duration,
    /// Requests up to this many segments past ffmpeg's output wait instead of restarting.
    pub lookahead: u32,
    /// Stop ffmpeg once it is this many segments ahead of the player.
    pub max_ahead: u32,
}

impl Default for TranscodeSettings {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(60),
            segment_timeout: Duration::from_secs(30),
            lookahead: 4,
            max_ahead: 40,
        }
    }
}

/// Snapshot of a session for dashboards and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: String,
    pub user_id: i64,
    pub item_id: i64,
    pub file_id: i64,
    /// An ffmpeg process is running.
    pub transcoding: bool,
    pub last_requested: Option<u32>,
}

#[derive(Clone)]
pub struct TranscodeManager {
    inner: Arc<Inner>,
}

struct Inner {
    ffmpeg: PathBuf,
    root: PathBuf,
    settings: TranscodeSettings,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
}

struct Session {
    id: String,
    spec: SessionSpec,
    dir: PathBuf,
    /// Held only briefly (checks and spawning), never across waits.
    state: tokio::sync::Mutex<State>,
    touched: Mutex<Instant>,
    /// Segment the player will start with (resume); the first run begins there.
    start_hint: AtomicU32,
}

#[derive(Default)]
struct State {
    run: Option<Run>,
    /// Segments known to be complete on disk, from any run.
    done: BTreeSet<u32>,
    last_requested: Option<u32>,
}

struct Run {
    child: Child,
    start: u32,
    /// Highest segment this run completed.
    produced: Option<u32>,
    exited: Option<bool>,
}

impl TranscodeManager {
    /// Creates the manager, clears leftovers of earlier runs in `root` and starts the
    /// reaper (needs a Tokio runtime).
    pub fn new(
        ffmpeg: impl Into<PathBuf>,
        root: impl Into<PathBuf>,
        settings: TranscodeSettings,
    ) -> Self {
        let root = root.into();
        // Leftovers of an earlier run. Only the contents: the root may be a mount point.
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                let path = entry.path();
                let removed = if path.is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                if let Err(err) = removed {
                    tracing::warn!(path = %path.display(), %err, "could not clear old transcode files");
                }
            }
        }
        let inner = Arc::new(Inner {
            ffmpeg: ffmpeg.into(),
            root,
            settings,
            sessions: Mutex::default(),
        });
        tokio::spawn(reaper(Arc::downgrade(&inner)));
        Self { inner }
    }

    /// Registers a session; ffmpeg starts with the first request.
    pub fn create(&self, spec: SessionSpec) -> String {
        let id = new_id();
        let session = Arc::new(Session {
            dir: self.inner.root.join(&id),
            id: id.clone(),
            spec,
            state: tokio::sync::Mutex::default(),
            touched: Mutex::new(Instant::now()),
            start_hint: AtomicU32::new(0),
        });
        self.inner
            .sessions
            .lock()
            .unwrap()
            .insert(id.clone(), session);
        id
    }

    fn session(&self, id: &str) -> Result<Arc<Session>, TranscodeError> {
        let session = self
            .inner
            .sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or(TranscodeError::UnknownSession)?;
        *session.touched.lock().unwrap() = Instant::now();
        Ok(session)
    }

    /// Announces where the player will start (a resume position): the first ffmpeg run
    /// begins at that segment instead of transcoding from the beginning first.
    pub fn hint_start(&self, id: &str, position_ms: i64) {
        let sessions = self.inner.sessions.lock().unwrap();
        let Some(session) = sessions.get(id) else {
            return;
        };
        let segments = &session.spec.segments;
        let index = (1..segments.count())
            .take_while(|i| segments.start_ms(*i) <= position_ms)
            .last()
            .unwrap_or(0);
        session.start_hint.store(index, Ordering::Relaxed);
    }

    /// The user a session belongs to.
    pub fn owner(&self, id: &str) -> Option<i64> {
        self.inner
            .sessions
            .lock()
            .unwrap()
            .get(id)
            .map(|s| s.spec.user_id)
    }

    /// The full VOD playlist.
    pub fn playlist(&self, id: &str) -> Result<String, TranscodeError> {
        let session = self.session(id)?;
        Ok(hls::vod_playlist(&session.spec.segments))
    }

    /// Path of the init segment, once a run has written it.
    pub async fn init(&self, id: &str) -> Result<PathBuf, TranscodeError> {
        let session = self.session(id)?;
        let path = session.dir.join(INIT_FILE);
        let deadline = Instant::now() + self.inner.settings.segment_timeout;
        // Players usually ask for the init segment and the first media segment together;
        // give that request a moment to start ffmpeg at the right place.
        let grace = Instant::now() + Duration::from_millis(1500);
        loop {
            {
                let mut state = session.state.lock().await;
                refresh(&session, &mut state);
                let complete = state
                    .run
                    .as_ref()
                    .is_some_and(|r| r.produced.is_some() || r.exited.is_some())
                    || !state.done.is_empty();
                if complete && path.exists() {
                    return Ok(path);
                }
                if let Some(Run {
                    exited: Some(false),
                    ..
                }) = &state.run
                {
                    return Err(failure(&session.dir));
                }
                // With a start hint the player's first segment is known: no need to wait.
                let hint = session.start_hint.load(Ordering::Relaxed);
                if state.run.is_none() && (hint > 0 || Instant::now() >= grace) {
                    self.start(&session, &mut state, hint)?;
                }
            }
            if Instant::now() >= deadline {
                return Err(TranscodeError::Timeout(0));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Path of segment `index`, transcoding it first if needed.
    pub async fn segment(&self, id: &str, index: u32) -> Result<PathBuf, TranscodeError> {
        let session = self.session(id)?;
        if index >= session.spec.segments.count() {
            return Err(TranscodeError::OutOfRange(index));
        }
        let path = session.dir.join(hls::segment_file(index));
        let deadline = Instant::now() + self.inner.settings.segment_timeout;
        loop {
            {
                let mut state = session.state.lock().await;
                state.last_requested = Some(index);
                refresh(&session, &mut state);
                if state.done.contains(&index) && path.exists() {
                    return Ok(path);
                }
                let restart = match &state.run {
                    None => true,
                    Some(run) => {
                        let reach = run.produced.map_or(run.start, |p| p + 1)
                            + self.inner.settings.lookahead;
                        match run.exited {
                            // A finished run that didn't produce this segment won't anymore.
                            Some(true) => true,
                            Some(false) => return Err(failure(&session.dir)),
                            None => index < run.start || index > reach,
                        }
                    }
                };
                if restart {
                    self.start(&session, &mut state, index)?;
                }
            }
            if Instant::now() >= deadline {
                return Err(TranscodeError::Timeout(index));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn start(
        &self,
        session: &Session,
        state: &mut State,
        segment: u32,
    ) -> Result<(), TranscodeError> {
        stop_run(state);
        std::fs::create_dir_all(&session.dir)?;
        let log = std::fs::File::create(session.dir.join("ffmpeg.log"))?;
        // Each run lists only its own segments; don't let an old list mislead us.
        let _ = std::fs::remove_file(session.dir.join(FFMPEG_PLAYLIST));
        let args = hls::hls_args(&HlsJob {
            input: &session.spec.input,
            out_dir: &session.dir,
            start_segment: segment,
            segments: &session.spec.segments,
            plan: &session.spec.plan,
        });
        // ffmpeg resolves the init segment's name against its working directory.
        let child = command(&self.inner.ffmpeg)
            .current_dir(&session.dir)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(TranscodeError::Spawn)?;
        tracing::debug!(session = %session.id, segment, "ffmpeg started");
        state.run = Some(Run {
            child,
            start: segment,
            produced: None,
            exited: None,
        });
        Ok(())
    }

    /// Stops a session: kills ffmpeg and removes its files.
    pub async fn stop(&self, id: &str) {
        let session = self.inner.sessions.lock().unwrap().remove(id);
        if let Some(session) = session {
            close(&session).await;
        }
    }

    /// Stops every session (server shutdown).
    pub async fn stop_all(&self) {
        let sessions: Vec<_> = self
            .inner
            .sessions
            .lock()
            .unwrap()
            .drain()
            .map(|(_, s)| s)
            .collect();
        for session in sessions {
            close(&session).await;
        }
    }

    pub async fn sessions(&self) -> Vec<SessionInfo> {
        let sessions: Vec<_> = self
            .inner
            .sessions
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        let mut out = Vec::with_capacity(sessions.len());
        for session in sessions {
            let mut state = session.state.lock().await;
            refresh(&session, &mut state);
            out.push(SessionInfo {
                id: session.id.clone(),
                user_id: session.spec.user_id,
                item_id: session.spec.item_id,
                file_id: session.spec.file_id,
                transcoding: state.run.as_ref().is_some_and(|r| r.exited.is_none()),
                last_requested: state.last_requested,
            });
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }
}

/// Picks up what the current run finished and whether it exited.
fn refresh(session: &Session, state: &mut State) {
    let Some(run) = state.run.as_mut() else {
        return;
    };
    if run.exited.is_none()
        && let Ok(Some(status)) = run.child.try_wait()
    {
        run.exited = Some(status.success());
    }
    if let Ok(list) = std::fs::read_to_string(session.dir.join(FFMPEG_PLAYLIST)) {
        for segment in hls::completed_segments(&list) {
            state.done.insert(segment);
            run.produced = run.produced.max(Some(segment));
        }
    }
}

fn stop_run(state: &mut State) {
    if let Some(mut run) = state.run.take()
        && run.exited.is_none()
    {
        let _ = run.child.start_kill();
    }
}

async fn close(session: &Session) {
    let mut state = session.state.lock().await;
    if let Some(mut run) = state.run.take() {
        let _ = run.child.start_kill();
        let _ = run.child.wait().await;
    }
    drop(state);
    if let Err(err) = tokio::fs::remove_dir_all(&session.dir).await
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(session = %session.id, %err, "could not remove transcode files");
    }
    tracing::debug!(session = %session.id, "transcode session closed");
}

/// The last lines ffmpeg wrote to its log.
fn failure(dir: &Path) -> TranscodeError {
    let log = std::fs::read_to_string(dir.join("ffmpeg.log")).unwrap_or_default();
    let tail: Vec<_> = log.lines().rev().take(5).collect();
    TranscodeError::Failed(tail.into_iter().rev().collect::<Vec<_>>().join(" | "))
}

/// Reaps idle sessions and stops runs that got too far ahead.
async fn reaper(inner: Weak<Inner>) {
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tick.tick().await;
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let now = Instant::now();
        let (idle, active): (Vec<_>, Vec<_>) = {
            let mut sessions = inner.sessions.lock().unwrap();
            let idle: Vec<String> = sessions
                .iter()
                .filter(|(_, s)| {
                    now.duration_since(*s.touched.lock().unwrap()) > inner.settings.idle_timeout
                })
                .map(|(id, _)| id.clone())
                .collect();
            let idle = idle.iter().filter_map(|id| sessions.remove(id)).collect();
            (idle, sessions.values().cloned().collect())
        };
        for session in idle {
            tracing::info!(session = %session.id, "stopping idle transcode session");
            close(&session).await;
        }
        for session in active {
            let mut state = session.state.lock().await;
            refresh(&session, &mut state);
            let ahead = match (&state.run, state.last_requested) {
                (Some(run), Some(requested)) if run.exited.is_none() => run
                    .produced
                    .is_some_and(|p| p > requested + inner.settings.max_ahead),
                _ => false,
            };
            if ahead {
                tracing::debug!(session = %session.id, "ffmpeg far ahead of the player; pausing");
                stop_run(&mut state);
            }
        }
    }
}

/// 128 random bits as hex; the id is the capability for the session's URLs.
fn new_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("operating system random source");
    hex::encode(bytes)
}

/// Accepts only ids this module could have made (keeps paths and logs clean).
pub fn is_session_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_are_random_hex() {
        let a = new_id();
        assert!(is_session_id(&a));
        assert_ne!(a, new_id());
        assert!(!is_session_id("../../etc/passwd"));
        assert!(!is_session_id(&a.to_uppercase()));
    }
}
