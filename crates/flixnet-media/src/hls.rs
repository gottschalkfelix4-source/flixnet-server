//! HLS output: ffmpeg arguments for a segment run and the playlist the server hands out.
//!
//! The server writes the full VOD playlist itself, so players can seek anywhere before
//! anything is produced. ffmpeg is (re)started at any segment, and segments from
//! different runs must fit together:
//!
//! * `-copyts -start_at_zero` keeps the file's own timeline, and `frag_discont` makes
//!   the fMP4 fragments carry those absolute times — so the init segment is the same
//!   for every run (no per-run edit list) and a segment's time never depends on where
//!   its run started.
//! * With a tiny `hls_time` the muxer cuts at *every* keyframe. Copied video has the
//!   file's keyframes (one segment per GOP, from the keyframe index); transcodes get
//!   exactly one keyframe per [`SEGMENT_SECS`] slot of the absolute timeline (the
//!   first frame at or after each multiple), so both cut at the same frames from any
//!   starting point.
//! * Hardware encoders ([`crate::hardware`]) keep that rule: forced keyframes become
//!   IDR frames, and the encoder inserts none of its own (no scene cuts, a GOP longer
//!   than any film). Their pipelines decode, scale and tone-map on the GPU where it
//!   can; whatever it cannot is done on the CPU.
//!
//! * Picture subtitles chosen for burn-in come from the same input, so `-ss` and
//!   `-copyts` move them with the video: they stay in sync after every restart. A
//!   subtitle shown since before a run's start point appears only from the next one
//!   on (the input seek skips its packet).
//!
//! Every argument is built from typed values and paths chosen by the server — never
//! from request strings.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::Path;

use crate::decision::{PlaybackPlan, SourceVideo, StreamAction, SubtitleDelivery, VideoPlan};
use crate::hardware::{Accel, HwEncoder, Tonemap, VideoEncoder, qsv_decoder};

/// Length of every segment except the last.
pub const SEGMENT_SECS: u32 = 6;

/// Init segment written by ffmpeg (fMP4 header, identical for every run).
pub const INIT_FILE: &str = "init.mp4";
/// Playlist ffmpeg keeps up to date; lists segments once they are complete.
pub const FFMPEG_PLAYLIST: &str = "ffmpeg.m3u8";

/// Audio codecs that may be copied into the fMP4 segments. Everything else is
/// converted to AAC: MP3 (and MP2) in particular play as files in every browser, but
/// Media Source rejects them inside MP4, and Apple's HLS only knows them in MPEG-TS.
pub fn fmp4_audio_codec(codec: &str) -> bool {
    matches!(codec, "aac" | "ac3" | "eac3" | "opus" | "flac")
}

pub fn segment_file(index: u32) -> String {
    format!("seg{index}.m4s")
}

/// Where the segments of a session start; the last one ends with the media.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segments {
    starts_ms: Vec<i64>,
    duration_ms: i64,
    keyframe_aligned: bool,
}

impl Segments {
    /// Equal [`SEGMENT_SECS`] segments (transcodes, which force their own keyframes).
    pub fn fixed(duration_ms: i64) -> Self {
        let duration_ms = duration_ms.max(1);
        let step = i64::from(SEGMENT_SECS) * 1000;
        Self {
            starts_ms: (0..duration_ms).step_by(step as usize).collect(),
            duration_ms,
            keyframe_aligned: false,
        }
    }

    /// One segment per GOP (copied video). Keyframe times are taken relative to the
    /// first one, which is where the file starts.
    pub fn keyframes(keyframes_ms: &[i64], duration_ms: i64) -> Option<Self> {
        let first = *keyframes_ms.first()?;
        let duration_ms = duration_ms.max(1);
        let starts_ms = keyframes_ms
            .iter()
            .map(|k| k - first)
            .filter(|k| *k < duration_ms)
            .collect();
        Some(Self {
            starts_ms,
            duration_ms,
            keyframe_aligned: true,
        })
    }

    pub fn count(&self) -> u32 {
        u32::try_from(self.starts_ms.len()).unwrap_or(u32::MAX)
    }

    pub fn start_ms(&self, index: u32) -> i64 {
        self.starts_ms
            .get(index as usize)
            .copied()
            .unwrap_or(self.duration_ms)
    }

    pub fn length_ms(&self, index: u32) -> i64 {
        (self.start_ms(index + 1) - self.start_ms(index)).max(1)
    }

    pub fn is_keyframe_aligned(&self) -> bool {
        self.keyframe_aligned
    }
}

/// The complete playlist. Segment and init URIs are relative (`init.mp4`, `{n}.m4s`),
/// so they inherit the playlist's location.
pub fn vod_playlist(segments: &Segments) -> String {
    let count = segments.count();
    let longest = (0..count)
        .map(|n| segments.length_ms(n))
        .max()
        .unwrap_or(1000);
    let mut out = String::with_capacity(64 + count as usize * 24);
    out.push_str("#EXTM3U\n#EXT-X-VERSION:7\n");
    let _ = writeln!(out, "#EXT-X-TARGETDURATION:{}", (longest + 999) / 1000);
    out.push_str(
        "#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-INDEPENDENT-SEGMENTS\n",
    );
    let _ = writeln!(out, "#EXT-X-MAP:URI=\"{INIT_FILE}\"");
    for n in 0..count {
        let length = segments.length_ms(n);
        let _ = writeln!(out, "#EXTINF:{:.3},\n{n}.m4s", length as f64 / 1000.0);
    }
    out.push_str("#EXT-X-ENDLIST\n");
    out
}

