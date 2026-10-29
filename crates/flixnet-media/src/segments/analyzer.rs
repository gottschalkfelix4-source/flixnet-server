//! The ffmpeg runs behind segment detection: Chromaprint fingerprints of a stretch of
//! audio, and black/silent gaps around an edge. Arguments are built from typed values
//! only (paths from the database, stream indexes and times as numbers).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::sync::OnceCell;

use super::SNAP_TOLERANCE_MS;
use super::matching::{Fingerprint, Gap, Span, refine};
use crate::process::command;

#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error("this ffmpeg build has no Chromaprint muxer")]
    Unavailable,
    #[error("could not start ffmpeg: {0}")]
    Spawn(std::io::Error),
    #[error("ffmpeg timed out after {0:?}")]
    Timeout(Duration),
    #[error("ffmpeg failed: {0}")]
    Failed(String),
}

/// Runs the analysis passes of one ffmpeg binary. Cheap to clone.
#[derive(Debug, Clone)]
pub struct SegmentAnalyzer {
    ffmpeg: PathBuf,
    timeout: Duration,
    chromaprint: std::sync::Arc<OnceCell<bool>>,
}

impl SegmentAnalyzer {
    pub fn new(ffmpeg: impl Into<PathBuf>) -> Self {
        Self {
            ffmpeg: ffmpeg.into(),
            // Reading ten minutes of a remux from a slow share can take a while.
            timeout: Duration::from_secs(300),
            chromaprint: Default::default(),
        }
    }

    /// Whether ffmpeg was built with `--enable-chromaprint` (checked once).
    pub async fn chromaprint_available(&self) -> bool {
        *self
            .chromaprint
            .get_or_init(|| async {
                let output = command(&self.ffmpeg)
                    .args(["-hide_banner", "-muxers"])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .output()
                    .await;
                let found = output.is_ok_and(|o| has_chromaprint(&String::from_utf8_lossy(&o.stdout)));
                if !found {
                    tracing::warn!(ffmpeg = %self.ffmpeg.display(), "ffmpeg has no Chromaprint muxer; intro and credits detection is off");
                }
                found
            })
            .await
    }

    /// Fingerprint of `duration_ms` of audio stream `stream_index` from `start_ms` on.
    pub async fn fingerprint(
        &self,
        file: &Path,
        stream_index: i64,
        start_ms: i64,
        duration_ms: i64,
    ) -> Result<Fingerprint, AnalysisError> {
        if !self.chromaprint_available().await {
            return Err(AnalysisError::Unavailable);
        }
        let args = fingerprint_args(file, stream_index, start_ms, duration_ms);
        let output = self.run(&args, true).await?;
        Ok(Fingerprint::new(start_ms, parse_raw(&output.stdout)))
    }

    /// Black and silent stretches within `duration_ms` from `start_ms` (absolute times).
    /// Black and silence are both reported as gaps; either stream may be left out.
    pub async fn gaps(
        &self,
        file: &Path,
        video_stream: Option<i64>,
        audio_stream: Option<i64>,
        start_ms: i64,
        duration_ms: i64,
    ) -> Result<Vec<Gap>, AnalysisError> {
        if video_stream.is_none() && audio_stream.is_none() {
            return Ok(Vec::new());
        }
        let args = gap_args(file, video_stream, audio_stream, start_ms, duration_ms);
        let output = self.run(&args, false).await?;
        Ok(parse_gaps(
            &String::from_utf8_lossy(&output.stderr),
            start_ms,
            start_ms + duration_ms,
        ))
    }

    /// Snaps the edges of a fingerprint match to black/silent gaps close by.
    pub async fn refine_edges(
        &self,
        file: &Path,
        video_stream: Option<i64>,
        audio_stream: Option<i64>,
        span: Span,
    ) -> Result<Span, AnalysisError> {
        let reach = SNAP_TOLERANCE_MS + 1_000;
        let mut gaps = Vec::new();
        for edge in [span.start_ms, span.end_ms] {
            let start = (edge - reach).max(0);
            gaps.extend(
                self.gaps(
                    file,
                    video_stream,
                    audio_stream,
                    start,
                    edge + reach - start,
                )
                .await?,
            );
        }
        Ok(refine(span, &gaps, SNAP_TOLERANCE_MS))
    }

