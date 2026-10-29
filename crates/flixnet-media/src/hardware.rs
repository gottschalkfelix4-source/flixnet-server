//! Hardware video encoding: which GPU pipelines the configured ffmpeg can run.
//!
//! At startup every candidate (NVENC, Quick Sync, VAAPI — whatever the platform can
//! have) runs a tiny test encode with exactly the encoder options real transcodes
//! use. A short HDR10 test clip then shows whether the GPU also decodes and which
//! tone mapping works on it. Detection runs in the background; until it is done, and
//! whenever nothing works, transcodes use libx264.
//!
//! The ffmpeg arguments for each pipeline are built in [`crate::hls`]; this module
//! only holds the typed description of an encoder and the detection.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::decision::{SourceVideo, StreamAction, VideoPlan};
use crate::hls;
use crate::process::command;

/// A GPU encoding method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Accel {
    /// NVIDIA NVENC/NVDEC (CUDA).
    Nvenc,
    /// Intel Quick Sync Video.
    Qsv,
    /// VA-API (Intel, AMD) on Linux.
    Vaapi,
}

impl Accel {
    /// Preference for `auto`: a dedicated NVIDIA card first, then Quick Sync (better
    /// presets than plain VA-API on the same Intel GPU), then VA-API.
    pub const PREFERENCE: [Accel; 3] = [Accel::Nvenc, Accel::Qsv, Accel::Vaapi];

    /// Whether the platform can have this method at all.
    pub fn possible_here() -> &'static [Accel] {
        if cfg!(target_os = "linux") {
            &Self::PREFERENCE
        } else if cfg!(windows) {
            // Quick Sync works through Direct3D 11 there; VA-API does not exist.
            &[Accel::Nvenc, Accel::Qsv]
        } else {
            &[]
        }
    }

    /// Whether ffmpeg needs the render node (`media.vaapi_device`).
    pub fn uses_render_node(self) -> bool {
        match self {
            Accel::Nvenc => false,
            Accel::Qsv => cfg!(not(windows)),
            Accel::Vaapi => true,
        }
    }
}

/// `media.hardware_acceleration`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HardwareMode {
    /// The best method that works.
    #[default]
    Auto,
    /// Always software (libx264).
    None,
    Qsv,
    Vaapi,
    Nvenc,
}

impl HardwareMode {
    fn candidates(self) -> Vec<Accel> {
        let only = |accel| {
            if Accel::possible_here().contains(&accel) {
                vec![accel]
            } else {
                vec![]
            }
        };
        match self {
            HardwareMode::Auto => Accel::possible_here().to_vec(),
            HardwareMode::None => vec![],
            HardwareMode::Qsv => only(Accel::Qsv),
            HardwareMode::Vaapi => only(Accel::Vaapi),
            HardwareMode::Nvenc => only(Accel::Nvenc),
        }
    }
}

/// How HDR is tone-mapped to SDR.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tonemap {
    /// On the CPU (zscale + tonemap), at the output size.
    #[default]
    Software,
    /// `tonemap_cuda` (jellyfin-ffmpeg).
    Cuda,
    /// `vpp_qsv` with `tonemap=1`.
    VppQsv,
    /// `tonemap_vaapi`.
    Vaapi,
}

impl Tonemap {
    /// The GPU method worth trying for an encoder.
    fn gpu_method(accel: Accel) -> Tonemap {
        match accel {
            Accel::Nvenc => Tonemap::Cuda,
            Accel::Qsv => Tonemap::VppQsv,
            Accel::Vaapi => Tonemap::Vaapi,
        }
    }
}

/// Which encoder a session runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EncoderKind {
    /// libx264 on the CPU.
    Software,
    Nvenc,
    Qsv,
    Vaapi,
}

impl From<Accel> for EncoderKind {
    fn from(accel: Accel) -> Self {
        match accel {
            Accel::Nvenc => EncoderKind::Nvenc,
            Accel::Qsv => EncoderKind::Qsv,
            Accel::Vaapi => EncoderKind::Vaapi,
        }
    }
}

