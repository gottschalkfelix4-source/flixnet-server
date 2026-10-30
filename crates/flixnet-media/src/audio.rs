//! Music playback: the audio-only path of the decision engine and AAC conversion for
//! formats a client cannot play (WMA, ALAC in browsers without it, APE, …).
//!
//! The decision mirrors [`crate::decision::decide`] — same device profile, same
//! reasons — but music has no video, no track switching and no subtitles, so there are
//! only two outcomes: the original file (Direct Play), or a converted copy.
//!
//! Conversion writes a complete `.m4a` (AAC-LC, `+faststart`) into a cache folder and
//! serves that file with range requests: a track converts in a second or two, the
//! player gets an exact duration and real seeking, and preloading the next track for
//! gapless playback works the same as with original files. Every ffmpeg argument is
//! built from typed values and paths the server chose.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::decision::{DeviceProfile, Reason, SourceMedia};
use crate::process::command;

/// Output codec of conversions: every browser and native player decodes AAC.
pub const TARGET_CODEC: &str = "aac";
/// AAC bitrate of conversions (transparent for stereo music).
pub const TARGET_BITRATE: i64 = 256_000;
/// Conversions of one track give up after this long.
const CONVERT_TIMEOUT: Duration = Duration::from_secs(300);
/// Converted files kept on disk; the oldest go first.
pub const CACHE_LIMIT_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AudioMethod {
    /// The original file, as-is.
    DirectPlay,
    /// Converted to AAC (`.m4a`).
    Transcode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AudioDecision {
    pub method: AudioMethod,
    /// Empty for Direct Play.
    pub reasons: Vec<Reason>,
    /// Stream index of the audio stream that is played.
    pub stream_index: Option<i64>,
    /// Channels of the output (conversions mix down to stereo).
    pub channels: i64,
}

/// Whether a client can play an audio file as-is.
pub fn decide_audio(
    source: &SourceMedia,
    profile: &DeviceProfile,
    max_bitrate: Option<i64>,
) -> AudioDecision {
    let mut reasons = Vec::new();
    let track = source
        .audio
        .iter()
        .find(|a| a.is_default)
        .or_else(|| source.audio.first());
    if !profile.containers.contains(&source.container) {
        reasons.push(Reason::ContainerNotSupported);
    }
    match track {
        None => reasons.push(Reason::AudioCodecNotSupported),
        Some(track) => match profile.audio_codecs.iter().find(|c| c.codec == track.codec) {
            None => reasons.push(Reason::AudioCodecNotSupported),
            Some(support) if track.channels > support.max_channels.unwrap_or(2) => {
                reasons.push(Reason::AudioChannelsNotSupported);
            }
            Some(_) => {}
        },
    }
    let limit = match (profile.max_bitrate, max_bitrate) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    if matches!((source.bitrate, limit), (Some(b), Some(max)) if b > max) {
        reasons.push(Reason::BitrateTooHigh);
    }
    let channels = track.map_or(2, |t| t.channels);
    if reasons.is_empty() {
        AudioDecision {
            method: AudioMethod::DirectPlay,
            reasons,
            stream_index: track.map(|t| t.index),
            channels,
        }
    } else {
        AudioDecision {
            method: AudioMethod::Transcode,
            reasons,
            stream_index: track.map(|t| t.index),
            channels: channels.min(2),
        }
    }
}

/// What to convert.
#[derive(Debug, Clone)]
pub struct AacJob<'a> {
    pub input: &'a Path,
    /// Audio stream within the input (`None`: the first one).
    pub stream_index: Option<i64>,
    pub channels: i64,
    pub sample_rate: Option<i64>,
}

