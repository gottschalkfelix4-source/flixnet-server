//! HLS output: ffmpeg arguments for a segment run and the playlist the server hands out.
//!
//! The server writes the full VOD playlist itself (every segment exactly
//! [`SEGMENT_SECS`] long), so players can seek anywhere before anything is encoded.
//! ffmpeg is (re)started at any segment: `-ss` seeks the input, `-output_ts_offset`
//! puts the output back on the absolute timeline and forced keyframes every
//! [`SEGMENT_SECS`] make the cuts land exactly where the playlist says.
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

/// Number of segments for a duration.
pub fn segment_count(duration_ms: i64) -> u32 {
    let segment_ms = i64::from(SEGMENT_SECS) * 1000;
    u32::try_from((duration_ms.max(1) + segment_ms - 1) / segment_ms).unwrap_or(u32::MAX)
}

/// The complete playlist for a file of the given length. Segment and init URIs are
/// relative (`init.mp4`, `{n}.m4s`), so they inherit the playlist's location.
pub fn vod_playlist(duration_ms: i64) -> String {
    let count = segment_count(duration_ms);
    let mut out = String::with_capacity(64 + count as usize * 24);
    out.push_str("#EXTM3U\n#EXT-X-VERSION:7\n");
    let _ = writeln!(out, "#EXT-X-TARGETDURATION:{SEGMENT_SECS}");
    out.push_str(
        "#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-INDEPENDENT-SEGMENTS\n",
    );
    let _ = writeln!(out, "#EXT-X-MAP:URI=\"{INIT_FILE}\"");
    let segment_ms = i64::from(SEGMENT_SECS) * 1000;
    for n in 0..count {
        let remaining = duration_ms - i64::from(n) * segment_ms;
        let length = remaining.clamp(1, segment_ms);
        let _ = writeln!(out, "#EXTINF:{:.3},\n{n}.m4s", length as f64 / 1000.0);
    }
    out.push_str("#EXT-X-ENDLIST\n");
    out
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
    pub plan: &'a PlaybackPlan,
}

pub fn hls_args(job: &HlsJob<'_>) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |values: &[&str]| args.extend(values.iter().map(OsString::from));
    push(&["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]);

    let start_secs = job.start_segment * SEGMENT_SECS;
    if start_secs > 0 {
        push(&["-ss", &start_secs.to_string()]);
    }
    args.push("-i".into());
    args.push(job.input.as_os_str().to_owned());
    let mut push = |values: &[&str]| args.extend(values.iter().map(OsString::from));

    match &job.plan.video {
        Some(video) => {
            push(&["-map", &format!("0:{}", video.stream_index)]);
            match video.action {
                StreamAction::Copy => push(&["-c:v", "copy"]),
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
                        "-force_key_frames",
                        &format!("expr:gte(t,n_forced*{SEGMENT_SECS})"),
                        "-sc_threshold",
                        "0",
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
        "-output_ts_offset",
        &start_secs.to_string(),
        "-f",
        "hls",
        "-hls_time",
        &SEGMENT_SECS.to_string(),
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
        let args = hls_args(&HlsJob {
            input: Path::new("/media/Film (2020)/film.mkv"),
            out_dir: Path::new("/transcode/abc"),
            start_segment: 10,
            plan: &plan,
        });
        let line = joined(&args);
        assert!(line.starts_with("-hide_banner -loglevel error -nostdin -y -ss 60 -i "));
        // The path is one argument, spaces and all.
        assert!(args.iter().any(|a| a == "/media/Film (2020)/film.mkv"));
        assert!(line.contains("-map 0:0 -c:v libx264 -preset veryfast"));
        assert!(line.contains("-vf scale=1280:720,format=yuv420p"));
        assert!(line.contains("-force_key_frames expr:gte(t,n_forced*6)"));
        assert!(line.contains("-maxrate 3000000 -bufsize 6000000"));
        assert!(line.contains("-map 0:2 -c:a aac -ac 6 -b:a 384000"));
        assert!(line.contains("-output_ts_offset 60 -f hls -hls_time 6 -hls_segment_type fmp4"));
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
    fn playlist_covers_the_whole_duration() {
        let playlist = vod_playlist(14_500);
        assert_eq!(segment_count(14_500), 3);
        assert!(playlist.starts_with("#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:6\n"));
        assert!(playlist.contains("#EXT-X-MAP:URI=\"init.mp4\"\n"));
        assert!(
            playlist
                .contains("#EXTINF:6.000,\n0.m4s\n#EXTINF:6.000,\n1.m4s\n#EXTINF:2.500,\n2.m4s\n")
        );
        assert!(playlist.ends_with("#EXT-X-ENDLIST\n"));
        assert_eq!(
            segment_count(12_000),
            2,
            "exact multiples have no empty tail"
        );
    }

    #[test]
    fn completed_segments_are_read_from_ffmpegs_playlist() {
        let playlist = "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:6.0,\nseg10.m4s\n#EXTINF:6.0,\nseg11.m4s\n";
        assert_eq!(completed_segments(playlist).collect::<Vec<_>>(), [10, 11]);
    }
}
