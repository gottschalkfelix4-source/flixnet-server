//! Text subtitles as WebVTT, the one format every browser renders in `<track>`.
//!
//! Embedded text streams (SubRip, ASS/SSA, mov_text, WebVTT) and sidecar files are
//! converted once by ffmpeg and cached on disk. Extraction runs in the background:
//! a long film has to be read completely to collect one subtitle stream, so a
//! cancelled request must not throw that work away, and requests for the same track
//! share one ffmpeg run. A semaphore bounds how many run at once and a timeout ends
//! runs that hang.
//!
//! Picture subtitles (PGS, VobSub, DVB) cannot become text; they are burned into the
//! video by the HLS transcode instead (see `hls`).

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use tokio::sync::{Semaphore, watch};

use crate::process::command;

/// Embedded codecs (ffprobe names) ffmpeg converts to WebVTT.
const TEXT_CODECS: &[&str] = &["subrip", "srt", "ass", "ssa", "mov_text", "webvtt", "text"];
/// Sidecar file extensions with text content. `.sub` is left out: it is either
/// MicroDVD text or the picture half of VobSub.
const TEXT_EXTENSIONS: &[&str] = &["srt", "ass", "ssa", "vtt", "smi"];

/// Whether a subtitle stream can be delivered as WebVTT. `codec` is the ffprobe codec
/// name for embedded streams and the file extension for sidecar files.
pub fn is_text_subtitle(codec: &str, is_external: bool) -> bool {
    let codec = codec.to_ascii_lowercase();
    if is_external {
        TEXT_EXTENSIONS.contains(&codec.as_str())
    } else {
        TEXT_CODECS.contains(&codec.as_str())
    }
}

/// Where a subtitle track comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubtitleInput {
    /// A stream inside the media file (`stream_index` as ffprobe numbers it).
    Embedded { file: PathBuf, stream_index: i64 },
    /// A sidecar file next to the media file; `track` is its stream number in the
    /// database (from 1000 on) and only names the cache entry.
    External { path: PathBuf, track: i64 },
}

impl SubtitleInput {
    fn source(&self) -> &Path {
        match self {
            Self::Embedded { file, .. } => file,
            Self::External { path, .. } => path,
        }
    }

    fn track(&self) -> i64 {
        match self {
            Self::Embedded { stream_index, .. } => *stream_index,
            Self::External { track, .. } => *track,
        }
    }
}

/// ffmpeg arguments converting `input` (read from `source`, which differs from the
/// input's own path only for re-encoded sidecar files) to a WebVTT file.
pub fn vtt_args(input: &SubtitleInput, source: &Path, output: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]
        .into_iter()
        .map(OsString::from)
        .collect();
    // The file's own timestamps: ffmpeg up to 7 otherwise shifts subtitles by a
    // negative start time (audio priming), ffmpeg 8 doesn't.
    args.push("-copyts".into());
    args.push("-i".into());
    args.push(source.as_os_str().to_owned());
    let map = match input {
        SubtitleInput::Embedded { stream_index, .. } => format!("0:{stream_index}"),
        SubtitleInput::External { .. } => "0:s:0".to_owned(),
    };
    for arg in [
        "-map", &map, "-vn", "-an", "-dn", "-c:s", "webvtt", "-f", "webvtt",
    ] {
        args.push(arg.into());
    }
    args.push(output.as_os_str().to_owned());
    args
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum SubtitleError {
    #[error("subtitle source unavailable: {0}")]
    Source(String),
    #[error("could not start ffmpeg: {0}")]
    Spawn(String),
    #[error("ffmpeg failed: {0}")]
    Failed(String),
    #[error("subtitle extraction timed out after {0:?}")]
    Timeout(Duration),
    #[error("subtitle cache: {0}")]
    Io(String),
}

type Outcome = Option<Result<PathBuf, SubtitleError>>;

/// WebVTT files on disk, one per track and source version.
#[derive(Clone)]
pub struct SubtitleCache {
    inner: Arc<Inner>,
}

struct Inner {
    ffmpeg: PathBuf,
    dir: PathBuf,
    timeout: Duration,
    permits: Semaphore,
    /// Extractions in progress, by cache key.
    running: Mutex<HashMap<String, watch::Receiver<Outcome>>>,
}

