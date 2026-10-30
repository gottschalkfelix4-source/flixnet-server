//! ffprobe: container, streams and chapters of a media file.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::process::command;

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("could not start ffprobe ({path}): {source}")]
    Spawn {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("ffprobe timed out after {0:?}")]
    Timeout(Duration),
    #[error("ffprobe failed: {0}")]
    Failed(String),
    #[error("unexpected ffprobe output: {0}")]
    Output(#[from] serde_json::Error),
}

/// Runs ffprobe. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Ffprobe {
    pub(crate) path: PathBuf,
    pub(crate) timeout: Duration,
}

impl Ffprobe {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            timeout: Duration::from_secs(60),
        }
    }

    pub async fn probe(&self, file: &Path) -> Result<MediaInfo, ProbeError> {
        let mut cmd = command(&self.path);
        cmd.args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            "-show_chapters",
            "-i",
        ])
        .arg(file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

        let child = cmd.spawn().map_err(|source| ProbeError::Spawn {
            path: self.path.clone(),
            source,
        })?;
        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| ProbeError::Timeout(self.timeout))?
            .map_err(|source| ProbeError::Spawn {
                path: self.path.clone(),
                source,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = stderr.lines().last().unwrap_or("unknown error").trim();
            return Err(ProbeError::Failed(message.to_owned()));
        }
        parse(&output.stdout, file)
    }

    /// Keyframe times (ms, ascending) of one video stream. Reads packets only — no
    /// decoding — so even long films take seconds, not minutes.
    pub async fn keyframes(&self, file: &Path, stream_index: i64) -> Result<Vec<i64>, ProbeError> {
        let timeout = Duration::from_secs(600);
        let mut cmd = command(&self.path);
        cmd.args(["-v", "error", "-select_streams"])
            .arg(stream_index.to_string())
            .args([
                "-show_entries",
                "packet=pts_time,flags",
                "-of",
                "csv=p=0",
                "-i",
            ])
            .arg(file)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = cmd.spawn().map_err(|source| ProbeError::Spawn {
            path: self.path.clone(),
            source,
        })?;
        let output = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| ProbeError::Timeout(timeout))?
            .map_err(|source| ProbeError::Spawn {
                path: self.path.clone(),
                source,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = stderr.lines().last().unwrap_or("unknown error").trim();
            return Err(ProbeError::Failed(message.to_owned()));
        }
        Ok(parse_keyframes(&String::from_utf8_lossy(&output.stdout)))
    }
}

/// `pts_time,flags` lines → keyframe times in ms.
fn parse_keyframes(csv: &str) -> Vec<i64> {
    let mut times: Vec<i64> = csv
        .lines()
        .filter_map(|line| {
            let (pts, flags) = line.trim().split_once(',')?;
            if !flags.starts_with('K') {
                return None;
            }
            let secs: f64 = pts.parse().ok()?;
            Some((secs * 1000.0).round() as i64)
        })
        .collect();
    times.sort_unstable();
    times.dedup();
    times
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
    Attachment,
    Data,
}