/// A working hardware pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HwEncoder {
    pub accel: Accel,
    /// Render node for VA-API (and Quick Sync on Linux).
    pub device: PathBuf,
    /// Intel's low-power encoder (the only one on some recent GPUs).
    pub low_power: bool,
    /// The GPU decodes too, so frames never leave video memory.
    pub hw_decode: bool,
    /// Tone mapping when the GPU decodes; CPU-decoded HDR is always tone-mapped in
    /// software.
    pub tonemap: Tonemap,
}

impl HwEncoder {
    /// Whether the GPU decodes this source (else ffmpeg decodes on the CPU and only
    /// encodes on the GPU). Profiles the decoders lack are caught by the fallback.
    pub fn decodes(&self, source: Option<&SourceVideo>) -> bool {
        self.hw_decode && source.is_some_and(hw_decodable)
    }
}

/// Codecs NVDEC, Quick Sync and VA-API all decode (AV1 and VP9 need newer GPUs;
/// the fallback covers older ones).
fn hw_decodable(source: &SourceVideo) -> bool {
    match source.codec.as_str() {
        // No hardware decodes 10-bit H.264.
        "h264" => source.bit_depth <= 8,
        "hevc" | "vp9" | "av1" | "mpeg2video" | "vc1" => true,
        _ => false,
    }
}

/// The Quick Sync decoder for an ffprobe codec name.
pub(crate) fn qsv_decoder(codec: &str) -> Option<&'static str> {
    Some(match codec {
        "h264" => "h264_qsv",
        "hevc" => "hevc_qsv",
        "vp9" => "vp9_qsv",
        "av1" => "av1_qsv",
        "mpeg2video" => "mpeg2_qsv",
        "vc1" => "vc1_qsv",
        _ => return None,
    })
}

/// The video encoder of a transcode.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum VideoEncoder {
    /// libx264.
    #[default]
    Software,
    Hardware(HwEncoder),
}

impl VideoEncoder {
    pub fn kind(&self) -> EncoderKind {
        match self {
            VideoEncoder::Software => EncoderKind::Software,
            VideoEncoder::Hardware(hw) => hw.accel.into(),
        }
    }

    /// This encoder and what to try when it fails, most capable first: GPU decode +
    /// encode → CPU decode + GPU encode → software.
    pub fn fallbacks(&self, source: Option<&SourceVideo>) -> Vec<VideoEncoder> {
        let VideoEncoder::Hardware(hw) = self else {
            return vec![VideoEncoder::Software];
        };
        let mut chain = vec![self.clone()];
        if hw.decodes(source) {
            chain.push(VideoEncoder::Hardware(HwEncoder {
                hw_decode: false,
                ..hw.clone()
            }));
        }
        chain.push(VideoEncoder::Software);
        chain
    }
}

/// Settings for detection (from `media.*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareOptions {
    pub mode: HardwareMode,
    pub vaapi_device: PathBuf,
}

impl Default for HardwareOptions {
    fn default() -> Self {
        Self {
            mode: HardwareMode::Auto,
            vaapi_device: PathBuf::from("/dev/dri/renderD128"),
        }
    }
}

/// Result of testing one method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct EncoderProbe {
    pub accel: Accel,
    /// The test encode worked.
    pub available: bool,
    /// Intel's low-power encoder was needed.
    pub low_power: bool,
    /// The GPU decodes HEVC Main 10 too.
    pub hw_decode: bool,
    /// How HDR is tone-mapped with this method.
    pub tonemap: Tonemap,
    /// Render node used (VA-API, Quick Sync on Linux).
    pub device: Option<String>,
    /// Why encoding does not work (ffmpeg's last lines).
    pub error: Option<String>,
    /// Why GPU decoding does not work.
    pub decode_error: Option<String>,
    /// Why GPU tone mapping does not work.
    pub tonemap_error: Option<String>,
    /// Time the tests took (ms).
    pub duration_ms: i64,
}

impl EncoderProbe {
    fn encoder(&self, device: &Path) -> Option<VideoEncoder> {
        self.available.then(|| {
            VideoEncoder::Hardware(HwEncoder {
                accel: self.accel,
                device: device.to_owned(),
                low_power: self.low_power,
                hw_decode: self.hw_decode,
                tonemap: self.tonemap,
            })
        })
    }
}

