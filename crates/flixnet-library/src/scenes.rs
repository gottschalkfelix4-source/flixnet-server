//! Scene search index: subtitle text of every file in time-stamped chunks, plus an
//! opt-in Whisper transcription (per library) for files without text subtitles.
//!
//! Both jobs run in the `ai` pool at backfill priority and never while someone is
//! watching a transcode: they wait until the transcoder is idle, and a running
//! Whisper process is stopped (and restarted later) as soon as a transcode begins.
//! Extraction and transcription happen outside any transaction; only the finished
//! chunks are written, in one short transaction per source.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use flixnet_core::jobs::{JobContext, JobHandler, JobOptions, JobQueue, Pool, priority};
use flixnet_core::scheduler::TaskDef;
use flixnet_db::transcripts::{self, NewChunk, RUN_MARKER, SOURCE_SUBTITLE, SOURCE_WHISPER};
use flixnet_db::{Db, DbError, media, settings};
use flixnet_media::subtitles::{SubtitleCache, SubtitleInput, is_text_subtitle};
use flixnet_media::transcode::TranscodeManager;
use flixnet_media::transcript::{ChunkOptions, chunk_cues, parse_cues};
use flixnet_media::whisper::{self, Transcription, WhisperLanguage, WhisperModel};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

/// Settings key of [`SceneSettings`].
pub const SETTINGS_KEY: &str = "scene_search";

/// Whisper settings (admin, JSON setting `scene_search`). Which libraries are
/// transcribed is stored on the library (`transcribe_audio`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SceneSettings {
    /// Path of the whisper.cpp CLI (`whisper-cli`); `None` = Whisper is off.
    pub whisper_path: Option<String>,
    /// Model name (see `flixnet_media::whisper::MODELS`), downloaded on first use.
    pub model: String,
    /// `auto` or an ISO 639-1 code.
    pub language: String,
    pub threads: u16,
}

impl Default for SceneSettings {
    fn default() -> Self {
        Self {
            whisper_path: None,
            model: whisper::DEFAULT_MODEL.to_owned(),
            language: "auto".to_owned(),
            threads: 4,
        }
    }
}

pub async fn load_settings(db: &Db) -> Result<SceneSettings, DbError> {
    Ok(settings::get(db, SETTINGS_KEY).await?.unwrap_or_default())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePayload {
    pub file_id: i64,
}

/// How often the jobs look whether the transcodes are over.
const IDLE_POLL: Duration = Duration::from_secs(10);

/// Whether a transcode is running right now (direct play and remux do not count:
/// they barely use the CPU).
async fn transcoding(transcoder: &TranscodeManager) -> bool {
    transcoder.sessions().await.iter().any(|s| s.transcoding)
}

/// Waits until no transcode runs. `false` if the job was cancelled meanwhile.
async fn wait_until_idle(transcoder: &TranscodeManager, cancel: &CancellationToken) -> bool {
    loop {
        if cancel.is_cancelled() {
            return false;
        }
        if !transcoding(transcoder).await {
            return true;
        }
        tokio::select! {
            () = tokio::time::sleep(IDLE_POLL) => {}
            () = cancel.cancelled() => return false,
        }
    }
}

/// Queues the subtitle index of a freshly probed file.
pub async fn queue_after_probe(queue: &JobQueue, file_id: i64) -> Result<(), DbError> {
    queue
        .enqueue::<IndexSceneText>(
            &FilePayload { file_id },
            JobOptions::priority(priority::BACKFILL).dedupe(format!("scene-index:{file_id}")),
        )
        .await?;
    Ok(())
}

// ---- Subtitle chunks ----------------------------------------------------------------

pub struct IndexSceneText {
    pub subtitles: SubtitleCache,
    pub transcoder: TranscodeManager,
}

impl JobHandler for IndexSceneText {
    const KIND: &'static str = "ai.scene_index";
    const POOL: Pool = Pool::Ai;
    type Payload = FilePayload;

    async fn run(&self, ctx: JobContext, payload: FilePayload) -> anyhow::Result<()> {
        if !wait_until_idle(&self.transcoder, &ctx.cancel_token()).await {
            anyhow::bail!("cancelled");
        }
        index_file(ctx.db(), &self.subtitles, payload.file_id).await?;
        Ok(())
    }
}

fn mtime_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// The text subtitle streams worth indexing: one per language, not forced (forced
/// tracks only cover foreign-language lines), regular before SDH.
fn pick_streams(streams: &[media::Stream]) -> Vec<&media::Stream> {
    let mut candidates: Vec<&media::Stream> = streams
        .iter()
        .filter(|s| s.kind == "subtitle" && !s.is_forced)
        .filter(|s| is_text_subtitle(s.codec.as_deref().unwrap_or_default(), s.is_external))
        .collect();
    candidates.sort_by_key(|s| (s.is_sdh, s.idx));
    let mut languages = HashSet::new();
    let mut out: Vec<&media::Stream> = candidates
        .into_iter()
        .filter(|s| languages.insert(s.language.clone().unwrap_or_default().to_lowercase()))
        .collect();
    out.sort_by_key(|s| s.idx);
    out
}

/// What the index run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexOutcome {
    /// Streams (re)indexed.
    pub indexed: usize,
    /// Streams already up to date.
    pub unchanged: usize,
    pub chunks: usize,
}