    async fn run(
        &self,
        args: &[OsString],
        stdout: bool,
    ) -> Result<std::process::Output, AnalysisError> {
        let child = command(&self.ffmpeg)
            .args(args)
            .stdout(if stdout {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::piped())
            .spawn()
            .map_err(AnalysisError::Spawn)?;
        // Dropping the child on timeout kills ffmpeg.
        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| AnalysisError::Timeout(self.timeout))?
            .map_err(AnalysisError::Spawn)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = stderr.lines().last().unwrap_or("unknown error").trim();
            return Err(AnalysisError::Failed(message.to_owned()));
        }
        Ok(output)
    }
}

fn has_chromaprint(muxers: &str) -> bool {
    muxers
        .lines()
        .any(|line| line.split_whitespace().nth(1) == Some("chromaprint"))
}

fn seconds(ms: i64) -> String {
    format!("{:.3}", ms.max(0) as f64 / 1000.0)
}

/// Seeks before opening the input (fast, and timestamps restart at 0 there).
fn input_args(file: &Path, start_ms: i64, duration_ms: i64) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-hide_banner", "-nostdin"].map(OsString::from).to_vec();
    if start_ms > 0 {
        args.extend(["-ss".into(), seconds(start_ms).into()]);
    }
    args.extend(["-t".into(), seconds(duration_ms).into()]);
    args.push("-i".into());
    args.push(file.as_os_str().to_owned());
    args
}

/// Raw Chromaprint fingerprint (little-endian `u32` items) of one audio stream on stdout.
pub fn fingerprint_args(
    file: &Path,
    stream_index: i64,
    start_ms: i64,
    duration_ms: i64,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["-loglevel".into(), "error".into()];
    args.extend(input_args(file, start_ms, duration_ms));
    for arg in [
        "-map",
        &format!("0:{stream_index}"),
        "-vn",
        "-sn",
        "-dn",
        "-ac",
        "1",
    ] {
        args.push(arg.into());
    }
    for arg in ["-f", "chromaprint", "-fp_format", "raw", "-"] {
        args.push(arg.into());
    }
    args
}

/// `blackdetect`/`silencedetect` over a short window; results are logged to stderr.
pub fn gap_args(
    file: &Path,
    video_stream: Option<i64>,
    audio_stream: Option<i64>,
    start_ms: i64,
    duration_ms: i64,
) -> Vec<OsString> {
    // The filters report at info level.
    let mut args: Vec<OsString> = vec!["-loglevel".into(), "info".into()];
    args.extend(input_args(file, start_ms, duration_ms));
    if let Some(index) = video_stream {
        for arg in [
            "-map",
            &format!("0:{index}"),
            "-vf",
            "blackdetect=d=0.1:pix_th=0.10",
        ] {
            args.push(arg.into());
        }
    }
    if let Some(index) = audio_stream {
        for arg in [
            "-map",
            &format!("0:{index}"),
            "-af",
            "silencedetect=noise=-50dB:d=0.3",
        ] {
            args.push(arg.into());
        }
    }
    for arg in ["-sn", "-dn", "-f", "null", "-"] {
        args.push(arg.into());
    }
    args
}

fn parse_raw(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect()
}