/// Everything detection found out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HardwareReport {
    pub ffmpeg_version: Option<String>,
    pub probes: Vec<EncoderProbe>,
    pub device: PathBuf,
}

impl HardwareReport {
    /// The working encoder for a method.
    pub fn encoder(&self, accel: Accel) -> Option<VideoEncoder> {
        self.probes
            .iter()
            .find(|p| p.accel == accel)
            .and_then(|p| p.encoder(&self.device))
    }

    /// The best working method, by [`Accel::PREFERENCE`]; software if none works.
    pub fn best(&self) -> VideoEncoder {
        Accel::PREFERENCE
            .iter()
            .find_map(|accel| self.encoder(*accel))
            .unwrap_or_default()
    }

    /// Methods whose test encode worked.
    pub fn available(&self) -> Vec<Accel> {
        self.probes
            .iter()
            .filter(|p| p.available)
            .map(|p| p.accel)
            .collect()
    }
}

/// What transcodes run on, for the admin diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct TranscodingInfo {
    /// Configured `media.hardware_acceleration`.
    pub mode: HardwareMode,
    /// Detection has finished (until then transcodes use software).
    pub detected: bool,
    /// Encoder new transcodes use.
    pub encoder: EncoderKind,
    /// The GPU decodes too (where it can decode the source).
    pub hw_decode: bool,
    /// How HDR is tone-mapped to SDR.
    pub tonemap: Tonemap,
    /// First line of `ffmpeg -version`.
    pub ffmpeg_version: Option<String>,
    /// Every method that was tested, in test order.
    pub probes: Vec<EncoderProbe>,
    /// Hardware runs that failed and were retried with a simpler pipeline since the
    /// server started.
    pub fallbacks: u64,
}

/// One test run may take this long (Quick Sync on Windows needs seconds to start).
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Tests what `ffmpeg` can do. `work_dir` holds the HDR test clip; it is removed
/// afterwards. Never fails: what does not work is reported.
pub async fn detect(ffmpeg: &Path, work_dir: &Path, options: &HardwareOptions) -> HardwareReport {
    let mut report = HardwareReport {
        ffmpeg_version: ffmpeg_version(ffmpeg).await,
        probes: Vec::new(),
        device: options.vaapi_device.clone(),
    };
    let candidates = options.mode.candidates();
    if candidates.is_empty() {
        return report;
    }
    let sample = match hdr_sample(ffmpeg, work_dir).await {
        Ok(path) => Some(path),
        Err(err) => {
            tracing::warn!(%err, "no HDR test clip; GPU decoding and tone mapping stay off");
            None
        }
    };
    for accel in candidates {
        let probe = probe(ffmpeg, accel, &options.vaapi_device, sample.as_deref()).await;
        tracing::info!(
            ?accel,
            available = probe.available,
            hw_decode = probe.hw_decode,
            tonemap = ?probe.tonemap,
            error = probe.error.as_deref().unwrap_or(""),
            "hardware encoder tested"
        );
        report.probes.push(probe);
    }
    let _ = tokio::fs::remove_dir_all(work_dir).await;
    report
}

async fn ffmpeg_version(ffmpeg: &Path) -> Option<String> {
    let output = command(ffmpeg)
        .arg("-version")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let output = tokio::time::timeout(Duration::from_secs(10), output)
        .await
        .ok()?
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
}

/// Picture size of the test runs.
const TEST_WIDTH: i64 = 320;
const TEST_HEIGHT: i64 = 180;

/// A short HEVC Main 10 clip with HDR10 metadata (what 4K HDR films are).
async fn hdr_sample(ffmpeg: &Path, work_dir: &Path) -> Result<PathBuf, String> {
    tokio::fs::create_dir_all(work_dir)
        .await
        .map_err(|e| e.to_string())?;
    let path = work_dir.join("hdr10.mkv");
    let mut args: Vec<OsString> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=640x360:r=25:d=0.4",
        "-vf",
        "format=yuv420p10le",
        "-c:v",
        "libx265",
        "-preset",
        "ultrafast",
        "-x265-params",
        "log-level=error:hdr10=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:\
         master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):\
         max-cll=1000,400",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.push(path.clone().into_os_string());
    run(ffmpeg, &args).await.map(|()| path)
}