/// Indexes the text subtitles of a file (embedded and sidecar). Sources that did not
/// change since the last run are skipped; transcripts of streams that are gone are
/// removed. Whisper transcripts are kept.
pub async fn index_file(
    db: &Db,
    subtitles: &SubtitleCache,
    file_id: i64,
) -> anyhow::Result<IndexOutcome> {
    let mut outcome = IndexOutcome::default();
    let Some(file) = media::get_file(db, file_id).await? else {
        return Ok(outcome);
    };
    if file.missing_since.is_some() || file.role != "main" {
        return Ok(outcome);
    }
    let streams = media::streams(db, file_id).await?;
    let existing = transcripts::for_file(db, file_id).await?;
    let mut keep: Vec<(String, i64)> = vec![(SOURCE_SUBTITLE.to_owned(), RUN_MARKER)];
    for stream in pick_streams(&streams) {
        let (input, source_path) = match (stream.is_external, stream.external_path.as_deref()) {
            (true, Some(path)) => (
                SubtitleInput::External {
                    path: path.into(),
                    track: stream.idx,
                },
                PathBuf::from(path),
            ),
            (true, None) => continue,
            (false, _) => (
                SubtitleInput::Embedded {
                    file: file.path.clone().into(),
                    stream_index: stream.idx,
                },
                PathBuf::from(&file.path),
            ),
        };
        let Ok(meta) = tokio::fs::metadata(&source_path).await else {
            continue;
        };
        let signature = format!("{}-{}", meta.len(), mtime_secs(&meta));
        keep.push((SOURCE_SUBTITLE.to_owned(), stream.idx));
        if existing.iter().any(|t| {
            t.source == SOURCE_SUBTITLE && t.stream_index == stream.idx && t.signature == signature
        }) {
            outcome.unchanged += 1;
            continue;
        }
        let vtt = match subtitles.webvtt(file_id, input).await {
            Ok(path) => path,
            Err(err) => {
                tracing::warn!(file = file_id, track = stream.idx, %err, "scene index: subtitle unavailable");
                continue;
            }
        };
        let text = tokio::fs::read_to_string(&vtt).await?;
        let chunks: Vec<NewChunk> = chunk_cues(&parse_cues(&text), ChunkOptions::default())
            .into_iter()
            .map(|c| NewChunk {
                start_ms: c.start_ms,
                end_ms: c.end_ms,
                text: c.text,
            })
            .collect();
        outcome.indexed += 1;
        outcome.chunks += chunks.len();
        transcripts::store(
            db,
            file_id,
            SOURCE_SUBTITLE,
            stream.idx,
            stream.language.as_deref(),
            &signature,
            &chunks,
        )
        .await?;
    }
    // Whisper transcripts stay unless real subtitles cover the file now.
    if outcome.indexed + outcome.unchanged == 0
        && let Some(w) = existing.iter().find(|t| t.source == SOURCE_WHISPER)
    {
        keep.push((SOURCE_WHISPER.to_owned(), w.stream_index));
    }
    transcripts::remove_except(db, file_id, &keep).await?;
    transcripts::store(db, file_id, SOURCE_SUBTITLE, RUN_MARKER, None, "run", &[]).await?;
    Ok(outcome)
}

// ---- Whisper ------------------------------------------------------------------------

pub struct TranscribeAudio {
    pub ffmpeg: PathBuf,
    /// `<config_dir>/models/whisper`.
    pub models_dir: PathBuf,
    /// Scratch space for the extracted audio.
    pub work_dir: PathBuf,
    pub transcoder: TranscodeManager,
    pub http: reqwest::Client,
}

impl JobHandler for TranscribeAudio {
    const KIND: &'static str = "ai.transcribe";
    const POOL: Pool = Pool::Ai;
    const MAX_ATTEMPTS: i64 = 2;
    type Payload = FilePayload;