/// Gaps from the filters' log lines (times relative to the window start):
/// `[blackdetect @ …] black_start:12.3 black_end:13.3 black_duration:1`,
/// `[silencedetect @ …] silence_start: 12.31` … `silence_end: 13.3 | silence_duration: 0.99`.
fn parse_gaps(log: &str, offset_ms: i64, window_end_ms: i64) -> Vec<Gap> {
    let value = |line: &str, key: &str| -> Option<f64> {
        let rest = &line[line.find(key)? + key.len()..];
        rest.trim_start()
            .split(|c: char| c.is_whitespace() || c == '|')
            .next()?
            .parse()
            .ok()
    };
    let ms = |secs: f64| offset_ms + (secs * 1000.0).round() as i64;
    let mut gaps = Vec::new();
    let mut silence_start = None;
    for line in log.lines() {
        if line.contains("black_start:")
            && let (Some(start), Some(end)) =
                (value(line, "black_start:"), value(line, "black_end:"))
        {
            gaps.push(Gap {
                start_ms: ms(start),
                end_ms: ms(end),
            });
        } else if line.contains("silence_start:") {
            silence_start = value(line, "silence_start:");
        } else if line.contains("silence_end:")
            && let Some(end) = value(line, "silence_end:")
        {
            let start = silence_start.take().unwrap_or(0.0).max(0.0);
            gaps.push(Gap {
                start_ms: ms(start),
                end_ms: ms(end),
            });
        }
    }
    // Silence still running at the end of the window.
    if let Some(start) = silence_start {
        gaps.push(Gap {
            start_ms: ms(start.max(0.0)),
            end_ms: window_end_ms,
        });
    }
    gaps.retain(|g| g.end_ms > g.start_ms);
    gaps.sort_by_key(|g| g.start_ms);
    gaps
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
    fn fingerprint_arguments() {
        let file = Path::new("/media/Serie (2020)/S01E01.mkv");
        assert_eq!(
            joined(&fingerprint_args(file, 2, 0, 600_000)),
            "-loglevel error -hide_banner -nostdin -t 600.000 -i /media/Serie (2020)/S01E01.mkv \
             -map 0:2 -vn -sn -dn -ac 1 -f chromaprint -fp_format raw -"
        );
        let tail = joined(&fingerprint_args(file, 1, 1_234_567, 300_000));
        assert!(tail.contains("-ss 1234.567 -t 300.000 -i"), "{tail}");
        assert!(
            fingerprint_args(file, 1, 0, 1)
                .iter()
                .any(|a| a == file.as_os_str())
        );
    }

    #[test]
    fn gap_arguments() {
        let file = Path::new("/m/e.mkv");
        assert_eq!(
            joined(&gap_args(file, Some(0), Some(1), 57_500, 6_000)),
            "-loglevel info -hide_banner -nostdin -ss 57.500 -t 6.000 -i /m/e.mkv \
             -map 0:0 -vf blackdetect=d=0.1:pix_th=0.10 -map 0:1 -af silencedetect=noise=-50dB:d=0.3 \
             -sn -dn -f null -"
        );
        assert!(!joined(&gap_args(file, None, Some(1), 0, 6_000)).contains("blackdetect"));
    }

    #[test]
    fn chromaprint_in_muxer_list() {
        let list = "File formats:\n D. = Demuxing\n --\n  E  chromaprint     Chromaprint\n  E  crc             CRC testing\n";
        assert!(has_chromaprint(list));
        assert!(!has_chromaprint("  E  crc             CRC testing\n"));
    }

    #[test]
    fn raw_items_are_little_endian() {
        assert_eq!(
            parse_raw(&[1, 0, 0, 0, 0xff, 0xff, 0xff, 0x7f, 9]),
            vec![1, 0x7fff_ffff]
        );
    }

    #[test]
    fn gaps_from_filter_logs() {
        let log = "\
[blackdetect @ 0000020] black_start:1.5 black_end:2.6 black_duration:1.1
[silencedetect @ 0000021] silence_start: 1.48
[silencedetect @ 0000021] silence_end: 2.61 | silence_duration: 1.13
frame=  60 fps=0.0 q=-0.0 size=N/A time=00:00:06.00
[silencedetect @ 0000021] silence_start: 5.2
";
        let gaps = parse_gaps(log, 100_000, 106_000);
        assert_eq!(
            gaps,
            vec![
                Gap {
                    start_ms: 101_480,
                    end_ms: 102_610
                },
                Gap {
                    start_ms: 101_500,
                    end_ms: 102_600
                },
                Gap {
                    start_ms: 105_200,
                    end_ms: 106_000
                },
            ]
        );
        assert!(parse_gaps("nothing here", 0, 1).is_empty());
    }
}