impl SubtitleCache {
    /// `timeout` bounds one ffmpeg run; at most `parallel` run at the same time.
    pub fn new(
        ffmpeg: impl Into<PathBuf>,
        dir: impl Into<PathBuf>,
        timeout: Duration,
        parallel: usize,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                ffmpeg: ffmpeg.into(),
                dir: dir.into(),
                timeout,
                permits: Semaphore::new(parallel.max(1)),
                running: Mutex::default(),
            }),
        }
    }

    /// Path of the track as WebVTT, converting it first if it is not cached yet.
    pub async fn webvtt(
        &self,
        file_id: i64,
        input: SubtitleInput,
    ) -> Result<PathBuf, SubtitleError> {
        let meta = tokio::fs::metadata(input.source())
            .await
            .map_err(|err| SubtitleError::Source(err.to_string()))?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        // A changed source gets a new key, so stale conversions are never served.
        let prefix = format!("{file_id}-{}-", input.track());
        let key = format!("{prefix}{}-{mtime}", meta.len());
        let output = self.inner.dir.join(format!("{key}.vtt"));

        let mut outcome = {
            let mut running = self.inner.running.lock().unwrap();
            if let Some(rx) = running.get(&key) {
                rx.clone()
            } else {
                if output.exists() {
                    return Ok(output);
                }
                let (tx, rx) = watch::channel(None);
                running.insert(key.clone(), rx.clone());
                let inner = Arc::clone(&self.inner);
                tokio::spawn(async move {
                    let result = inner.extract(&input, &prefix, &key, &output).await;
                    if let Err(err) = &result {
                        tracing::warn!(file = file_id, track = input.track(), %err, "subtitle conversion failed");
                    }
                    inner.running.lock().unwrap().remove(&key);
                    let _ = tx.send(Some(result));
                });
                rx
            }
        };
        let done = outcome
            .wait_for(Option::is_some)
            .await
            .map_err(|_| SubtitleError::Failed("conversion aborted".to_owned()))?;
        done.clone().expect("waited for a result")
    }
}

impl Inner {
    async fn extract(
        &self,
        input: &SubtitleInput,
        prefix: &str,
        key: &str,
        output: &Path,
    ) -> Result<PathBuf, SubtitleError> {
        let io = |err: std::io::Error| SubtitleError::Io(err.to_string());
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| SubtitleError::Failed("shutting down".to_owned()))?;
        tokio::fs::create_dir_all(&self.dir).await.map_err(io)?;

        // Sidecar files are often in a legacy 8-bit encoding; ffmpeg expects UTF-8.
        let mut converted = None;
        if let SubtitleInput::External { path, .. } = input {
            let bytes = tokio::fs::read(path)
                .await
                .map_err(|err| SubtitleError::Source(err.to_string()))?;
            if let Some(text) = legacy_to_utf8(&bytes) {
                let ext = path.extension().unwrap_or_default().to_string_lossy();
                let copy = self.dir.join(format!("{key}.source.{ext}"));
                tokio::fs::write(&copy, text).await.map_err(io)?;
                converted = Some(copy);
            }
        }
        let source = converted.as_deref().unwrap_or(input.source());
        let partial = self.dir.join(format!("{key}.part"));
        let args = vtt_args(input, source, &partial);
        let child = command(&self.ffmpeg)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| SubtitleError::Spawn(err.to_string()))?;
        // Dropping the child on timeout kills ffmpeg.
        let result = tokio::time::timeout(self.timeout, child.wait_with_output()).await;
        if let Some(copy) = &converted {
            let _ = tokio::fs::remove_file(copy).await;
        }
        let finished = match result {
            Err(_) => Err(SubtitleError::Timeout(self.timeout)),
            Ok(Err(err)) => Err(SubtitleError::Spawn(err.to_string())),
            Ok(Ok(out)) if !out.status.success() => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let message = stderr.lines().last().unwrap_or("unknown error").trim();
                Err(SubtitleError::Failed(message.to_owned()))
            }
            Ok(Ok(_)) => Ok(()),
        };
        if let Err(err) = finished {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(err);
        }
        tokio::fs::rename(&partial, output).await.map_err(io)?;
        self.remove_stale(prefix, key).await;
        Ok(output.to_owned())
    }

    /// Conversions of earlier versions of the same track.
    async fn remove_stale(&self, prefix: &str, key: &str) {
        let Ok(mut entries) = tokio::fs::read_dir(&self.dir).await else {
            return;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(prefix) && name.ends_with(".vtt") && name != format!("{key}.vtt") {
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
    }
}

/// Text that is neither UTF-8 nor UTF-16 (which ffmpeg detects by its BOM) is taken
/// as Windows-1252, the usual encoding of older European subtitle files. Returns
/// `None` when the bytes can be used as they are.
fn legacy_to_utf8(bytes: &[u8]) -> Option<String> {
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return None;
    }
    if std::str::from_utf8(bytes).is_ok() {
        return None;
    }
    Some(bytes.iter().map(|b| windows_1252(*b)).collect())
}