/// Tests one method: encoding, then GPU decoding and tone mapping of the HDR clip.
async fn probe(ffmpeg: &Path, accel: Accel, device: &Path, sample: Option<&Path>) -> EncoderProbe {
    let started = std::time::Instant::now();
    let mut probe = EncoderProbe {
        accel,
        available: false,
        low_power: false,
        hw_decode: false,
        tonemap: Tonemap::Software,
        device: accel
            .uses_render_node()
            .then(|| device.display().to_string()),
        error: None,
        decode_error: None,
        tonemap_error: None,
        duration_ms: 0,
    };
    let mut hw = HwEncoder {
        accel,
        device: device.to_owned(),
        low_power: false,
        hw_decode: false,
        tonemap: Tonemap::Software,
    };

    let encodes = if accel.uses_render_node() && !device.exists() {
        Err(format!("{} nicht vorhanden", device.display()))
    } else {
        match test_run(ffmpeg, &hw, None, false).await {
            Ok(()) => Ok(()),
            // Some Intel GPUs only have the low-power encoder.
            Err(err) if matches!(accel, Accel::Qsv | Accel::Vaapi) => {
                hw.low_power = true;
                test_run(ffmpeg, &hw, None, false).await.map_err(|_| err)
            }
            Err(err) => Err(err),
        }
    };
    match encodes {
        Ok(()) => {
            probe.available = true;
            probe.low_power = hw.low_power;
        }
        Err(err) => {
            probe.error = Some(err);
            probe.duration_ms = elapsed_ms(started);
            return probe;
        }
    }

    match sample {
        None => probe.decode_error = Some("kein HDR-Testclip (libx265 fehlt?)".to_owned()),
        Some(sample) => {
            hw.hw_decode = true;
            match test_run(ffmpeg, &hw, Some(sample), false).await {
                Ok(()) => probe.hw_decode = true,
                Err(err) => {
                    hw.hw_decode = false;
                    probe.decode_error = Some(err);
                }
            }
            if probe.hw_decode {
                hw.tonemap = Tonemap::gpu_method(accel);
                match test_run(ffmpeg, &hw, Some(sample), true).await {
                    Ok(()) => probe.tonemap = hw.tonemap,
                    Err(err) => probe.tonemap_error = Some(err),
                }
            }
        }
    }
    probe.duration_ms = elapsed_ms(started);
    probe
}

fn elapsed_ms(started: std::time::Instant) -> i64 {
    i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)
}

/// The source the test runs read: test picture (SDR, decoded on the CPU) or the HDR clip.
fn sample_source() -> SourceVideo {
    SourceVideo {
        index: 0,
        codec: "hevc".into(),
        width: 640,
        height: 360,
        bit_depth: 10,
        hdr: "hdr10".into(),
    }
}

/// One transcode as real sessions run it, into the null muxer.
async fn test_run(
    ffmpeg: &Path,
    hw: &HwEncoder,
    sample: Option<&Path>,
    tonemap: bool,
) -> Result<(), String> {
    let plan = VideoPlan {
        stream_index: 0,
        action: StreamAction::Transcode,
        codec: "h264".into(),
        width: TEST_WIDTH,
        height: TEST_HEIGHT,
        bitrate: None,
        tonemap,
    };
    let source = sample_source();
    let encoder = VideoEncoder::Hardware(hw.clone());
    let args = hls::test_args(sample, &plan, &encoder, sample.map(|_| &source));
    run(ffmpeg, &args).await
}

/// Runs ffmpeg to completion; the error is the end of its log.
async fn run(ffmpeg: &Path, args: &[OsString]) -> Result<(), String> {
    let child = command(ffmpeg)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg nicht startbar: {e}"))?;
    let output = tokio::time::timeout(PROBE_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| format!("Zeitüberschreitung nach {} s", PROBE_TIMEOUT.as_secs()))?
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    Err(log_tail(&String::from_utf8_lossy(&output.stderr)))
}