impl StreamKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StreamKind::Video => "video",
            StreamKind::Audio => "audio",
            StreamKind::Subtitle => "subtitle",
            StreamKind::Attachment => "attachment",
            StreamKind::Data => "data",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StreamInfo {
    pub index: i64,
    pub kind: StreamKind,
    pub codec: Option<String>,
    pub profile: Option<String>,
    pub level: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub fps: Option<f64>,
    pub bit_depth: Option<i64>,
    pub pix_fmt: Option<String>,
    pub color_transfer: Option<String>,
    pub color_primaries: Option<String>,
    /// sdr | hdr10 | hlg | dovi (video only)
    pub hdr: Option<&'static str>,
    pub channels: Option<i64>,
    pub channel_layout: Option<String>,
    pub sample_rate: Option<i64>,
    pub bitrate: Option<i64>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_default: bool,
    pub is_forced: bool,
    pub is_sdh: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Chapter {
    pub start_ms: i64,
    pub end_ms: i64,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MediaInfo {
    /// Normalized container: mkv, mp4, webm, ts, avi, mov, …
    pub container: Option<String>,
    pub duration_ms: Option<i64>,
    pub bitrate: Option<i64>,
    pub streams: Vec<StreamInfo>,
    pub chapters: Vec<Chapter>,
}

// ---- Raw ffprobe JSON -------------------------------------------------------------

#[derive(Deserialize)]
struct RawOutput {
    #[serde(default)]
    streams: Vec<RawStream>,
    #[serde(default)]
    chapters: Vec<RawChapter>,
    format: Option<RawFormat>,
}

#[derive(Deserialize)]
struct RawFormat {
    format_name: Option<String>,
    duration: Option<String>,
    bit_rate: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawDisposition {
    #[serde(default)]
    default: i64,
    #[serde(default)]
    forced: i64,
    #[serde(default)]
    hearing_impaired: i64,
}

#[derive(Deserialize)]
struct RawSideData {
    side_data_type: Option<String>,
}

#[derive(Deserialize)]
struct RawStream {
    index: i64,
    codec_type: Option<String>,
    codec_name: Option<String>,
    profile: Option<String>,
    level: Option<i64>,
    width: Option<i64>,
    height: Option<i64>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    bits_per_raw_sample: Option<String>,
    pix_fmt: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    channels: Option<i64>,
    channel_layout: Option<String>,
    sample_rate: Option<String>,
    bit_rate: Option<String>,
    #[serde(default)]
    disposition: RawDisposition,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
    #[serde(default)]
    side_data_list: Vec<RawSideData>,
}

#[derive(Deserialize)]
struct RawChapter {
    start_time: Option<String>,
    end_time: Option<String>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
}

fn seconds_to_ms(value: Option<&str>) -> Option<i64> {
    value
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map(|s| (s * 1000.0).round() as i64)
}

fn fraction(value: Option<&str>) -> Option<f64> {
    let (num, den) = value?.split_once('/')?;
    let (num, den): (f64, f64) = (num.parse().ok()?, den.parse().ok()?);
    (den > 0.0 && num > 0.0).then(|| (num / den * 1000.0).round() / 1000.0)
}

fn tag(tags: &std::collections::HashMap<String, String>, key: &str) -> Option<String> {
    tags.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn container(format_name: Option<&str>, file: &Path) -> Option<String> {
    let ext = file
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    let name = format_name?;
    Some(
        if name.contains("matroska") {
            if ext.as_deref() == Some("webm") {
                "webm"
            } else {
                "mkv"
            }
        } else if name.contains("mp4") || name.contains("mov") {
            match ext.as_deref() {
                Some("mov") => "mov",
                Some("m4v") => "m4v",
                _ => "mp4",
            }
        } else if name == "mpegts" {
            "ts"
        } else if name == "avi" {
            "avi"
        } else if name.starts_with("mpeg") {
            "mpeg"
        } else {
            name.split(',').next().unwrap_or(name)
        }
        .to_owned(),
    )
}

fn hdr(stream: &RawStream) -> Option<&'static str> {
    if stream.codec_type.as_deref() != Some("video") {
        return None;
    }
    let dovi = stream.side_data_list.iter().any(|s| {
        s.side_data_type
            .as_deref()
            .is_some_and(|t| t.contains("DOVI") || t.contains("Dolby Vision"))
    });
    Some(if dovi {
        "dovi"
    } else {
        match stream.color_transfer.as_deref() {
            Some("smpte2084") => "hdr10",
            Some("arib-std-b67") => "hlg",
            _ => "sdr",
        }
    })
}

/// Bit depth from bits_per_raw_sample or the pixel format name (yuv420p10le → 10).
fn bit_depth(stream: &RawStream) -> Option<i64> {
    if let Some(bits) = stream
        .bits_per_raw_sample
        .as_deref()
        .and_then(|b| b.parse().ok())
    {
        return Some(bits);
    }
    let pix = stream.pix_fmt.as_deref()?;
    Some(if pix.contains("p10") {
        10
    } else if pix.contains("p12") {
        12
    } else {
        8
    })
}

/// Parses ffprobe's JSON output.
pub fn parse(json: &[u8], file: &Path) -> Result<MediaInfo, ProbeError> {
    let raw: RawOutput = serde_json::from_slice(json)?;
    let format = raw.format;

    let streams = raw
        .streams
        .iter()
        .map(|s| {
            let kind = match s.codec_type.as_deref() {
                Some("video") => StreamKind::Video,
                Some("audio") => StreamKind::Audio,
                Some("subtitle") => StreamKind::Subtitle,
                Some("attachment") => StreamKind::Attachment,
                _ => StreamKind::Data,
            };
            // Cover art in MKV/MP4 shows up as a "video" stream with attached_pic.
            let title = tag(&s.tags, "title");
            StreamInfo {
                index: s.index,
                kind,
                codec: s.codec_name.clone(),
                profile: s.profile.clone(),
                level: s.level.filter(|l| *l > 0),
                width: s.width,
                height: s.height,
                fps: if kind == StreamKind::Video {
                    fraction(s.avg_frame_rate.as_deref())
                        .or_else(|| fraction(s.r_frame_rate.as_deref()))
                } else {
                    None
                },
                bit_depth: (kind == StreamKind::Video).then(|| bit_depth(s)).flatten(),
                pix_fmt: s.pix_fmt.clone(),
                color_transfer: s.color_transfer.clone(),
                color_primaries: s.color_primaries.clone(),
                hdr: hdr(s),
                channels: s.channels,
                channel_layout: s.channel_layout.clone(),
                sample_rate: s.sample_rate.as_deref().and_then(|r| r.parse().ok()),
                bitrate: s
                    .bit_rate
                    .as_deref()
                    .and_then(|b| b.parse().ok())
                    .or_else(|| tag(&s.tags, "BPS").and_then(|b| b.parse().ok())),
                language: tag(&s.tags, "language").filter(|l| l != "und"),
                is_sdh: s.disposition.hearing_impaired == 1
                    || title
                        .as_deref()
                        .is_some_and(|t| t.to_ascii_lowercase().contains("sdh")),
                title,
                is_default: s.disposition.default == 1,
                is_forced: s.disposition.forced == 1,
            }
        })
        .collect();

    let chapters = raw
        .chapters
        .iter()
        .filter_map(|c| {
            Some(Chapter {
                start_ms: seconds_to_ms(c.start_time.as_deref())?,
                end_ms: seconds_to_ms(c.end_time.as_deref())?,
                title: tag(&c.tags, "title"),
            })
        })
        .collect();

    Ok(MediaInfo {
        container: format
            .as_ref()
            .and_then(|f| container(f.format_name.as_deref(), file)),
        duration_ms: format
            .as_ref()
            .and_then(|f| seconds_to_ms(f.duration.as_deref())),
        bitrate: format
            .as_ref()
            .and_then(|f| f.bit_rate.as_deref())
            .and_then(|b| b.parse().ok()),
        streams,
        chapters,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyframes_from_packet_csv() {
        let csv = "0.000000,K__
0.040000,___
2.002000,K_
N/A,K__
1.001000,K__
2.002000,K__
";
        assert_eq!(parse_keyframes(csv), [0, 1001, 2002]);
    }

    const SAMPLE: &str = r#"{
      "streams": [
        { "index": 0, "codec_name": "hevc", "codec_type": "video", "profile": "Main 10",
          "width": 3840, "height": 2160, "pix_fmt": "yuv420p10le", "level": 153,
          "color_transfer": "smpte2084", "color_primaries": "bt2020",
          "avg_frame_rate": "24000/1001", "r_frame_rate": "24000/1001",
          "disposition": { "default": 1, "forced": 0 },
          "tags": { "language": "und" },
          "side_data_list": [ { "side_data_type": "DOVI configuration record" } ] },
        { "index": 1, "codec_name": "eac3", "codec_type": "audio", "channels": 6,
          "channel_layout": "5.1(side)", "sample_rate": "48000",
          "disposition": { "default": 1 }, "tags": { "language": "ger", "title": "Deutsch DD+" } },
        { "index": 2, "codec_name": "subrip", "codec_type": "subtitle",
          "disposition": { "default": 0, "forced": 1 }, "tags": { "language": "ger", "title": "Forced" } },
        { "index": 3, "codec_name": "hdmv_pgs_subtitle", "codec_type": "subtitle",
          "disposition": { "hearing_impaired": 1 }, "tags": { "LANGUAGE": "eng", "BPS": "35000" } }
      ],
      "chapters": [
        { "start_time": "0.000000", "end_time": "312.500000", "tags": { "title": "Intro" } }
      ],
      "format": { "format_name": "matroska,webm", "duration": "5400.123000", "bit_rate": "24000000" }
    }"#;

    #[test]
    fn parses_streams_hdr_languages_and_chapters() {
        let info = parse(SAMPLE.as_bytes(), Path::new("movie.mkv")).unwrap();
        assert_eq!(info.container.as_deref(), Some("mkv"));
        assert_eq!(info.duration_ms, Some(5_400_123));
        assert_eq!(info.bitrate, Some(24_000_000));

        let video = &info.streams[0];
        assert_eq!(video.kind, StreamKind::Video);
        assert_eq!(video.hdr, Some("dovi"));
        assert_eq!(video.bit_depth, Some(10));
        assert_eq!(video.fps, Some(23.976));
        assert_eq!(video.language, None, "und is no language");

        let audio = &info.streams[1];
        assert_eq!((audio.channels, audio.sample_rate), (Some(6), Some(48_000)));
        assert_eq!(audio.language.as_deref(), Some("ger"));

        assert!(info.streams[2].is_forced);
        assert!(info.streams[3].is_sdh);
        assert_eq!(info.streams[3].language.as_deref(), Some("eng"));
        assert_eq!(info.streams[3].bitrate, Some(35_000));

        assert_eq!(info.chapters[0].end_ms, 312_500);
        assert_eq!(info.chapters[0].title.as_deref(), Some("Intro"));
    }

    #[test]
    fn container_names_are_normalized() {
        assert_eq!(
            container(Some("matroska,webm"), Path::new("a.webm")).as_deref(),
            Some("webm")
        );
        assert_eq!(
            container(Some("mov,mp4,m4a,3gp,3g2,mj2"), Path::new("a.mp4")).as_deref(),
            Some("mp4")
        );
        assert_eq!(
            container(Some("mov,mp4,m4a,3gp,3g2,mj2"), Path::new("a.mov")).as_deref(),
            Some("mov")
        );
        assert_eq!(
            container(Some("mpegts"), Path::new("a.ts")).as_deref(),
            Some("ts")
        );
    }
}
