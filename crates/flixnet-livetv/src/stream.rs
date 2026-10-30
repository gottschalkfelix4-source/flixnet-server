//! Live streams: one ffmpeg per watched channel (and output variant) writes MPEG-TS
//! segments into its own directory under `<transcode>/live`; every viewer of that
//! channel — and every recording of it — shares them.
//!
//! * Watching creates a viewer session (128-bit id = capability for the HLS URLs)
//!   and joins the channel's running stream or starts one — if the tuner host has a
//!   free tuner (streams per host are counted against its tuner count).
//! * **Timeshift:** the manager indexes the segments ffmpeg completes and keeps them
//!   for the timeshift buffer (default 2 h) instead of a short live window. Every
//!   viewer gets the whole buffer as a live playlist (a DVR window), so its player
//!   pauses and rewinds within it on its own and jumps back to the live edge at will;
//!   the buffer belongs to the stream, so viewers joining later see it too.
//! * **Restarts:** when ffmpeg exits (the source dropped, the provider hiccupped) the
//!   stream is restarted with backoff and continues the segment numbering behind a
//!   discontinuity; viewers keep their buffer and never see "stream ended" for a
//!   short outage. Only a stream that never produced anything, or one that keeps
//!   failing without a new segment, is given up. A run on a GPU encoder that fails
//!   before producing anything is retried on the next simpler encoder.
//! * Viewers are kept alive by fetching the playlist and segments (a paused player
//!   keeps refreshing the playlist); a viewer silent for the viewer timeout is
//!   dropped, and a stream without viewers is stopped (process killed, directory
//!   removed). Recordings join as *pinned* viewers that never time out.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use flixnet_media::MediaInfo;
use flixnet_media::decision::{PlaybackPlan, StreamAction};
use flixnet_media::hardware::{EncoderKind, VideoEncoder};
use flixnet_media::transcode::TranscodeManager;
use tokio::process::Child;

use crate::ffmpeg::{self, FFMPEG_PLAYLIST, LiveJob};
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
    /// Segments in ffmpeg's own playlist, and the least the manager keeps even with
    /// timeshift switched off.
    pub window: u32,
    /// A stream counts as started once this many segments exist.
    pub min_segments: usize,
    /// How long starting a stream may take.
    pub start_timeout: Duration,
    /// Viewers without a request for this long are dropped.
    pub viewer_timeout: Duration,
    pub probe_timeout: Duration,
    /// Timeshift buffer per stream (changeable at runtime, see
    /// [`LiveTvManager::set_buffer`]).
    pub buffer: Duration,
    /// Wait before the first restart of a failed upstream; doubles per failure.
    pub restart_delay: Duration,
    pub restart_max_delay: Duration,
    /// Restarts in a row without a new segment before the stream is given up.
    pub max_restarts: u32,
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
            buffer: Duration::from_secs(2 * 60 * 60),
            restart_delay: Duration::from_secs(1),
            restart_max_delay: Duration::from_secs(30),
            max_restarts: 8,
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
    /// User ids of the viewers (one per session; recordings excluded).
    pub viewers: Vec<i64>,
    /// Recordings fed by this stream.
    pub recordings: usize,
    /// Seconds of timeshift buffer on disk.
    pub buffered_secs: f64,
    /// Times the upstream was restarted after a failure.
    pub restarts: u32,
    /// Encoder of the current run (software for copied video).
    pub encoder: EncoderKind,
}

/// A completed segment of a stream, for recordings.
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentFile {
    pub seq: u64,
    pub path: PathBuf,
    /// The upstream restarted before this segment.
    pub discontinuity: bool,
    /// Wall-clock time the segment starts at (unix ms, approximate).
    pub start_ms: i64,
    pub duration: f64,
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
    buffer: Mutex<Duration>,
    /// Where transcodes get their encoder from (none: software).
    transcoder: Mutex<Option<TranscodeManager>>,
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
    source: StreamSource,
    plan: PlaybackPlan,
    /// Video encoder and its fallbacks, most capable first.
    encoders: Vec<VideoEncoder>,
    state: Mutex<StreamState>,
}

#[derive(Default)]
struct StreamState {
    child: Option<Child>,
    /// Index into the stream's encoders.
    level: usize,
    runs: u32,
    /// Restarts since the last run that produced a segment.
    failures: u32,
    /// Restarts so far (dashboard).
    restarts: u32,
    run_first_seq: u64,
    run_produced: bool,
    restart_at: Option<Instant>,
    segments: VecDeque<Segment>,
    next_seq: u64,
    /// Discontinuities that left the buffer (`EXT-X-DISCONTINUITY-SEQUENCE`).
    dropped_discontinuities: u64,
    /// Had enough segments once (a start that never got there is not restarted).
    ready: bool,
    /// Given up; the reaper removes the stream.
    ended: bool,
}