/// The last few lines of an ffmpeg log, short enough for the UI.
pub(crate) fn log_tail(log: &str) -> String {
    let lines: Vec<&str> = log
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let tail = lines[lines.len().saturating_sub(4)..].join(" | ");
    let mut tail = if tail.is_empty() {
        "ffmpeg ist ohne Meldung fehlgeschlagen".to_owned()
    } else {
        tail
    };
    if tail.len() > 600 {
        let mut cut = 600;
        while !tail.is_char_boundary(cut) {
            cut -= 1;
        }
        tail.truncate(cut);
        tail.push('…');
    }
    tail
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hw(accel: Accel) -> HwEncoder {
        HwEncoder {
            accel,
            device: PathBuf::from("/dev/dri/renderD128"),
            low_power: false,
            hw_decode: true,
            tonemap: Tonemap::Software,
        }
    }

    fn source(codec: &str, bit_depth: i64) -> SourceVideo {
        SourceVideo {
            codec: codec.into(),
            bit_depth,
            ..sample_source()
        }
    }

    #[test]
    fn gpu_decodes_only_what_it_can() {
        let nvenc = hw(Accel::Nvenc);
        assert!(nvenc.decodes(Some(&source("hevc", 10))));
        assert!(nvenc.decodes(Some(&source("h264", 8))));
        assert!(!nvenc.decodes(Some(&source("h264", 10))), "Hi10P");
        assert!(!nvenc.decodes(Some(&source("mpeg4", 8))));
        assert!(!nvenc.decodes(None), "unknown source");
        let off = HwEncoder {
            hw_decode: false,
            ..nvenc
        };
        assert!(!off.decodes(Some(&source("hevc", 10))));
    }

    #[test]
    fn fallbacks_end_in_software() {
        let hevc = source("hevc", 10);
        let encoder = VideoEncoder::Hardware(hw(Accel::Qsv));
        let chain = encoder.fallbacks(Some(&hevc));
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0], encoder);
        assert!(matches!(&chain[1], VideoEncoder::Hardware(h) if !h.hw_decode));
        assert_eq!(chain[2], VideoEncoder::Software);
        // Nothing to drop when the GPU would not decode anyway.
        assert_eq!(encoder.fallbacks(Some(&source("mpeg4", 8))).len(), 2);
        assert_eq!(
            VideoEncoder::Software.fallbacks(None),
            [VideoEncoder::Software]
        );
    }

    #[test]
    fn best_follows_the_preference() {
        let probe = |accel, available| EncoderProbe {
            accel,
            available,
            low_power: false,
            hw_decode: true,
            tonemap: Tonemap::Software,
            device: None,
            error: None,
            decode_error: None,
            tonemap_error: None,
            duration_ms: 0,
        };
        let mut report = HardwareReport {
            probes: vec![
                probe(Accel::Vaapi, true),
                probe(Accel::Qsv, true),
                probe(Accel::Nvenc, false),
            ],
            ..Default::default()
        };
        assert_eq!(report.best().kind(), EncoderKind::Qsv);
        assert_eq!(report.available(), [Accel::Vaapi, Accel::Qsv]);
        report.probes.clear();
        assert_eq!(report.best(), VideoEncoder::Software);
    }

    #[test]
    fn modes_pick_candidates() {
        assert!(HardwareMode::None.candidates().is_empty());
        let auto = HardwareMode::Auto.candidates();
        if cfg!(windows) {
            assert_eq!(auto, [Accel::Nvenc, Accel::Qsv]);
            assert!(
                HardwareMode::Vaapi.candidates().is_empty(),
                "no VA-API on Windows"
            );
        }
        if cfg!(target_os = "linux") {
            assert_eq!(auto, Accel::PREFERENCE);
            assert_eq!(HardwareMode::Vaapi.candidates(), [Accel::Vaapi]);
        }
    }

    #[test]
    fn log_tail_is_short() {
        assert_eq!(log_tail("a\n\nb\n c \nd\ne\n"), "b | c | d | e");
        assert_eq!(log_tail(""), "ffmpeg ist ohne Meldung fehlgeschlagen");
        assert!(log_tail(&"ä".repeat(1000)).chars().count() <= 301);
    }
}
