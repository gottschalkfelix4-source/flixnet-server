//! Live streams: one ffmpeg per watched channel (and output variant) writes a rolling
//! HLS window into its own directory under `<transcode>/live`; every viewer of that
//! channel shares it.
//!
//! * Watching creates a viewer session (128-bit id = capability for the HLS URLs)
//!   and joins the channel's running stream or starts one — if the tuner host has a
//!   free tuner (streams per host are counted against its tuner count).
//! * Viewers are kept alive by fetching the playlist and segments; a viewer silent for
//!   the viewer timeout is dropped, and a stream without viewers is stopped (process
//!   killed, directory removed). Closing the player ends its viewer at once.
//! * A stream whose ffmpeg exits (the source ended or failed) is removed; its viewers'
//!   next request fails and the player reports the interruption.
//!
//! Timeshift (7.4) builds on this: a stream keeps more segments than the live window
//! and a viewer gets its own position within them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use flixnet_media::MediaInfo;
use flixnet_media::decision::{PlaybackPlan, StreamAction};
use tokio::process::Child;

use crate::ffmpeg::{self, LIVE_PLAYLIST, LiveJob};
use crate::process::command;
use crate::tuner::StreamSource;

#[derive(Debug, thiserror::Error)]
pub enum LiveError {
    #[error("all tuners of this source are in use")]
    TunersBusy,
    #[error("unknown or expired live session")]
    UnknownSession,
    #[error("the live stream ended")]
    Ended,
    #[error("could not start ffmpeg: {0}")]
    Spawn(std::io::Error),
    #[error("the stream could not be opened: {0}")]
    Failed(String),
    #[error("the stream did not start in time")]
    Timeout,
    #[error("the stream could not be analysed: {0}")]
    Probe(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Tunables; the defaults suit real use, tests shorten them.
#[derive(Debug, Clone)]
pub struct LiveSettings {
    pub segment_secs: u32,
    /// Segments in the live playlist.
    pub window: u32,
    /// A stream counts as started once its playlist lists this many segments.
    pub min_segments: usize,
    /// How long starting a stream may take.
    pub start_timeout: Duration,
    /// Viewers without a request for this long are dropped.
    pub viewer_timeout: Duration,
    pub probe_timeout: Duration,
}

impl Default for LiveSettings {
    fn default() -> Self {
        Self {
            segment_secs: 4,
            window: 6,
            min_segments: 2,
            start_timeout: Duration::from_secs(25),
            viewer_timeout: Duration::from_secs(30),
            probe_timeout: Duration::from_secs(15),
        }
    }
}

/// What to watch.
#[derive(Debug, Clone)]
pub struct WatchRequest {
    pub user_id: i64,
    pub channel_id: i64,
    pub tuner_host_id: i64,
    /// Streams the host may run at once (0 = unlimited).
    pub tuner_limit: i64,
    pub source: StreamSource,
    pub plan: PlaybackPlan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchTicket {
    pub session_id: String,
    pub stream_id: String,
    /// Joined a stream that was already running.
    pub shared: bool,
}

/// Snapshot of a running stream (dashboard, tests).
#[derive(Debug, Clone, PartialEq)]
pub struct StreamInfo {
    pub id: String,
    pub channel_id: i64,
    pub tuner_host_id: i64,
    pub started_ms: i64,
    pub video_copied: bool,
    pub audio_copied: bool,
    /// User ids of the viewers (one per session).
    pub viewers: Vec<i64>,
}

#[derive(Clone)]
pub struct LiveTvManager {
    inner: Arc<Inner>,
}

struct Inner {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    root: PathBuf,
    settings: LiveSettings,
    registry: Mutex<Registry>,
}

#[derive(Default)]
struct Registry {
    streams: HashMap<String, Arc<Stream>>,
    viewers: HashMap<String, Viewer>,
}

/// Streams are shared per channel and output variant.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StreamKey {
    channel_id: i64,
    video_copy: bool,
    audio_copy: bool,
    height: i64,
}

impl StreamKey {
    fn new(channel_id: i64, plan: &PlaybackPlan) -> Self {
        let copied = |action: Option<StreamAction>| action != Some(StreamAction::Transcode);
        Self {
            channel_id,
            video_copy: copied(plan.video.as_ref().map(|v| v.action)),
            audio_copy: copied(plan.audio.as_ref().map(|a| a.action)),
            height: plan
                .video
                .as_ref()
                .filter(|v| v.action == StreamAction::Transcode)
                .map_or(0, |v| v.height),
        }
    }
}

struct Stream {
    id: String,
    key: StreamKey,
    tuner_host_id: i64,
    dir: PathBuf,
    started_ms: i64,
    child: Mutex<Option<Child>>,
    /// Set once ffmpeg exited (`true` = success).
    exited: Mutex<Option<bool>>,
}

impl Stream {
    /// Whether ffmpeg is still running (checks without blocking).
    fn alive(&self) -> bool {
        let mut exited = self.exited.lock().unwrap();
        if exited.is_some() {
            return false;
        }
        let mut child = self.child.lock().unwrap();
        match child.as_mut().map(Child::try_wait) {
            Some(Ok(None)) => true,
            Some(Ok(Some(status))) => {
                *exited = Some(status.success());
                false
            }
            Some(Err(_)) | None => {
                *exited = Some(false);
                false
            }
        }
    }

    fn playlist_path(&self) -> PathBuf {
        self.dir.join(LIVE_PLAYLIST)
    }

    fn segments_ready(&self) -> usize {
        std::fs::read_to_string(self.playlist_path())
            .map(|p| ffmpeg::playlist_segments(&p).count())
            .unwrap_or(0)
    }
}

struct Viewer {
    stream: String,
    user_id: i64,
    seen: Instant,
}

impl LiveTvManager {
    /// Clears leftovers in `root` and starts the reaper (needs a Tokio runtime).
    pub fn new(
        ffmpeg: impl Into<PathBuf>,
        ffprobe: impl Into<PathBuf>,
        root: impl Into<PathBuf>,
        settings: LiveSettings,
    ) -> Self {
        let root = root.into();
        if root.exists()
            && let Err(err) = std::fs::remove_dir_all(&root)
        {
            tracing::warn!(path = %root.display(), %err, "could not clear old live streams");
        }
        let inner = Arc::new(Inner {
            ffmpeg: ffmpeg.into(),
            ffprobe: ffprobe.into(),
            root,
            settings,
            registry: Mutex::default(),
        });
        tokio::spawn(reaper(Arc::downgrade(&inner)));
        Self { inner }
    }

    pub fn settings(&self) -> &LiveSettings {
        &self.inner.settings
    }

    /// Reads codecs and size of a stream (ffprobe; takes a few seconds).
    pub async fn probe(&self, source: &StreamSource) -> Result<MediaInfo, LiveError> {
        let child = command(&self.inner.ffprobe)
            .args(ffmpeg::probe_args(source))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(LiveError::Spawn)?;
        let output =
            tokio::time::timeout(self.inner.settings.probe_timeout, child.wait_with_output())
                .await
                .map_err(|_| LiveError::Probe("Zeitüberschreitung".into()))??;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(LiveError::Probe(
                stderr
                    .lines()
                    .last()
                    .unwrap_or("unbekannter Fehler")
                    .trim()
                    .to_owned(),
            ));
        }
        flixnet_media::probe::parse(&output.stdout, Path::new(&source.url))
            .map_err(|e| LiveError::Probe(e.to_string()))
    }

    /// Streams of a tuner host that are running.
    pub fn tuners_in_use(&self, tuner_host_id: i64) -> usize {
        let registry = self.inner.registry.lock().unwrap();
        registry
            .streams
            .values()
            .filter(|s| s.tuner_host_id == tuner_host_id && s.alive())
            .count()
    }

    /// Whether a stream of this channel is running (watching it needs no new tuner).
    pub fn is_streaming(&self, channel_id: i64) -> bool {
        let registry = self.inner.registry.lock().unwrap();
        registry
            .streams
            .values()
            .any(|s| s.key.channel_id == channel_id && s.alive())
    }

    /// Starts watching: joins the channel's stream or starts it, then waits until its
    /// playlist has the first segments.
    pub async fn watch(&self, request: WatchRequest) -> Result<WatchTicket, LiveError> {
        let key = StreamKey::new(request.channel_id, &request.plan);
        let session_id = new_id();
        let (stream, shared) = {
            let mut registry = self.inner.registry.lock().unwrap();
            let existing = registry
                .streams
                .values()
                .find(|s| s.key == key && s.alive())
                .cloned();
            let (stream, shared) = match existing {
                Some(stream) => (stream, true),
                None => {
                    let busy = registry
                        .streams
                        .values()
                        .filter(|s| s.tuner_host_id == request.tuner_host_id && s.alive())
                        .count();
                    if request.tuner_limit > 0 && busy as i64 >= request.tuner_limit {
                        return Err(LiveError::TunersBusy);
                    }
                    let stream = Arc::new(self.spawn(&request, key)?);
                    registry.streams.insert(stream.id.clone(), stream.clone());
                    (stream, false)
                }
            };
            registry.viewers.insert(
                session_id.clone(),
                Viewer {
                    stream: stream.id.clone(),
                    user_id: request.user_id,
                    seen: Instant::now(),
                },
            );
            (stream, shared)
        };

        let deadline = Instant::now() + self.inner.settings.start_timeout;
        loop {
            // A viewer waiting for the start is active (the reaper must not drop it).
            match self.touch(&session_id) {
                Ok(_) => {}
                // Removed meanwhile (the stream ended and the reaper cleaned up).
                Err(_) => return Err(LiveError::Failed(log_tail(&stream.dir))),
            }
            if stream.segments_ready() >= self.inner.settings.min_segments {
                tracing::info!(
                    stream = %stream.id,
                    channel = request.channel_id,
                    shared,
                    "live stream ready"
                );
                return Ok(WatchTicket {
                    session_id,
                    stream_id: stream.id.clone(),
                    shared,
                });
            }
            if !stream.alive() {
                let reason = log_tail(&stream.dir);
                self.drop_viewer(&session_id).await;
                self.remove_stream(&stream.id).await;
                return Err(if busy_tuner(&reason) {
                    LiveError::TunersBusy
                } else {
                    LiveError::Failed(reason)
                });
            }
            if Instant::now() >= deadline {
                self.drop_viewer(&session_id).await;
                return Err(LiveError::Timeout);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    fn spawn(&self, request: &WatchRequest, key: StreamKey) -> Result<Stream, LiveError> {
        let id = new_id();
        let dir = self.inner.root.join(&id);
        std::fs::create_dir_all(&dir)?;
        let log = std::fs::File::create(dir.join("ffmpeg.log"))?;
        let args = ffmpeg::live_hls_args(&LiveJob {
            source: &request.source,
            out_dir: &dir,
            plan: &request.plan,
            segment_secs: self.inner.settings.segment_secs,
            window: self.inner.settings.window,
        });
        let child = command(&self.inner.ffmpeg)
            .current_dir(&dir)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(LiveError::Spawn)?;
        tracing::info!(stream = %id, channel = key.channel_id, video_copy = key.video_copy, audio_copy = key.audio_copy, "live stream starting");
        Ok(Stream {
            id,
            key,
            tuner_host_id: request.tuner_host_id,
            dir,
            started_ms: flixnet_db::now_ms(),
            child: Mutex::new(Some(child)),
            exited: Mutex::new(None),
        })
    }

    /// The stream of a viewer session, marking the viewer as active.
    fn touch(&self, session_id: &str) -> Result<Arc<Stream>, LiveError> {
        let mut registry = self.inner.registry.lock().unwrap();
        let viewer = registry
            .viewers
            .get_mut(session_id)
            .ok_or(LiveError::UnknownSession)?;
        viewer.seen = Instant::now();
        let stream_id = viewer.stream.clone();
        registry
            .streams
            .get(&stream_id)
            .cloned()
            .ok_or(LiveError::UnknownSession)
    }

    /// The live playlist of a viewer session.
    pub fn playlist(&self, session_id: &str) -> Result<String, LiveError> {
        let stream = self.touch(session_id)?;
        if !stream.alive() {
            return Err(LiveError::Ended);
        }
        Ok(std::fs::read_to_string(stream.playlist_path())?)
    }

    /// Path of a segment of a viewer session's stream (`seg{n}.ts`).
    pub fn segment(&self, session_id: &str, name: &str) -> Result<PathBuf, LiveError> {
        if ffmpeg::segment_number(name).is_none() {
            return Err(LiveError::UnknownSession);
        }
        let stream = self.touch(session_id)?;
        let path = stream.dir.join(name);
        if path.is_file() {
            Ok(path)
        } else {
            Err(LiveError::Ended)
        }
    }

    /// The user a viewer session belongs to.
    pub fn owner(&self, session_id: &str) -> Option<i64> {
        self.inner
            .registry
            .lock()
            .unwrap()
            .viewers
            .get(session_id)
            .map(|v| v.user_id)
    }

    /// Ends a viewer session; the stream stops when it was the last viewer.
    pub async fn stop(&self, session_id: &str) {
        self.drop_viewer(session_id).await;
    }

    async fn drop_viewer(&self, session_id: &str) {
        let orphan = {
            let mut registry = self.inner.registry.lock().unwrap();
            let Some(viewer) = registry.viewers.remove(session_id) else {
                return;
            };
            let watched = registry.viewers.values().any(|v| v.stream == viewer.stream);
            if watched {
                None
            } else {
                registry.streams.remove(&viewer.stream)
            }
        };
        if let Some(stream) = orphan {
            close(&stream).await;
        }
    }

    async fn remove_stream(&self, stream_id: &str) {
        let stream = {
            let mut registry = self.inner.registry.lock().unwrap();
            registry.viewers.retain(|_, v| v.stream != stream_id);
            registry.streams.remove(stream_id)
        };
        if let Some(stream) = stream {
            close(&stream).await;
        }
    }

    /// Running streams with their viewers.
    pub fn streams(&self) -> Vec<StreamInfo> {
        let registry = self.inner.registry.lock().unwrap();
        let mut out: Vec<StreamInfo> = registry
            .streams
            .values()
            .map(|s| StreamInfo {
                id: s.id.clone(),
                channel_id: s.key.channel_id,
                tuner_host_id: s.tuner_host_id,
                started_ms: s.started_ms,
                video_copied: s.key.video_copy,
                audio_copied: s.key.audio_copy,
                viewers: registry
                    .viewers
                    .values()
                    .filter(|v| v.stream == s.id)
                    .map(|v| v.user_id)
                    .collect(),
            })
            .collect();
        out.sort_by_key(|s| s.started_ms);
        out
    }

    /// Stops every stream (server shutdown).
    pub async fn stop_all(&self) {
        let streams: Vec<_> = {
            let mut registry = self.inner.registry.lock().unwrap();
            registry.viewers.clear();
            registry.streams.drain().map(|(_, s)| s).collect()
        };
        for stream in streams {
            close(&stream).await;
        }
    }
}

/// Kills ffmpeg and removes the stream's files.
async fn close(stream: &Stream) {
    let child = stream.child.lock().unwrap().take();
    if let Some(mut child) = child {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    if let Err(err) = tokio::fs::remove_dir_all(&stream.dir).await
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(stream = %stream.id, %err, "could not remove live stream files");
    }
    tracing::info!(stream = %stream.id, channel = stream.key.channel_id, "live stream stopped");
}

/// Drops silent viewers, stops streams nobody watches and removes ended ones.
async fn reaper(inner: Weak<Inner>) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tick.tick().await;
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let doomed: Vec<Arc<Stream>> = {
            let mut registry = inner.registry.lock().unwrap();
            let timeout = inner.settings.viewer_timeout;
            registry.viewers.retain(|_, v| v.seen.elapsed() <= timeout);
            let gone: Vec<String> = registry
                .streams
                .values()
                .filter(|s| !registry.viewers.values().any(|v| v.stream == s.id) || !s.alive())
                .map(|s| s.id.clone())
                .collect();
            registry.viewers.retain(|_, v| !gone.contains(&v.stream));
            gone.iter()
                .filter_map(|id| registry.streams.remove(id))
                .collect()
        };
        for stream in doomed {
            close(&stream).await;
        }
    }
}

/// The last lines ffmpeg logged.
fn log_tail(dir: &Path) -> String {
    let log = std::fs::read_to_string(dir.join("ffmpeg.log")).unwrap_or_default();
    let lines: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(3)..].join(" | ")
}

/// HDHomeRun answers 503 when all its tuners are taken (also by other apps).
fn busy_tuner(log: &str) -> bool {
    log.contains("503 Service Unavailable") || log.contains("Server returned 503")
}

/// 128 random bits as hex.
fn new_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("operating system random source");
    hex::encode(bytes)
}

/// Accepts only ids this module could have made.
pub fn is_session_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