    async fn run(&self, ctx: JobContext, payload: FilePayload) -> anyhow::Result<()> {
        let db = ctx.db();
        let settings = load_settings(db).await?;
        let Some(whisper_path) = settings
            .whisper_path
            .clone()
            .filter(|p| !p.trim().is_empty())
        else {
            return Ok(()); // switched off meanwhile
        };
        let Some(file) = media::get_file(db, payload.file_id).await? else {
            return Ok(());
        };
        if file.missing_since.is_some()
            || file.role != "main"
            || !transcripts::library_transcribes(db, file.library_id).await?
        {
            return Ok(());
        }
        let streams = media::streams(db, file.id).await?;
        if !pick_streams(&streams).is_empty() || !streams.iter().any(|s| s.kind == "audio") {
            return Ok(()); // text subtitles cover it (or there is nothing to hear)
        }
        let model = whisper::model(&settings.model)
            .ok_or_else(|| anyhow::anyhow!("unknown Whisper model {}", settings.model))?;
        let model_path =
            ensure_model(&self.http, whisper::MODEL_BASE_URL, &self.models_dir, model).await?;
        let language = WhisperLanguage::parse(&settings.language).unwrap_or(WhisperLanguage::Auto);
        let work_dir = self.work_dir.join(format!("file-{}", file.id));
        let cancel = ctx.cancel_token();

        // Restart from scratch whenever a transcode interrupts the run.
        let srt = loop {
            if !wait_until_idle(&self.transcoder, &cancel).await {
                anyhow::bail!("cancelled");
            }
            let run = cancel.child_token();
            let watcher = {
                let (transcoder, run) = (self.transcoder.clone(), run.clone());
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            () = tokio::time::sleep(IDLE_POLL) => {}
                            () = run.cancelled() => return,
                        }
                        if transcoding(&transcoder).await {
                            run.cancel();
                            return;
                        }
                    }
                })
            };
            ctx.progress(0.0).await;
            let result = whisper::transcribe(
                &Transcription {
                    ffmpeg: &self.ffmpeg,
                    whisper: Path::new(&whisper_path),
                    model: &model_path,
                    input: Path::new(&file.path),
                    language,
                    threads: settings.threads,
                    work_dir: &work_dir,
                },
                &run,
            )
            .await;
            run.cancel();
            let _ = watcher.await;
            match result {
                Ok(srt) => break srt,
                Err(whisper::WhisperError::Cancelled) if !cancel.is_cancelled() => {
                    tracing::info!(file = file.id, "Whisper paused for a transcode");
                }
                Err(err) => {
                    let _ = tokio::fs::remove_dir_all(&work_dir).await;
                    return Err(err.into());
                }
            }
        };
        let _ = tokio::fs::remove_dir_all(&work_dir).await;
        let chunks: Vec<NewChunk> = chunk_cues(&parse_cues(&srt), ChunkOptions::default())
            .into_iter()
            .map(|c| NewChunk {
                start_ms: c.start_ms,
                end_ms: c.end_ms,
                text: c.text,
            })
            .collect();
        let language_code = match language {
            WhisperLanguage::Auto => None,
            WhisperLanguage::Code(_) => Some(settings.language.as_str()),
        };
        transcripts::store(
            db,
            file.id,
            SOURCE_WHISPER,
            RUN_MARKER,
            language_code,
            &format!("{}-{}", file.size, file.mtime),
            &chunks,
        )
        .await?;
        Ok(())
    }
}

/// Path of a Whisper model in `models_dir`, downloading it from `base_url` on first
/// use. The download is checked against the published SHA-256 before it is used;
/// a mismatch deletes it.
pub async fn ensure_model(
    http: &reqwest::Client,
    base_url: &str,
    models_dir: &Path,
    model: &WhisperModel,
) -> anyhow::Result<PathBuf> {
    let path = models_dir.join(model.file_name);
    if let Ok(meta) = tokio::fs::metadata(&path).await
        && meta.len() == model.size
    {
        return Ok(path); // verified when it was downloaded
    }
    tokio::fs::create_dir_all(models_dir).await?;
    let partial = models_dir.join(format!("{}.part", model.file_name));
    let url = format!("{}/{}", base_url.trim_end_matches('/'), model.file_name);
    tracing::info!(%url, "downloading Whisper model");
    let result = async {
        let mut response = http
            .get(&url)
            .timeout(Duration::from_secs(3600))
            .send()
            .await?
            .error_for_status()?;
        let mut out = tokio::fs::File::create(&partial).await?;
        let mut hasher = Sha256::new();
        let mut written: u64 = 0;
        while let Some(bytes) = response.chunk().await? {
            hasher.update(&bytes);
            written += bytes.len() as u64;
            tokio::io::AsyncWriteExt::write_all(&mut out, &bytes).await?;
        }
        tokio::io::AsyncWriteExt::flush(&mut out).await?;
        drop(out);
        let digest = hex::encode(hasher.finalize());
        if digest != model.sha256 || written != model.size {
            anyhow::bail!(
                "Whisper model {} failed verification (sha256 {digest}, {written} bytes)",
                model.name
            );
        }
        tokio::fs::rename(&partial, &path).await?;
        Ok(path.clone())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&partial).await;
    }
    result
}