fn windows_1252(byte: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž',
        '\u{8F}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}',
        'ž', 'Ÿ',
    ];
    match byte {
        0x80..=0x9F => HIGH[usize::from(byte - 0x80)],
        _ => char::from(byte),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(args: &[OsString]) -> String {
        args.iter()
            .map(|a| a.to_string_lossy().replace('\\', "/"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn embedded_streams_are_mapped_by_index() {
        let input = SubtitleInput::Embedded {
            file: PathBuf::from("/media/Film (2020)/film.mkv"),
            stream_index: 4,
        };
        let args = vtt_args(&input, input.source(), Path::new("/cache/1-4.part"));
        assert_eq!(
            joined(&args),
            "-hide_banner -loglevel error -nostdin -y -copyts -i /media/Film (2020)/film.mkv \
             -map 0:4 -vn -an -dn -c:s webvtt -f webvtt /cache/1-4.part"
        );
        // The path stays one argument, spaces and all.
        assert!(args.iter().any(|a| a == "/media/Film (2020)/film.mkv"));
    }

    #[test]
    fn sidecar_files_use_their_only_stream() {
        let input = SubtitleInput::External {
            path: PathBuf::from("/media/film.de.srt"),
            track: 1000,
        };
        let line = joined(&vtt_args(
            &input,
            Path::new("/cache/x.source.srt"),
            Path::new("out.part"),
        ));
        assert!(line.contains("-i /cache/x.source.srt -map 0:s:0 -vn"));
        assert!(line.ends_with("-c:s webvtt -f webvtt out.part"));
    }

    #[test]
    fn text_and_picture_codecs() {
        for codec in ["subrip", "ass", "ssa", "mov_text", "webvtt"] {
            assert!(is_text_subtitle(codec, false), "{codec}");
        }
        for codec in ["hdmv_pgs_subtitle", "dvd_subtitle", "dvb_subtitle"] {
            assert!(!is_text_subtitle(codec, false), "{codec}");
        }
        assert!(is_text_subtitle("SRT", true));
        assert!(is_text_subtitle("ass", true));
        assert!(!is_text_subtitle("sup", true));
        assert!(!is_text_subtitle("sub", true), "may be VobSub");
        assert!(!is_text_subtitle("idx", true));
    }

    #[test]
    fn legacy_encodings_become_utf8() {
        assert_eq!(legacy_to_utf8("Grüße".as_bytes()), None);
        assert_eq!(legacy_to_utf8(&[0xFF, 0xFE, b'a', 0]), None, "UTF-16");
        assert_eq!(
            legacy_to_utf8(b"Gr\xfc\xdfe \x84zitiert\x93 \x80").as_deref(),
            Some("Grüße „zitiert“ €")
        );
    }

    #[tokio::test]
    async fn missing_sources_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let cache = SubtitleCache::new("ffmpeg", dir.path(), Duration::from_secs(5), 1);
        let err = cache
            .webvtt(
                1,
                SubtitleInput::External {
                    path: dir.path().join("missing.srt"),
                    track: 1000,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SubtitleError::Source(_)), "{err}");
    }
}