/// `12345` ms → `12.345` (ffmpeg time syntax).
fn seconds(ms: i64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

/// Software tone mapping HDR → SDR (needs ffmpeg with zimg, as jellyfin-ffmpeg has).
pub(crate) const TONEMAP: &str = "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,\
                       tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv,format=yuv420p";

/// Exactly one keyframe per [`SEGMENT_SECS`] slot of the absolute timeline: the first
/// frame at or after each multiple (see the module docs).
fn force_key_frames() -> String {
    format!(
        "expr:if(isnan(prev_forced_t),1,gt(floor(t/{SEGMENT_SECS}),floor(prev_forced_t/{SEGMENT_SECS})))"
    )
}

/// GOP length for hardware encoders: never reached in practice, so the forced
/// keyframes are the only ones. Quick Sync stores it in 16 bits.
const HW_GOP: &str = "100000";
const QSV_GOP: &str = "65535";

/// One ffmpeg run producing segments from `start_segment` on.
#[derive(Debug, Clone, Copy)]
pub struct HlsJob<'a> {
    pub input: &'a Path,
    pub out_dir: &'a Path,
    pub start_segment: u32,
    pub segments: &'a Segments,
    pub plan: &'a PlaybackPlan,
    /// Encoder for a video transcode (unused when the video is copied).
    pub encoder: &'a VideoEncoder,
    /// The source's video stream, so the GPU decodes only what it can.
    pub source: Option<&'a SourceVideo>,
}