#[derive(Debug, Clone)]
struct Segment {
    seq: u64,
    duration: f64,
    discontinuity: bool,
    start_ms: i64,
}

impl Stream {
    fn segment_path(&self, seq: u64) -> PathBuf {
        self.dir.join(format!("seg{seq}.ts"))
    }

    fn alive(&self) -> bool {
        !self.state.lock().unwrap().ended
    }
}

struct Viewer {
    stream: String,
    user_id: i64,
    seen: Instant,
    /// Recordings: never timed out.
    pinned: bool,
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
            buffer: Mutex::new(settings.buffer),
            settings,
            transcoder: Mutex::default(),
            registry: Mutex::default(),
        });
        tokio::spawn(reaper(Arc::downgrade(&inner)));
        Self { inner }
    }

    pub fn settings(&self) -> &LiveSettings {
        &self.inner.settings
    }

    /// Transcodes use the encoder this manager selected (GPU when detection found
    /// one), with software as the fallback.
    pub fn use_encoders_of(&self, transcoder: TranscodeManager) {
        *self.inner.transcoder.lock().unwrap() = Some(transcoder);
    }

    /// The timeshift buffer per stream.
    pub fn buffer(&self) -> Duration {
        *self.inner.buffer.lock().unwrap()
    }

    /// Changes the timeshift buffer (running streams trim to it within a second).
    pub fn set_buffer(&self, buffer: Duration) {
        *self.inner.buffer.lock().unwrap() = buffer;
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

    /// Streams of a tuner host that are running (restarting ones included: they keep
    /// their tuner).
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

    /// Starts watching: joins the channel's stream or starts it, then waits until it
    /// has the first segments.
    pub async fn watch(&self, request: WatchRequest) -> Result<WatchTicket, LiveError> {
        self.join(request, false).await
    }

    /// Like [`Self::watch`] for a recording: the session never times out (end it with
    /// [`Self::stop`]).
    pub async fn watch_pinned(&self, request: WatchRequest) -> Result<WatchTicket, LiveError> {
        self.join(request, true).await
    }

    async fn join(&self, request: WatchRequest, pinned: bool) -> Result<WatchTicket, LiveError> {
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
                    pinned,
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
            let (ready, ended) = self.inner.maintain(&stream);
            if ready {
                tracing::info!(
                    stream = %stream.id,
                    channel = request.channel_id,
                    shared,
                    pinned,
                    "live stream ready"
                );
                return Ok(WatchTicket {
                    session_id,
                    stream_id: stream.id.clone(),
                    shared,
                });
            }
            if ended {
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
        let encoders = if key.video_copy {
            vec![VideoEncoder::Software]
        } else {
            let selected = self
                .inner
                .transcoder
                .lock()
                .unwrap()
                .as_ref()
                .map(TranscodeManager::encoder)
                .unwrap_or_default();
            selected.fallbacks(None)
        };
        let stream = Stream {
            id,
            key,
            tuner_host_id: request.tuner_host_id,
            dir,
            started_ms: flixnet_db::now_ms(),
            source: request.source.clone(),
            plan: request.plan.clone(),
            encoders,
            state: Mutex::default(),
        };
        {
            let mut state = stream.state.lock().unwrap();
            self.inner.start_run(&stream, &mut state)?;
        }
        tracing::info!(
            stream = %stream.id,
            channel = stream.key.channel_id,
            video_copy = stream.key.video_copy,
            audio_copy = stream.key.audio_copy,
            encoder = ?stream.encoders[0].kind(),
            "live stream starting"
        );
        Ok(stream)
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

    /// The playlist of a viewer session: the whole timeshift buffer as a live
    /// playlist (`EXT-X-PROGRAM-DATE-TIME` gives each segment's wall-clock time).
    pub fn playlist(&self, session_id: &str) -> Result<String, LiveError> {
        let stream = self.touch(session_id)?;
        let buffer = self.buffer();
        let mut state = stream.state.lock().unwrap();
        if state.ended {
            return Err(LiveError::Ended);
        }
        self.inner.sync(&stream, &mut state, buffer);
        Ok(render_playlist(
            &state,
            self.inner.settings.segment_secs.max(1),
        ))
    }

    /// Path of a segment of a viewer session's stream (`seg{n}.ts`), while it is in
    /// the buffer.
    pub fn segment(&self, session_id: &str, name: &str) -> Result<PathBuf, LiveError> {
        let Some(seq) = ffmpeg::segment_number(name) else {
            return Err(LiveError::UnknownSession);
        };
        let stream = self.touch(session_id)?;
        let state = stream.state.lock().unwrap();
        if state.segments.iter().any(|s| s.seq == seq) {
            Ok(stream.segment_path(seq))
        } else {
            Err(LiveError::Ended)
        }
    }

    /// Segments of a session's stream for a recording: those after `after`, or —
    /// for the first call — those that end after `from_ms` (the buffer may already
    /// hold the beginning of the program).
    pub fn segments_since(
        &self,
        session_id: &str,
        after: Option<u64>,
        from_ms: i64,
    ) -> Result<Vec<SegmentFile>, LiveError> {
        let stream = self.touch(session_id)?;
        let buffer = self.buffer();
        let mut state = stream.state.lock().unwrap();
        if state.ended {
            return Err(LiveError::Ended);
        }
        self.inner.sync(&stream, &mut state, buffer);
        Ok(state
            .segments
            .iter()
            .filter(|s| match after {
                Some(after) => s.seq > after,
                None => s.start_ms + (s.duration * 1000.0) as i64 > from_ms,
            })
            .map(|s| SegmentFile {
                seq: s.seq,
                path: stream.segment_path(s.seq),
                discontinuity: s.discontinuity,
                start_ms: s.start_ms,
                duration: s.duration,
            })
            .collect())
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

    /// Frees a tuner of a host for a recording: stops the live-only stream (no
    /// recording on it) with the fewest viewers. Returns whether one was stopped.
    pub async fn preempt(&self, tuner_host_id: i64) -> bool {
        let victim = {
            let registry = self.inner.registry.lock().unwrap();
            registry
                .streams
                .values()
                .filter(|s| s.tuner_host_id == tuner_host_id && s.alive())
                .filter(|s| {
                    !registry
                        .viewers
                        .values()
                        .any(|v| v.stream == s.id && v.pinned)
                })
                .min_by_key(|s| {
                    registry
                        .viewers
                        .values()
                        .filter(|v| v.stream == s.id)
                        .count()
                })
                .map(|s| s.id.clone())
        };
        match victim {
            Some(id) => {
                tracing::info!(stream = %id, tuner = tuner_host_id, "live stream stopped for a recording");
                self.remove_stream(&id).await;
                true
            }
            None => false,
        }
    }

    /// Ends the current ffmpeg run of a stream as if the source had failed (the
    /// supervisor restarts it). For diagnostics and tests.
    pub fn interrupt(&self, stream_id: &str) -> bool {
        let stream = self
            .inner
            .registry
            .lock()
            .unwrap()
            .streams
            .get(stream_id)
            .cloned();
        let Some(stream) = stream else {
            return false;
        };
        let mut state = stream.state.lock().unwrap();
        match state.child.as_mut() {
            Some(child) => child.start_kill().is_ok(),
            None => false,
        }
    }

    /// Running streams with their viewers.
    pub fn streams(&self) -> Vec<StreamInfo> {
        let registry = self.inner.registry.lock().unwrap();
        let mut out: Vec<StreamInfo> = registry
            .streams
            .values()
            .map(|s| {
                let state = s.state.lock().unwrap();
                let viewers = registry.viewers.values().filter(|v| v.stream == s.id);
                StreamInfo {
                    id: s.id.clone(),
                    channel_id: s.key.channel_id,
                    tuner_host_id: s.tuner_host_id,
                    started_ms: s.started_ms,
                    video_copied: s.key.video_copy,
                    audio_copied: s.key.audio_copy,
                    viewers: viewers
                        .clone()
                        .filter(|v| !v.pinned)
                        .map(|v| v.user_id)
                        .collect(),
                    recordings: viewers.filter(|v| v.pinned).count(),
                    buffered_secs: state.segments.iter().map(|s| s.duration).sum(),
                    restarts: state.restarts,
                    encoder: s.encoders[state.level.min(s.encoders.len() - 1)].kind(),
                }
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

impl Inner {
    /// Starts an ffmpeg run continuing the stream's segment numbering.
    fn start_run(&self, stream: &Stream, state: &mut StreamState) -> Result<(), LiveError> {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(stream.dir.join("ffmpeg.log"))?;
        let encoder = &stream.encoders[state.level.min(stream.encoders.len() - 1)];
        let args = ffmpeg::live_hls_args(&LiveJob {
            source: &stream.source,
            out_dir: &stream.dir,
            plan: &stream.plan,
            segment_secs: self.settings.segment_secs,
            list_size: (self.settings.window * 3).max(30),
            start_number: state.next_seq,
            encoder,
        });
        let child = command(&self.ffmpeg)
            .current_dir(&stream.dir)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(LiveError::Spawn)?;
        state.child = Some(child);
        state.runs += 1;
        state.run_first_seq = state.next_seq;
        state.run_produced = false;
        Ok(())
    }

    /// Indexes segments ffmpeg completed and trims the buffer to `buffer`.
    fn sync(&self, stream: &Stream, state: &mut StreamState, buffer: Duration) {
        if let Ok(text) = std::fs::read_to_string(stream.dir.join(FFMPEG_PLAYLIST)) {
            let fresh: Vec<(u64, f64)> = ffmpeg::playlist_entries(&text)
                .into_iter()
                .filter(|(n, _)| *n >= state.next_seq && *n >= state.run_first_seq)
                .filter(|(n, _)| stream.segment_path(*n).is_file())
                .collect();
            if !fresh.is_empty() {
                let now = flixnet_db::now_ms();
                // Wall-clock times: continuing the previous segment, or counted back
                // from now for the first segments of a run.
                let batch_ms: f64 = fresh.iter().map(|(_, d)| d * 1000.0).sum();
                let mut backwards = now - batch_ms as i64;
                for (n, duration) in fresh {
                    let discontinuity = state.runs > 1 && n == state.run_first_seq;
                    let start_ms = match state.segments.back() {
                        Some(prev) if prev.seq + 1 == n && !discontinuity => {
                            prev.start_ms + (prev.duration * 1000.0) as i64
                        }
                        _ => backwards,
                    };
                    backwards = start_ms + (duration * 1000.0) as i64;
                    state.segments.push_back(Segment {
                        seq: n,
                        duration,
                        discontinuity,
                        start_ms,
                    });
                    state.next_seq = n + 1;
                    state.run_produced = true;
                }
            }
        }
        // Keep the buffer (and at least the live window).
        let keep = buffer.as_secs_f64();
        let min_keep = self.settings.window.max(2) as usize;
        let mut total: f64 = state.segments.iter().map(|s| s.duration).sum();
        while state.segments.len() > min_keep {
            let front = &state.segments[0];
            if total - front.duration < keep {
                break;
            }
            total -= front.duration;
            let front = state.segments.pop_front().expect("non-empty");
            if front.discontinuity {
                state.dropped_discontinuities += 1;
            }
            let _ = std::fs::remove_file(stream.segment_path(front.seq));
        }
        if state.segments.len() >= self.settings.min_segments {
            state.ready = true;
        }
    }

    /// Indexes new segments and supervises ffmpeg: restarts a failed run with
    /// backoff (or on a simpler encoder), or gives the stream up. Returns
    /// `(ready, ended)`.
    fn maintain(&self, stream: &Stream) -> (bool, bool) {
        let buffer = *self.buffer.lock().unwrap();
        let mut state = stream.state.lock().unwrap();
        if state.ended {
            return (state.ready, true);
        }
        self.sync(stream, &mut state, buffer);

        let exited = match state.child.as_mut().map(Child::try_wait) {
            Some(Ok(None)) => return (state.ready, false),
            Some(Ok(Some(status))) => Some(status.success()),
            Some(Err(_)) => Some(false),
            None => None,
        };
        if let Some(success) = exited {
            state.child = None;
            let can_fall_back = !state.run_produced && state.level + 1 < stream.encoders.len();
            if !state.ready {
                if can_fall_back {
                    tracing::warn!(stream = %stream.id, "live transcode failed on the GPU; retrying in software");
                    state.level += 1;
                    state.restart_at = Some(Instant::now());
                } else {
                    state.ended = true;
                    return (false, true);
                }
            } else {
                if state.run_produced {
                    state.failures = 0;
                } else {
                    state.failures += 1;
                }
                if can_fall_back {
                    state.level += 1;
                }
                if state.failures >= self.settings.max_restarts {
                    tracing::warn!(stream = %stream.id, failures = state.failures, "live stream given up");
                    state.ended = true;
                    return (true, true);
                }
                let delay = self
                    .settings
                    .restart_delay
                    .saturating_mul(1 << state.failures.min(16))
                    .min(self.settings.restart_max_delay);
                tracing::info!(
                    stream = %stream.id,
                    success,
                    delay_ms = delay.as_millis() as u64,
                    "live upstream stopped; restarting"
                );
                state.restart_at = Some(Instant::now() + delay);
            }
        }
        if state.child.is_none() && state.restart_at.is_some_and(|at| Instant::now() >= at) {
            state.restart_at = None;
            match self.start_run(stream, &mut state) {
                Ok(()) => state.restarts += 1,
                Err(err) => {
                    tracing::warn!(stream = %stream.id, %err, "live stream could not be restarted");
                    state.ended = true;
                    return (state.ready, true);
                }
            }
        }
        (state.ready, false)
    }
}

/// The DVR-window live playlist of the buffered segments.
fn render_playlist(state: &StreamState, segment_secs: u32) -> String {
    use std::fmt::Write as _;
    let target = state
        .segments
        .iter()
        .map(|s| s.duration.ceil() as u64)
        .max()
        .unwrap_or(0)
        .max(u64::from(segment_secs));
    let first = state.segments.front().map_or(state.next_seq, |s| s.seq);
    let mut out = String::with_capacity(64 + state.segments.len() * 80);
    let _ = write!(
        out,
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{target}\n#EXT-X-MEDIA-SEQUENCE:{first}\n#EXT-X-DISCONTINUITY-SEQUENCE:{}\n",
        state.dropped_discontinuities
    );
    for (i, s) in state.segments.iter().enumerate() {
        if s.discontinuity && i > 0 {
            out.push_str("#EXT-X-DISCONTINUITY\n");
        }
        if i == 0 || s.discontinuity {
            let _ = writeln!(out, "#EXT-X-PROGRAM-DATE-TIME:{}", iso8601(s.start_ms));
        }
        let _ = write!(out, "#EXTINF:{:.3},\nseg{}.ts\n", s.duration, s.seq);
    }
    out
}

/// `2026-10-30T19:15:00.000Z`
pub fn iso8601(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// H. Hinnant's civil_from_days: days since 1970-01-01 → (year, month, day).
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// Kills ffmpeg and removes the stream's files.
async fn close(stream: &Stream) {
    let child = stream.state.lock().unwrap().child.take();
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

/// Supervises streams, drops silent viewers, stops streams nobody watches and
/// removes given-up ones.
async fn reaper(inner: Weak<Inner>) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tick.tick().await;
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let streams: Vec<Arc<Stream>> = inner
            .registry
            .lock()
            .unwrap()
            .streams
            .values()
            .cloned()
            .collect();
        for stream in &streams {
            inner.maintain(stream);
        }
        let doomed: Vec<Arc<Stream>> = {
            let mut registry = inner.registry.lock().unwrap();
            let timeout = inner.settings.viewer_timeout;
            registry
                .viewers
                .retain(|_, v| v.pinned || v.seen.elapsed() <= timeout);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(seq: u64, discontinuity: bool) -> Segment {
        Segment {
            seq,
            duration: 4.0,
            discontinuity,
            start_ms: 1_793_000_000_000 + seq as i64 * 4000,
        }
    }

    #[test]
    fn playlist_lists_the_whole_buffer_with_discontinuities() {
        let state = StreamState {
            segments: [
                segment(10, false),
                segment(11, false),
                segment(12, true),
                segment(13, false),
            ]
            .into(),
            dropped_discontinuities: 2,
            next_seq: 14,
            ..StreamState::default()
        };
        let playlist = render_playlist(&state, 4);
        assert_eq!(
            playlist,
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:10\n\
             #EXT-X-DISCONTINUITY-SEQUENCE:2\n\
             #EXT-X-PROGRAM-DATE-TIME:2026-10-26T07:34:00.000Z\n#EXTINF:4.000,\nseg10.ts\n\
             #EXTINF:4.000,\nseg11.ts\n\
             #EXT-X-DISCONTINUITY\n#EXT-X-PROGRAM-DATE-TIME:2026-10-26T07:34:08.000Z\n#EXTINF:4.000,\nseg12.ts\n\
             #EXTINF:4.000,\nseg13.ts\n"
        );
        assert!(!playlist.contains("ENDLIST"), "a live playlist");
    }

    #[test]
    fn iso_dates() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601(951_782_400_123), "2000-02-29T00:00:00.123Z");
    }
}