// ---- Backfill (scheduled) -----------------------------------------------------------

/// Queues files whose subtitles are not indexed yet and, with Whisper configured,
/// files of opted-in libraries that have no text at all.
pub struct SceneIndexBackfill;

const BACKFILL_BATCH: i64 = 500;
const TRANSCRIBE_BATCH: i64 = 20;

impl JobHandler for SceneIndexBackfill {
    const KIND: &'static str = "ai.scene_backfill";
    const POOL: Pool = Pool::Io;
    type Payload = ();

    async fn run(&self, ctx: JobContext, _: ()) -> anyhow::Result<()> {
        let db = ctx.db();
        for file_id in transcripts::files_to_index(db, BACKFILL_BATCH).await? {
            queue_after_probe(ctx.queue(), file_id).await?;
        }
        let settings = load_settings(db).await?;
        if settings
            .whisper_path
            .as_deref()
            .is_some_and(|p| !p.trim().is_empty())
        {
            for file_id in transcripts::files_to_transcribe(db, TRANSCRIBE_BATCH).await? {
                ctx.queue()
                    .enqueue::<TranscribeAudio>(
                        &FilePayload { file_id },
                        JobOptions::priority(priority::BACKFILL)
                            .dedupe(format!("transcribe:{file_id}")),
                    )
                    .await?;
            }
        }
        Ok(())
    }
}

/// Name of the scheduled task (also run from the settings page).
pub const TASK_NAME: &str = "scene_index";

pub fn tasks() -> Vec<TaskDef> {
    vec![TaskDef::new::<SceneIndexBackfill>(
        TASK_NAME,
        "Szenensuche: Untertitel indexieren und (falls aktiviert) Whisper-Transkripte erstellen",
        Duration::from_secs(6 * 60 * 60),
        &(),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(idx: i64, codec: &str, language: &str, forced: bool, sdh: bool) -> media::Stream {
        media::Stream {
            idx,
            kind: "subtitle".into(),
            codec: Some(codec.into()),
            language: Some(language.into()),
            is_forced: forced,
            is_sdh: sdh,
            is_external: idx >= 1000,
            external_path: (idx >= 1000).then(|| format!("/m/film.{idx}.srt")),
            ..Default::default()
        }
    }

    #[test]
    fn one_text_stream_per_language_is_indexed() {
        let streams = vec![
            sub(2, "hdmv_pgs_subtitle", "deu", false, false),
            sub(3, "subrip", "deu", true, false),
            sub(4, "subrip", "deu", false, true),
            sub(5, "subrip", "deu", false, false),
            sub(6, "ass", "eng", false, false),
            sub(1000, "srt", "eng", false, false),
        ];
        let picked: Vec<i64> = pick_streams(&streams).iter().map(|s| s.idx).collect();
        assert_eq!(picked, vec![5, 6]);
    }

    #[tokio::test]
    async fn models_are_downloaded_once_and_verified() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = b"fake whisper model".to_vec();
        Mock::given(method("GET"))
            .and(path("/ggml-test.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let http = crate::http::client();
        let sha = hex::encode(Sha256::digest(&body));
        let sha: &'static str = Box::leak(sha.into_boxed_str());
        let good = WhisperModel {
            name: "test",
            file_name: "ggml-test.bin",
            sha256: sha,
            size: body.len() as u64,
        };
        let stored = ensure_model(&http, &server.uri(), dir.path(), &good)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&stored).unwrap(), body);
        // Present: no second download (the mock expects exactly one request).
        ensure_model(&http, &server.uri(), dir.path(), &good)
            .await
            .unwrap();

        let bad = WhisperModel {
            file_name: "ggml-bad.bin",
            ..good
        };
        Mock::given(method("GET"))
            .and(path("/ggml-bad.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"tampered!!!!!!!!!!".to_vec()))
            .mount(&server)
            .await;
        let err = ensure_model(&http, &server.uri(), dir.path(), &bad)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("verification"), "{err}");
        assert!(!dir.path().join("ggml-bad.bin").exists());
        assert!(!dir.path().join("ggml-bad.bin.part").exists());
    }
}