pub fn hls_args(job: &HlsJob<'_>) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |values: &[&str]| args.extend(values.iter().map(OsString::from));
    push(&["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]);

    let transcode_video = job
        .plan
        .video
        .as_ref()
        .is_some_and(|v| v.action == StreamAction::Transcode);
    // Burned-in subtitles run on the software encoder (the overlay is a CPU filter).
    let burn_in = burn_in_stream(job.plan);
    if transcode_video && burn_in.is_none() {
        args.extend(hw_input_args(job.encoder, job.source, cfg!(windows)));
    }
    let mut push = |values: &[&str]| args.extend(values.iter().map(OsString::from));

    let start_ms = job.segments.start_ms(job.start_segment);
    if start_ms > 0 {
        // Copying starts at the keyframe at or before the seek position. ffmpeg up to
        // 7 also seeks ~130 ms early for streams with decode delay, so aim a bit past
        // the segment's keyframe (but well within its GOP).
        let margin = if job.segments.is_keyframe_aligned() {
            (job.segments.length_ms(job.start_segment) / 2).min(150)
        } else {
            0
        };
        let seek_ms = start_ms + margin;
        push(&["-ss", &seconds(seek_ms)]);
    }
    push(&["-copyts", "-start_at_zero"]);
    args.push("-i".into());
    args.push(job.input.as_os_str().to_owned());
    let mut push = |values: &[&str]| args.extend(values.iter().map(OsString::from));

    match &job.plan.video {
        Some(video) => {
            if burn_in.is_none() {
                push(&["-map", &format!("0:{}", video.stream_index)]);
            }
            match video.action {
                StreamAction::Copy => {
                    push(&["-c:v", "copy"]);
                    if video.codec == "hevc" {
                        // Apple players only accept HEVC tagged as hvc1.
                        push(&["-tag:v", "hvc1"]);
                    }
                }
                StreamAction::Transcode => {
                    args.extend(match burn_in {
                        None => video_transcode_args(video, job.encoder, job.source),
                        Some(subtitle) => burn_in_args(video, subtitle),
                    });
                }
            }
        }
        None => push(&["-vn"]),
    }
    let mut push = |values: &[&str]| args.extend(values.iter().map(OsString::from));

    match &job.plan.audio {
        Some(audio) => {
            push(&["-map", &format!("0:{}", audio.stream_index)]);
            match audio.action {
                StreamAction::Copy => push(&["-c:a", "copy"]),
                StreamAction::Transcode => {
                    let channels = audio.channels.clamp(1, 6);
                    let bitrate = (channels * 64_000).min(384_000);
                    push(&[
                        "-c:a",
                        "aac",
                        "-ac",
                        &channels.to_string(),
                        "-b:a",
                        &bitrate.to_string(),
                    ]);
                }
            }
        }
        None => push(&["-an"]),
    }

    push(&[
        "-sn",
        "-dn",
        "-max_muxing_queue_size",
        "2048",
        "-avoid_negative_ts",
        "disabled",
        "-f",
        "hls",
        // Cut at every keyframe (see the module docs).
        "-hls_time",
        "0.001",
        "-hls_segment_options",
        "movflags=+frag_discont",
        "-hls_segment_type",
        "fmp4",
        "-hls_fmp4_init_filename",
        INIT_FILE,
        "-hls_flags",
        "independent_segments+temp_file",
        "-hls_list_size",
        "0",
        "-hls_playlist_type",
        "event",
        "-start_number",
        &job.start_segment.to_string(),
        "-hls_segment_filename",
    ]);
    args.push(job.out_dir.join("seg%d.m4s").into_os_string());
    args.push(job.out_dir.join(FFMPEG_PLAYLIST).into_os_string());
    args
}

/// A short test transcode into the null muxer, with exactly the pipeline sessions
/// use: from `input`, or from a test picture (decoded on the CPU) without one.
pub(crate) fn test_args(
    input: Option<&Path>,
    video: &VideoPlan,
    encoder: &VideoEncoder,
    source: Option<&SourceVideo>,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-hide_banner", "-loglevel", "error", "-nostdin"]
        .into_iter()
        .map(OsString::from)
        .collect();
    let source = input.and(source);
    args.extend(hw_input_args(encoder, source, cfg!(windows)));
    match input {
        Some(path) => {
            args.push("-i".into());
            args.push(path.as_os_str().to_owned());
        }
        None => args.extend(
            ["-f", "lavfi", "-i", "testsrc2=s=640x360:r=25:d=0.4"]
                .into_iter()
                .map(OsString::from),
        ),
    }
    args.extend(["-map", "0:v:0"].into_iter().map(OsString::from));
    args.extend(video_transcode_args(video, encoder, source));
    args.extend(
        ["-an", "-sn", "-f", "null", "-"]
            .into_iter()
            .map(OsString::from),
    );
    args
}

/// Input options for a hardware pipeline: devices, and GPU decoding when the GPU
/// can decode the source. `windows` selects how Quick Sync finds the GPU.
fn hw_input_args(
    encoder: &VideoEncoder,
    source: Option<&SourceVideo>,
    windows: bool,
) -> Vec<OsString> {
    let VideoEncoder::Hardware(hw) = encoder else {
        return Vec::new();
    };
    let gpu = hw.decodes(source);
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |values: &[&str]| args.extend(values.iter().map(OsString::from));
    // `vaapi=va:/dev/dri/renderD128` (the node comes from the configuration).
    let vaapi_device = || {
        let mut value = OsString::from("vaapi=va:");
        value.push(hw.device.as_os_str());
        value
    };
    match hw.accel {
        Accel::Nvenc => {
            if gpu {
                push(&["-hwaccel", "cuda", "-hwaccel_output_format", "cuda"]);
            }
        }
        Accel::Qsv => {
            if windows {
                push(&[
                    "-init_hw_device",
                    "d3d11va=dx:,vendor_id=0x8086",
                    "-init_hw_device",
                    "qsv=qs@dx",
                ]);
            } else {
                push(&["-init_hw_device"]);
                args.push(vaapi_device());
                args.extend(["-init_hw_device", "qsv=qs@va"].map(OsString::from));
            }
            args.extend(["-filter_hw_device", "qs"].map(OsString::from));
            if gpu {
                args.extend(
                    [
                        "-hwaccel",
                        "qsv",
                        "-hwaccel_device",
                        "qs",
                        "-hwaccel_output_format",
                        "qsv",
                    ]
                    .map(OsString::from),
                );
                // Quick Sync decodes with its own decoders, chosen by name.
                if let Some(decoder) = source.and_then(|s| qsv_decoder(&s.codec)) {
                    args.extend(["-c:v", decoder].map(OsString::from));
                }
            }
        }
        Accel::Vaapi => {
            push(&["-init_hw_device"]);
            args.push(vaapi_device());
            args.extend(["-filter_hw_device", "va"].map(OsString::from));
            if gpu {
                args.extend(
                    [
                        "-hwaccel",
                        "vaapi",
                        "-hwaccel_device",
                        "va",
                        "-hwaccel_output_format",
                        "vaapi",
                    ]
                    .map(OsString::from),
                );
            }
        }
    }
    args
}

/// Filters and encoder options for a video transcode (after `-map`).
fn video_transcode_args(
    video: &VideoPlan,
    encoder: &VideoEncoder,
    source: Option<&SourceVideo>,
) -> Vec<OsString> {
    let mut args: Vec<String> = Vec::new();
    match encoder {
        VideoEncoder::Software => {
            let mut filters = format!("scale={}:{}", video.width, video.height);
            if video.tonemap {
                filters.push(',');
                filters.push_str(TONEMAP);
            } else {
                filters.push_str(",format=yuv420p");
            }
            args.extend(
                [
                    "-c:v",
                    "libx264",
                    "-preset",
                    "veryfast",
                    "-crf",
                    "23",
                    "-profile:v",
                    "high",
                    "-vf",
                    &filters,
                    // No keyframes of its own: they would add cuts.
                    "-x264-params",
                    "keyint=infinite:scenecut=0",
                    "-force_key_frames",
                    &force_key_frames(),
                ]
                .map(str::to_owned),
            );
            if let Some(bitrate) = video.bitrate {
                args.extend([
                    "-maxrate".to_owned(),
                    bitrate.to_string(),
                    "-bufsize".to_owned(),
                    (bitrate * 2).to_string(),
                ]);
            }
        }
        VideoEncoder::Hardware(hw) => {
            args.push("-vf".to_owned());
            args.push(hw_filters(hw, hw.decodes(source), video));
            args.extend(hw_encoder_args(hw, video));
            // Forced keyframes become IDR frames (`forced-idr` and friends above), and
            // the encoder adds none of its own: no scene cuts, a GOP never reached.
            args.extend(["-force_key_frames".to_owned(), force_key_frames()]);
            if video.tonemap {
                args.extend(
                    [
                        "-color_primaries",
                        "bt709",
                        "-color_trc",
                        "bt709",
                        "-colorspace",
                        "bt709",
                    ]
                    .map(str::to_owned),
                );
            }
        }
    }
    args.into_iter().map(OsString::from).collect()
}

/// Scaling and tone mapping for a hardware encoder. With GPU decoding the frames stay
/// in video memory, except for software tone mapping, which downloads them after
/// scaling (so the CPU only works on the output size).
fn hw_filters(hw: &HwEncoder, gpu_decode: bool, video: &VideoPlan) -> String {
    let (w, h) = (video.width, video.height);
    if !gpu_decode {
        // Decoded on the CPU: scale (and tone-map) there; the GPU only encodes.
        let mut filters = format!("scale={w}:{h},");
        filters.push_str(if video.tonemap {
            TONEMAP
        } else {
            "format=yuv420p"
        });
        match hw.accel {
            Accel::Nvenc => {}
            Accel::Qsv => filters.push_str(",format=nv12"),
            Accel::Vaapi => filters.push_str(",format=nv12,hwupload"),
        }
        return filters;
    }
    let cpu_tonemap = format!("hwdownload,format=p010le,format=yuv420p10le,{TONEMAP}");
    match (hw.accel, video.tonemap, hw.tonemap) {
        (Accel::Nvenc, false, _) => format!("scale_cuda={w}:{h}:format=nv12"),
        (Accel::Nvenc, true, Tonemap::Cuda) => format!(
            "scale_cuda={w}:{h}:format=p010le,\
             tonemap_cuda=format=nv12:p=bt709:t=bt709:m=bt709:tonemap=bt2390:peak=100:desat=0"
        ),
        (Accel::Nvenc, true, _) => format!("scale_cuda={w}:{h}:format=p010le,{cpu_tonemap}"),
        (Accel::Qsv, false, _) => format!("vpp_qsv=w={w}:h={h}:format=nv12"),
        (Accel::Qsv, true, Tonemap::VppQsv) => format!("vpp_qsv=w={w}:h={h}:format=nv12:tonemap=1"),
        (Accel::Qsv, true, _) => {
            format!("vpp_qsv=w={w}:h={h}:format=p010le,{cpu_tonemap},format=nv12")
        }
        (Accel::Vaapi, false, _) => format!("scale_vaapi=w={w}:h={h}:format=nv12"),
        (Accel::Vaapi, true, Tonemap::Vaapi) => format!(
            "scale_vaapi=w={w}:h={h}:format=p010,tonemap_vaapi=format=nv12:p=bt709:t=bt709:m=bt709"
        ),
        (Accel::Vaapi, true, _) => {
            format!("scale_vaapi=w={w}:h={h}:format=p010,{cpu_tonemap},format=nv12,hwupload")
        }
    }
}

/// Bitrate of a hardware transcode without a limit (they encode with a target
/// bitrate rather than a quality level).
fn default_bitrate(height: i64) -> i64 {
    match height {
        ..=480 => 2_000_000,
        481..=720 => 4_000_000,
        721..=1080 => 8_000_000,
        _ => 20_000_000,
    }
}

fn hw_encoder_args(hw: &HwEncoder, video: &VideoPlan) -> Vec<String> {
    let bitrate = video
        .bitrate
        .unwrap_or_else(|| default_bitrate(video.height));
    let rate = [
        "-b:v".to_owned(),
        bitrate.to_string(),
        "-maxrate".to_owned(),
        bitrate.to_string(),
        "-bufsize".to_owned(),
        (bitrate * 2).to_string(),
    ];
    let owned = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
    let mut args = match hw.accel {
        Accel::Nvenc => owned(&[
            "-c:v",
            "h264_nvenc",
            "-preset",
            "p4",
            "-profile:v",
            "high",
            "-rc",
            "vbr",
        ]),
        Accel::Qsv => owned(&[
            "-c:v",
            "h264_qsv",
            "-preset",
            "veryfast",
            "-profile:v",
            "high",
        ]),
        Accel::Vaapi => owned(&[
            "-c:v",
            "h264_vaapi",
            "-profile:v",
            "high",
            "-rc_mode",
            "VBR",
        ]),
    };
    args.extend(rate);
    args.extend(match hw.accel {
        Accel::Nvenc => owned(&["-g", HW_GOP, "-forced-idr", "1", "-no-scenecut", "1"]),
        // VA-API turns every forced keyframe into an IDR frame by itself and has no
        // scene-cut detection.
        Accel::Vaapi => owned(&["-g", HW_GOP]),
        Accel::Qsv => owned(&[
            "-g",
            QSV_GOP,
            "-forced_idr",
            "1",
            "-adaptive_i",
            "0",
            "-adaptive_b",
            "0",
        ]),
    });
    if hw.low_power {
        args.extend(owned(&["-low_power", "1"]));
    }
    args
}

/// The picture subtitle stream to draw into the video, if the plan burns one in.
/// Only transcodes can: copied video passes through untouched. Burn-in runs on the
/// software encoder (the overlay is a CPU filter).
pub fn burn_in_stream(plan: &PlaybackPlan) -> Option<i64> {
    let video = plan.video.as_ref()?;
    let subtitle = plan.subtitle?;
    (subtitle.delivery == SubtitleDelivery::BurnIn && video.action == StreamAction::Transcode)
        .then_some(subtitle.stream_index)
}

/// Filter graph for burn-in: the video through its usual filters (`video_filters`,
/// ending in an 8-bit SDR format), the subtitle canvas scaled to the same output size
/// (it covers the source picture) and laid over it. The output pad is `[v]`.
fn burn_in_graph(video: &VideoPlan, subtitle_index: i64, video_filters: &str) -> String {
    format!(
        "[0:{v}]{video_filters}[main];[0:{subtitle_index}]scale={w}:{h}[sub];\
         [main][sub]overlay=eof_action=pass,format=yuv420p[v]",
        v = video.stream_index,
        w = video.width,
        h = video.height,
    )
}

/// Software (libx264) transcode with a subtitle drawn in: scaled and, for HDR,
/// tone-mapped on the CPU, then overlaid; keyframes as for every transcode.
fn burn_in_args(video: &VideoPlan, subtitle_index: i64) -> Vec<OsString> {
    let mut filters = format!("scale={}:{}", video.width, video.height);
    if video.tonemap {
        filters.push(',');
        filters.push_str(TONEMAP);
    } else {
        filters.push_str(",format=yuv420p");
    }
    let mut args: Vec<String> = [
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "23",
        "-profile:v",
        "high",
        "-filter_complex",
        &burn_in_graph(video, subtitle_index, &filters),
        "-map",
        "[v]",
        // No keyframes of its own: they would add cuts.
        "-x264-params",
        "keyint=infinite:scenecut=0",
        "-force_key_frames",
        &force_key_frames(),
    ]
    .iter()
    .map(|a| (*a).to_owned())
    .collect();
    if let Some(bitrate) = video.bitrate {
        args.extend([
            "-maxrate".to_owned(),
            bitrate.to_string(),
            "-bufsize".to_owned(),
            (bitrate * 2).to_string(),
        ]);
    }
    args.into_iter().map(OsString::from).collect()
}

/// Segment numbers ffmpeg reports as complete in its own playlist.
pub fn completed_segments(ffmpeg_playlist: &str) -> impl Iterator<Item = u32> + '_ {
    ffmpeg_playlist.lines().filter_map(|line| {
        line.trim()
            .strip_prefix("seg")?
            .strip_suffix(".m4s")?
            .parse()
            .ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::{AudioPlan, PlayMethod, SubtitlePlan};
    use std::path::PathBuf;

    fn plan(tonemap: bool, audio: StreamAction) -> PlaybackPlan {
        PlaybackPlan {
            method: PlayMethod::Transcode,
            video: Some(VideoPlan {
                stream_index: 0,
                action: StreamAction::Transcode,
                codec: "h264".into(),
                width: 1280,
                height: 720,
                bitrate: Some(3_000_000),
                tonemap,
            }),
            audio: Some(AudioPlan {
                stream_index: 2,
                action: audio,
                codec: "aac".into(),
                channels: 6,
            }),
            subtitle: None,
            reasons: vec![],
        }
    }

    fn joined(args: &[OsString]) -> String {
        args.iter()
            .map(|a| a.to_string_lossy().replace('\\', "/"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn transcode_from_a_later_segment() {
        let plan = plan(false, StreamAction::Transcode);
        let segments = Segments::fixed(120_000);
        let args = hls_args(&HlsJob {
            input: Path::new("/media/Film (2020)/film.mkv"),
            out_dir: Path::new("/transcode/abc"),
            start_segment: 10,
            segments: &segments,
            plan: &plan,
            encoder: &VideoEncoder::Software,
            source: None,
        });
        let line = joined(&args);
        assert!(line.starts_with(
            "-hide_banner -loglevel error -nostdin -y -ss 60.000 -copyts -start_at_zero -i "
        ));
        // The path is one argument, spaces and all.
        assert!(args.iter().any(|a| a == "/media/Film (2020)/film.mkv"));
        assert!(line.contains("-map 0:0 -c:v libx264 -preset veryfast"));
        assert!(line.contains("-vf scale=1280:720,format=yuv420p"));
        assert!(line.contains("-x264-params keyint=infinite:scenecut=0 -force_key_frames expr:if(isnan(prev_forced_t),1,gt(floor(t/6),floor(prev_forced_t/6)))"));
        assert!(line.contains("-maxrate 3000000 -bufsize 6000000"));
        assert!(line.contains("-map 0:2 -c:a aac -ac 6 -b:a 384000"));
        assert!(line.contains(
            "-avoid_negative_ts disabled -f hls -hls_time 0.001 -hls_segment_options movflags=+frag_discont -hls_segment_type fmp4"
        ));
        assert!(line.contains("-start_number 10"));
        assert!(line.ends_with(
            "-hls_segment_filename /transcode/abc/seg%d.m4s /transcode/abc/ffmpeg.m3u8"
        ));
    }

    #[test]
    fn first_segment_has_no_seek_and_copies_audio() {
        let plan = plan(true, StreamAction::Copy);
        let line = joined(&hls_args(&HlsJob {
            input: Path::new("in.mkv"),
            out_dir: Path::new("out"),
            start_segment: 0,
            segments: &Segments::fixed(60_000),
            plan: &plan,
            encoder: &VideoEncoder::Software,
            source: None,
        }));
        assert!(!line.contains("-ss"));
        assert!(line.contains("-c:a copy"));
        assert!(
            line.contains("scale=1280:720,zscale=t=linear"),
            "scaled, then tone-mapped"
        );
    }

    #[test]
    fn copied_video_cuts_at_every_keyframe() {
        let mut plan = plan(false, StreamAction::Transcode);
        let video = plan.video.as_mut().unwrap();
        video.action = StreamAction::Copy;
        video.codec = "hevc".into();
        let segments = Segments::keyframes(&[40, 2_040, 4_540, 10_040], 12_000).unwrap();
        let line = joined(&hls_args(&HlsJob {
            input: Path::new("in.mkv"),
            out_dir: Path::new("out"),
            start_segment: 2,
            segments: &segments,
            plan: &plan,
            encoder: &VideoEncoder::Software,
            source: None,
        }));
        // 150 ms past the keyframe, so the seek cannot fall back to the previous one.
        assert!(line.contains("-ss 4.650 -copyts -start_at_zero -i in.mkv"));
        assert!(line.contains("-c:v copy -tag:v hvc1"));
        assert!(!line.contains("force_key_frames"));
        assert!(line.contains("-f hls -hls_time 0.001"));
        assert!(!line.contains("output_ts_offset"));
        assert!(line.contains("-start_number 2"));
    }

    #[test]
    fn picture_subtitles_are_burned_in_on_the_absolute_timeline() {
        let mut plan = plan(true, StreamAction::Copy);
        plan.subtitle = Some(SubtitlePlan {
            stream_index: 5,
            delivery: SubtitleDelivery::BurnIn,
        });
        let segments = Segments::fixed(120_000);
        let line = joined(&hls_args(&HlsJob {
            input: Path::new("in.mkv"),
            out_dir: Path::new("out"),
            start_segment: 3,
            segments: &segments,
            plan: &plan,
            encoder: &VideoEncoder::Software,
            source: None,
        }));
        // Same input, same seek and timeline as the video: in sync after restarts.
        assert!(line.contains("-ss 18.000 -copyts -start_at_zero -i in.mkv -c:v libx264"));
        assert!(line.contains(&format!(
            "-filter_complex [0:0]scale=1280:720,{TONEMAP}[main];[0:5]scale=1280:720[sub];\
             [main][sub]overlay=eof_action=pass,format=yuv420p[v] -map [v] -x264-params"
        )));
        assert!(!line.contains("-vf "));
        assert!(!line.contains("-map 0:0"));
        assert!(line.contains("-force_key_frames expr:"));
        assert!(line.contains("-map 0:2 -c:a copy"));
        assert!(line.contains("-hls_time 0.001 -hls_segment_options movflags=+frag_discont"));

        // With a GPU encoder configured, burn-in still runs on the CPU end to end.
        let gpu = hw(Accel::Nvenc, Tonemap::Cuda);
        let on_gpu = joined(&hls_args(&HlsJob {
            input: Path::new("in.mkv"),
            out_dir: Path::new("out"),
            start_segment: 3,
            segments: &segments,
            plan: &plan,
            encoder: &gpu,
            source: None,
        }));
        assert_eq!(on_gpu, line);

        // Text subtitles are the player's business; copied video is never touched.
        plan.subtitle = Some(SubtitlePlan {
            stream_index: 5,
            delivery: SubtitleDelivery::External,
        });
        let text = joined(&hls_args(&HlsJob {
            input: Path::new("in.mkv"),
            out_dir: Path::new("out"),
            start_segment: 0,
            segments: &segments,
            plan: &plan,
            encoder: &VideoEncoder::Software,
            source: None,
        }));
        assert!(!text.contains("-filter_complex"));
        assert!(text.contains("-map 0:0 -c:v libx264"));
        plan.subtitle = Some(SubtitlePlan {
            stream_index: 5,
            delivery: SubtitleDelivery::BurnIn,
        });
        plan.video.as_mut().unwrap().action = StreamAction::Copy;
        let copied = joined(&hls_args(&HlsJob {
            input: Path::new("in.mkv"),
            out_dir: Path::new("out"),
            start_segment: 0,
            segments: &segments,
            plan: &plan,
            encoder: &VideoEncoder::Software,
            source: None,
        }));
        assert!(copied.contains("-map 0:0 -c:v copy"));
        assert!(!copied.contains("overlay"));
    }

    #[test]
    fn fixed_playlist_covers_the_whole_duration() {
        let segments = Segments::fixed(14_500);
        assert_eq!(segments.count(), 3);
        let playlist = vod_playlist(&segments);
        assert!(playlist.starts_with("#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:6\n"));
        assert!(playlist.contains("#EXT-X-MAP:URI=\"init.mp4\"\n"));
        assert!(
            playlist
                .contains("#EXTINF:6.000,\n0.m4s\n#EXTINF:6.000,\n1.m4s\n#EXTINF:2.500,\n2.m4s\n")
        );
        assert!(playlist.ends_with("#EXT-X-ENDLIST\n"));
        assert_eq!(
            Segments::fixed(12_000).count(),
            2,
            "exact multiples have no empty tail"
        );
    }

    #[test]
    fn keyframe_playlist_has_one_segment_per_gop() {
        // The first keyframe is the file's start; the last GOP runs to the end.
        let segments = Segments::keyframes(&[40, 2_040, 4_540, 10_040], 12_000).unwrap();
        assert_eq!(segments.count(), 4);
        assert_eq!(
            (0..4).map(|n| segments.length_ms(n)).collect::<Vec<_>>(),
            [2_000, 2_500, 5_500, 2_000]
        );
        let playlist = vod_playlist(&segments);
        assert!(
            playlist.contains("#EXT-X-TARGETDURATION:6\n"),
            "longest GOP, rounded up"
        );
        assert!(playlist.contains("#EXTINF:5.500,\n2.m4s\n"));
        assert!(Segments::keyframes(&[], 1000).is_none());
    }

    #[test]
    fn completed_segments_are_read_from_ffmpegs_playlist() {
        let playlist = "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:6.0,\nseg10.m4s\n#EXTINF:6.0,\nseg11.m4s\n";
        assert_eq!(completed_segments(playlist).collect::<Vec<_>>(), [10, 11]);
    }

    fn hw(accel: Accel, tonemap: Tonemap) -> VideoEncoder {
        VideoEncoder::Hardware(HwEncoder {
            accel,
            device: PathBuf::from("/dev/dri/renderD128"),
            low_power: false,
            hw_decode: true,
            tonemap,
        })
    }

    fn hevc10() -> SourceVideo {
        SourceVideo {
            index: 0,
            codec: "hevc".into(),
            width: 3840,
            height: 2160,
            bit_depth: 10,
            hdr: "hdr10".into(),
        }
    }

    /// Arguments of a run from segment 10 with a hardware encoder.
    fn hw_line(encoder: &VideoEncoder, tonemap: bool, source: Option<&SourceVideo>) -> String {
        let plan = plan(tonemap, StreamAction::Copy);
        joined(&hls_args(&HlsJob {
            input: Path::new("in.mkv"),
            out_dir: Path::new("out"),
            start_segment: 10,
            segments: &Segments::fixed(120_000),
            plan: &plan,
            encoder,
            source,
        }))
    }

    const FORCED: &str =
        "-force_key_frames expr:if(isnan(prev_forced_t),1,gt(floor(t/6),floor(prev_forced_t/6)))";

    #[test]
    fn nvenc_runs_entirely_on_the_gpu() {
        let source = hevc10();
        let line = hw_line(&hw(Accel::Nvenc, Tonemap::Software), false, Some(&source));
        assert!(
            line.starts_with(
                "-hide_banner -loglevel error -nostdin -y -hwaccel cuda -hwaccel_output_format cuda \
                 -ss 60.000 -copyts -start_at_zero -i in.mkv -map 0:0 "
            ),
            "{line}"
        );
        assert!(
            line.contains(&format!(
                "-vf scale_cuda=1280:720:format=nv12 -c:v h264_nvenc -preset p4 -profile:v high -rc vbr \
                 -b:v 3000000 -maxrate 3000000 -bufsize 6000000 -g 100000 -forced-idr 1 -no-scenecut 1 \
                 {FORCED} -map 0:2 -c:a copy"
            )),
            "{line}"
        );
        assert!(!line.contains("libx264") && !line.contains("color_trc"));
    }

    #[test]
    fn nvenc_tone_maps_with_cuda_or_after_download() {
        let source = hevc10();
        let cuda = hw_line(&hw(Accel::Nvenc, Tonemap::Cuda), true, Some(&source));
        assert!(
            cuda.contains(
                "-vf scale_cuda=1280:720:format=p010le,tonemap_cuda=format=nv12:p=bt709:t=bt709:m=bt709:\
                 tonemap=bt2390:peak=100:desat=0 -c:v h264_nvenc"
            ),
            "{cuda}"
        );
        assert!(cuda.contains("-color_primaries bt709 -color_trc bt709 -colorspace bt709"));

        let hybrid = hw_line(&hw(Accel::Nvenc, Tonemap::Software), true, Some(&source));
        assert!(
            hybrid.contains(
                "-vf scale_cuda=1280:720:format=p010le,hwdownload,format=p010le,format=yuv420p10le,\
                 zscale=t=linear:npl=100,"
            ),
            "{hybrid}"
        );
        assert!(hybrid.contains("format=yuv420p -c:v h264_nvenc"));
    }

    #[test]
    fn cpu_decoding_feeds_the_gpu_encoder() {
        // MPEG-4 Part 2: no GPU decoder, so ffmpeg decodes and scales on the CPU.
        let source = SourceVideo {
            codec: "mpeg4".into(),
            bit_depth: 8,
            hdr: "sdr".into(),
            ..hevc10()
        };
        let nvenc = hw_line(&hw(Accel::Nvenc, Tonemap::Cuda), false, Some(&source));
        assert!(!nvenc.contains("-hwaccel"));
        assert!(
            nvenc.contains("-vf scale=1280:720,format=yuv420p -c:v h264_nvenc"),
            "{nvenc}"
        );

        // Unknown source: same thing. VA-API needs the frames uploaded.
        let vaapi = hw_line(&hw(Accel::Vaapi, Tonemap::Vaapi), true, None);
        assert!(vaapi.contains("-filter_hw_device va -ss 60.000"), "{vaapi}");
        assert!(!vaapi.contains("-hwaccel "));
        assert!(
            vaapi.contains("tonemap=tonemap=hable"),
            "software tone mapping"
        );
        assert!(
            vaapi.contains("format=yuv420p,format=nv12,hwupload -c:v h264_vaapi"),
            "{vaapi}"
        );

        let qsv = hw_line(&hw(Accel::Qsv, Tonemap::VppQsv), false, None);
        assert!(
            qsv.contains("-vf scale=1280:720,format=yuv420p,format=nv12 -c:v h264_qsv"),
            "{qsv}"
        );
    }

    #[test]
    fn qsv_pipeline() {
        let source = hevc10();
        let line = hw_line(&hw(Accel::Qsv, Tonemap::VppQsv), true, Some(&source));
        assert!(
            line.contains(
                "-hwaccel qsv -hwaccel_device qs -hwaccel_output_format qsv -c:v hevc_qsv -ss 60.000"
            ),
            "{line}"
        );
        assert!(
            line.contains(&format!(
                "-vf vpp_qsv=w=1280:h=720:format=nv12:tonemap=1 -c:v h264_qsv -preset veryfast \
                 -profile:v high -b:v 3000000 -maxrate 3000000 -bufsize 6000000 -g 65535 \
                 -forced_idr 1 -adaptive_i 0 -adaptive_b 0 {FORCED}"
            )),
            "{line}"
        );
        let sdr = hw_line(&hw(Accel::Qsv, Tonemap::VppQsv), false, Some(&source));
        assert!(
            sdr.contains("-vf vpp_qsv=w=1280:h=720:format=nv12 -c:v"),
            "{sdr}"
        );
        let hybrid = hw_line(&hw(Accel::Qsv, Tonemap::Software), true, Some(&source));
        assert!(
            hybrid.contains("-vf vpp_qsv=w=1280:h=720:format=p010le,hwdownload,format=p010le,")
        );
        assert!(hybrid.contains("format=yuv420p,format=nv12 -c:v h264_qsv"));

        let VideoEncoder::Hardware(mut low) = hw(Accel::Qsv, Tonemap::VppQsv) else {
            unreachable!()
        };
        low.low_power = true;
        let line = hw_line(&VideoEncoder::Hardware(low), false, Some(&source));
        assert!(
            line.contains("-adaptive_b 0 -low_power 1 -force_key_frames"),
            "{line}"
        );
    }

    #[test]
    fn qsv_devices_per_platform() {
        let source = hevc10();
        let encoder = hw(Accel::Qsv, Tonemap::VppQsv);
        assert_eq!(
            joined(&hw_input_args(&encoder, Some(&source), false)),
            "-init_hw_device vaapi=va:/dev/dri/renderD128 -init_hw_device qsv=qs@va \
             -filter_hw_device qs -hwaccel qsv -hwaccel_device qs -hwaccel_output_format qsv \
             -c:v hevc_qsv"
        );
        assert_eq!(
            joined(&hw_input_args(&encoder, None, true)),
            "-init_hw_device d3d11va=dx:,vendor_id=0x8086 -init_hw_device qsv=qs@dx \
             -filter_hw_device qs"
        );
    }

    #[test]
    fn vaapi_pipeline() {
        let source = hevc10();
        let encoder = hw(Accel::Vaapi, Tonemap::Vaapi);
        assert_eq!(
            joined(&hw_input_args(&encoder, Some(&source), false)),
            "-init_hw_device vaapi=va:/dev/dri/renderD128 -filter_hw_device va \
             -hwaccel vaapi -hwaccel_device va -hwaccel_output_format vaapi"
        );
        let line = hw_line(&encoder, true, Some(&source));
        assert!(
            line.contains(&format!(
                "-vf scale_vaapi=w=1280:h=720:format=p010,tonemap_vaapi=format=nv12:p=bt709:t=bt709:m=bt709 \
                 -c:v h264_vaapi -profile:v high -rc_mode VBR -b:v 3000000 -maxrate 3000000 \
                 -bufsize 6000000 -g 100000 {FORCED}"
            )),
            "{line}"
        );
        let sdr = hw_line(&encoder, false, Some(&source));
        assert!(sdr.contains("-vf scale_vaapi=w=1280:h=720:format=nv12 -c:v h264_vaapi"));
        let hybrid = hw_line(&hw(Accel::Vaapi, Tonemap::Software), true, Some(&source));
        assert!(hybrid.contains("-vf scale_vaapi=w=1280:h=720:format=p010,hwdownload,"));
        assert!(hybrid.contains("format=yuv420p,format=nv12,hwupload -c:v h264_vaapi"));
    }

    #[test]
    fn hardware_without_a_limit_uses_a_bitrate_for_the_size() {
        let mut plan = plan(false, StreamAction::Copy);
        plan.video.as_mut().unwrap().bitrate = None;
        let line = joined(&hls_args(&HlsJob {
            input: Path::new("in.mkv"),
            out_dir: Path::new("out"),
            start_segment: 0,
            segments: &Segments::fixed(60_000),
            plan: &plan,
            encoder: &hw(Accel::Nvenc, Tonemap::Software),
            source: None,
        }));
        assert!(
            line.contains("-b:v 4000000 -maxrate 4000000 -bufsize 8000000"),
            "{line}"
        );
        assert_eq!(default_bitrate(1080), 8_000_000);
        assert_eq!(default_bitrate(2160), 20_000_000);
    }

    #[test]
    fn copied_video_ignores_the_hardware_encoder() {
        let mut plan = plan(false, StreamAction::Copy);
        plan.video.as_mut().unwrap().action = StreamAction::Copy;
        let source = hevc10();
        let line = joined(&hls_args(&HlsJob {
            input: Path::new("in.mkv"),
            out_dir: Path::new("out"),
            start_segment: 0,
            segments: &Segments::fixed(60_000),
            plan: &plan,
            encoder: &hw(Accel::Qsv, Tonemap::VppQsv),
            source: Some(&source),
        }));
        assert!(
            line.starts_with("-hide_banner -loglevel error -nostdin -y -copyts"),
            "{line}"
        );
        assert!(!line.contains("qsv"));
    }

    #[test]
    fn test_runs_use_the_session_pipeline() {
        let video = plan(true, StreamAction::Copy).video.unwrap();
        let source = hevc10();
        let encoder = hw(Accel::Nvenc, Tonemap::Cuda);
        let picture = joined(&test_args(None, &video, &encoder, Some(&source)));
        assert!(
            picture.starts_with(
                "-hide_banner -loglevel error -nostdin -f lavfi -i testsrc2=s=640x360:r=25:d=0.4 \
                 -map 0:v:0 -vf scale=1280:720,"
            ),
            "the test picture is decoded on the CPU: {picture}"
        );
        assert!(picture.ends_with("-an -sn -f null -"));
        let clip = joined(&test_args(
            Some(Path::new("hdr.mkv")),
            &video,
            &encoder,
            Some(&source),
        ));
        assert!(
            clip.starts_with(
                "-hide_banner -loglevel error -nostdin -hwaccel cuda -hwaccel_output_format cuda \
                 -i hdr.mkv -map 0:v:0 -vf scale_cuda="
            ),
            "{clip}"
        );
        assert!(clip.contains("tonemap_cuda"));
    }
}