/// ffmpeg arguments of a conversion to AAC in MP4 (moov atom first for streaming).
pub fn aac_args(job: &AacJob<'_>, output: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"]
        .into_iter()
        .map(OsString::from)
        .collect();
    args.push(job.input.as_os_str().to_owned());
    let map = match job.stream_index {
        Some(index) => format!("0:{index}"),
        None => "0:a:0".to_owned(),
    };
    let bitrate = TARGET_BITRATE.to_string();
    args.extend(
        [
            "-map",
            &map,
            "-vn",
            "-sn",
            "-dn",
            "-map_metadata",
            "-1",
            "-c:a",
            TARGET_CODEC,
            "-b:a",
            &bitrate,
        ]
        .map(OsString::from),
    );
    if job.channels > 2 {
        args.extend(["-ac", "2"].map(OsString::from));
    }
    // AAC in browsers: at most 48 kHz.
    if job.sample_rate.is_none_or(|rate| rate > 48_000) {
        args.extend(["-ar", "48000"].map(OsString::from));
    }
    args.extend(["-movflags", "+faststart", "-f", "mp4"].map(OsString::from));
    args.push(output.as_os_str().to_owned());
    args
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("ffmpeg could not be started: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("conversion failed: {0}")]
    Failed(String),
    #[error("conversion timed out")]
    Timeout,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Conversions in progress, so concurrent requests for one track wait for one ffmpeg.
static IN_FLIGHT: LazyLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Default::default);

/// Converted copies of tracks in `<dir>/<key>.m4a`.
#[derive(Debug, Clone)]
pub struct AudioTranscoder {
    ffmpeg: PathBuf,
    dir: PathBuf,
}

impl AudioTranscoder {
    pub fn new(ffmpeg: impl Into<PathBuf>, dir: impl Into<PathBuf>) -> Self {
        Self {
            ffmpeg: ffmpeg.into(),
            dir: dir.into(),
        }
    }

    /// The AAC copy of a file, converting it first if needed. `key` identifies the
    /// source version (file id, size and modification time), so a changed file is
    /// converted again.
    pub async fn aac(&self, key: &str, job: &AacJob<'_>) -> Result<PathBuf, AudioError> {
        let safe: String = key
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        let target = self.dir.join(format!("{safe}.m4a"));
        let lock = IN_FLIGHT
            .lock()
            .expect("conversion map")
            .entry(target.clone())
            .or_default()
            .clone();
        let _guard = lock.lock().await;
        if tokio::fs::try_exists(&target).await.unwrap_or(false) {
            return Ok(target);
        }
        tokio::fs::create_dir_all(&self.dir).await?;
        let partial = self.dir.join(format!("{safe}.part.m4a"));
        let args = aac_args(job, &partial);
        let run = command(&self.ffmpeg)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output();
        let output = match tokio::time::timeout(CONVERT_TIMEOUT, run).await {
            Ok(result) => result.map_err(AudioError::Spawn)?,
            Err(_) => {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(AudioError::Timeout);
            }
        };
        if !output.status.success() {
            let _ = tokio::fs::remove_file(&partial).await;
            let message = String::from_utf8_lossy(&output.stderr);
            return Err(AudioError::Failed(
                message.lines().last().unwrap_or("unknown error").to_owned(),
            ));
        }
        tokio::fs::rename(&partial, &target).await?;
        IN_FLIGHT.lock().expect("conversion map").remove(&target);
        let dir = self.dir.clone();
        let keep = target.clone();
        tokio::task::spawn_blocking(move || prune(&dir, CACHE_LIMIT_BYTES, &keep));
        Ok(target)
    }
}

/// Deletes the oldest converted files until the folder is below `limit` bytes.
fn prune(dir: &Path, limit: u64, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            meta.is_file()
                .then(|| Some((meta.modified().ok()?, meta.len(), e.path())))
                .flatten()
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort_by_key(|f| f.0);
    for (_, size, path) in files {
        if total <= limit {
            break;
        }
        if path != keep && std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::{AudioCodecSupport, SourceAudio};

    fn source(container: &str, codec: &str, channels: i64) -> SourceMedia {
        SourceMedia {
            container: container.to_owned(),
            bitrate: Some(900_000),
            audio: vec![SourceAudio {
                index: 0,
                codec: codec.to_owned(),
                channels,
                is_default: true,
            }],
            ..Default::default()
        }
    }

    fn browser() -> DeviceProfile {
        DeviceProfile {
            containers: ["flac", "mp3", "m4a", "ogg", "opus", "wav"]
                .map(str::to_owned)
                .to_vec(),
            audio_codecs: ["flac", "mp3", "aac", "vorbis", "opus", "pcm_s16le"]
                .map(|codec| AudioCodecSupport {
                    codec: codec.to_owned(),
                    max_channels: Some(8),
                })
                .to_vec(),
            ..Default::default()
        }
    }

    /// container, codec, channels, bitrate limit → method, reasons
    type Case<'a> = (
        &'a str,
        &'a str,
        i64,
        Option<i64>,
        AudioMethod,
        &'a [Reason],
    );

    #[test]
    fn decisions_over_formats_and_profiles() {
        let cases: &[Case] = &[
            ("flac", "flac", 2, None, AudioMethod::DirectPlay, &[]),
            ("mp3", "mp3", 2, None, AudioMethod::DirectPlay, &[]),
            ("m4a", "aac", 2, None, AudioMethod::DirectPlay, &[]),
            ("opus", "opus", 2, None, AudioMethod::DirectPlay, &[]),
            // Chromium has no ALAC; nobody plays WMA.
            (
                "m4a",
                "alac",
                2,
                None,
                AudioMethod::Transcode,
                &[Reason::AudioCodecNotSupported],
            ),
            (
                "wma",
                "wmav2",
                2,
                None,
                AudioMethod::Transcode,
                &[
                    Reason::ContainerNotSupported,
                    Reason::AudioCodecNotSupported,
                ],
            ),
            (
                "flac",
                "flac",
                2,
                Some(320_000),
                AudioMethod::Transcode,
                &[Reason::BitrateTooHigh],
            ),
        ];
        for (container, codec, channels, limit, method, reasons) in cases {
            let decision = decide_audio(&source(container, codec, *channels), &browser(), *limit);
            assert_eq!(decision.method, *method, "{container}/{codec}");
            assert_eq!(decision.reasons, *reasons, "{container}/{codec}");
        }
        // A client that declares nothing gets AAC.
        let empty = decide_audio(&source("flac", "flac", 6), &DeviceProfile::default(), None);
        assert_eq!(empty.method, AudioMethod::Transcode);
        assert_eq!(empty.channels, 2);
    }

    #[test]
    fn conversion_arguments_are_typed() {
        let args = aac_args(
            &AacJob {
                input: Path::new("/music/a b.wma"),
                stream_index: Some(1),
                channels: 6,
                sample_rate: Some(96_000),
            },
            Path::new("/cache/x.m4a"),
        );
        let args: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args.join(" "),
            "-hide_banner -loglevel error -nostdin -y -i /music/a b.wma -map 0:1 -vn -sn -dn \
             -map_metadata -1 -c:a aac -b:a 256000 -ac 2 -ar 48000 -movflags +faststart -f mp4 \
             /cache/x.m4a"
        );
    }
}
