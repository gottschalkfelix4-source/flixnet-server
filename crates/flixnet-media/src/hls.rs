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
//!
//! Every argument is built from typed values and paths chosen by the server — never
//! from request strings.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::Path;

use crate::decision::{PlaybackPlan, StreamAction};

/// Length of every segment except the last.
pub const SEGMENT_SECS: u32 = 6;

/// Init segment written by ffmpeg (fMP4 header, identical for every run).
pub const INIT_FILE: &str = "init.mp4";
/// Playlist ffmpeg keeps up to date; lists segments once they are complete.
pub const FFMPEG_PLAYLIST: &str = "ffmpeg.m3u8";

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
const TONEMAP: &str = "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,\
                       tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv,format=yuv420p";

/// One ffmpeg run producing segments from `start_segment` on.
#[derive(Debug, Clone, Copy)]
pub struct HlsJob<'a> {
    pub input: &'a Path,
    pub out_dir: &'a Path,
    pub start_segment: u32,
    pub segments: &'a Segments,
    pub plan: &'a PlaybackPlan,
}

pub fn hls_args(job: &HlsJob<'_>) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |values: &[&str]| args.extend(values.iter().map(OsString::from));
    push(&["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]);

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
            push(&["-map", &format!("0:{}", video.stream_index)]);
            match video.action {
                StreamAction::Copy => {
                    push(&["-c:v", "copy"]);
                    if video.codec == "hevc" {
                        // Apple players only accept HEVC tagged as hvc1.
                        push(&["-tag:v", "hvc1"]);
                    }
                }
                StreamAction::Transcode => {
                    let mut filters = format!("scale={}:{}", video.width, video.height);
                    if video.tonemap {
                        filters.push(',');
                        filters.push_str(TONEMAP);
                    } else {
                        filters.push_str(",format=yuv420p");
                    }
                    push(&[
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
                        &format!(
                            "expr:if(isnan(prev_forced_t),1,gt(floor(t/{SEGMENT_SECS}),floor(prev_forced_t/{SEGMENT_SECS})))"
                        ),
                    ]);
                    if let Some(bitrate) = video.bitrate {
                        push(&[
                            "-maxrate",
                            &bitrate.to_string(),
                            "-bufsize",
                            &(bitrate * 2).to_string(),
                        ]);
                    }
                }
            }
        }
        None => push(&["-vn"]),
    }

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
    use crate::decision::{AudioPlan, PlayMethod, VideoPlan};

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
}
