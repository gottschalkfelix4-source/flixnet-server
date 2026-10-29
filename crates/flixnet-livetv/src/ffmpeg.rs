//! ffmpeg/ffprobe arguments for live streams, built only from typed values: the plan
//! from the decision engine, paths chosen by the server and a stream URL that passed
//! [`crate::tuner::is_stream_url`] (network protocols only; `-protocol_whitelist`
//! keeps nested playlists from reaching local files either).
//!
//! The output is a rolling HLS window of MPEG-TS segments (`live.m3u8`,
//! `seg{n}.ts`): copied streams keep the source's keyframes; transcodes deinterlace
//! (broadcast TV is mostly interlaced), keep the aspect ratio and force a keyframe
//! per segment.

use std::ffi::OsString;
use std::path::Path;

use flixnet_media::decision::{PlaybackPlan, StreamAction};

use crate::tuner::StreamSource;

/// Playlist ffmpeg keeps up to date.
pub const LIVE_PLAYLIST: &str = "live.m3u8";

/// Protocols ffmpeg may use while reading a live stream.
const PROTOCOLS: &str = "http,https,tcp,tls,crypto,udp,rtp,rtsp,rtmp,httpproxy";

/// `seg12.ts` → 12 (and only names ffmpeg writes).
pub fn segment_number(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("seg")?.strip_suffix(".ts")?;
    if digits.is_empty() || digits.len() > 12 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Segment names a live playlist lists.
pub fn playlist_segments(playlist: &str) -> impl Iterator<Item = &str> {
    playlist
        .lines()
        .map(str::trim)
        .filter(|line| segment_number(line).is_some())
}

/// Input options shared by probe and stream: identification, timeouts, reconnects.
fn input_args(source: &StreamSource) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |values: &[&str]| args.extend(values.iter().map(OsString::from));
    push(&["-protocol_whitelist", PROTOCOLS]);
    let lower = source.url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        if let Some(agent) = &source.user_agent {
            push(&["-user_agent", agent]);
        }
        // Stalls fail after 15 s; dropped connections are reopened.
        push(&[
            "-rw_timeout",
            "15000000",
            "-reconnect",
            "1",
            "-reconnect_streamed",
            "1",
            "-reconnect_delay_max",
            "4",
        ]);
    }
    args
}

/// ffprobe of a live stream: the first video and audio stream, quickly.
pub fn probe_args(source: &StreamSource) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-v",
        "error",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.extend(input_args(source));
    args.extend(
        ["-analyzeduration", "3000000", "-probesize", "3000000", "-i"]
            .into_iter()
            .map(OsString::from),
    );
    args.push(OsString::from(&source.url));
    args
}

/// One live ffmpeg run.
#[derive(Debug, Clone, Copy)]
pub struct LiveJob<'a> {
    pub source: &'a StreamSource,
    pub out_dir: &'a Path,
    pub plan: &'a PlaybackPlan,
    pub segment_secs: u32,
    /// Segments listed in the playlist.
    pub window: u32,
}

pub fn live_hls_args(job: &LiveJob<'_>) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |values: &[&str]| args.extend(values.iter().map(OsString::from));
    push(&["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]);
    args.extend(input_args(job.source));
    let mut push = |values: &[&str]| args.extend(values.iter().map(OsString::from));
    push(&[
        "-fflags",
        "+genpts+discardcorrupt",
        "-analyzeduration",
        "3000000",
        "-probesize",
        "5000000",
        "-i",
    ]);
    args.push(OsString::from(&job.source.url));
    let mut push = |values: &[&str]| args.extend(values.iter().map(OsString::from));

    let segment = job.segment_secs.max(1).to_string();
    match &job.plan.video {
        Some(video) => {
            push(&["-map", "0:v:0?"]);
            match video.action {
                StreamAction::Copy => push(&["-c:v", "copy"]),
                StreamAction::Transcode => {
                    let height = if video.height > 0 { video.height } else { 1080 };
                    let filters = format!(
                        "yadif=mode=send_frame:parity=auto:deint=interlaced,\
                         scale=-2:min(ih\\,{height}),format=yuv420p"
                    );
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
                        "-sc_threshold",
                        "0",
                        "-force_key_frames",
                        &format!("expr:gte(t,n_forced*{segment})"),
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
            push(&["-map", "0:a:0?"]);
            match audio.action {
                StreamAction::Copy => push(&["-c:a", "copy"]),
                StreamAction::Transcode => {
                    let channels = audio.channels.clamp(1, 2);
                    push(&[
                        "-c:a",
                        "aac",
                        "-ac",
                        &channels.to_string(),
                        "-b:a",
                        "192000",
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
        "4096",
        "-f",
        "hls",
        "-hls_time",
        &segment,
        "-hls_list_size",
        &job.window.max(2).to_string(),
        // Old segments go once they are well out of the window (a slow player may
        // still be fetching one).
        "-hls_delete_threshold",
        "4",
        "-hls_flags",
        "delete_segments+independent_segments+temp_file+omit_endlist",
        "-hls_segment_type",
        "mpegts",
        "-hls_allow_cache",
        "0",
        "-hls_segment_filename",
    ]);
    args.push(job.out_dir.join("seg%d.ts").into_os_string());
    args.push(job.out_dir.join(LIVE_PLAYLIST).into_os_string());
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use flixnet_media::decision::{AudioPlan, PlayMethod, VideoPlan};

    fn plan(video: StreamAction, audio: StreamAction) -> PlaybackPlan {
        PlaybackPlan {
            method: PlayMethod::Transcode,
            video: Some(VideoPlan {
                stream_index: 0,
                action: video,
                codec: "h264".into(),
                width: 1280,
                height: 720,
                bitrate: None,
                tonemap: false,
            }),
            audio: Some(AudioPlan {
                stream_index: 1,
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

    fn source(url: &str) -> StreamSource {
        StreamSource {
            url: url.into(),
            user_agent: Some("Kodi/21".into()),
        }
    }

    #[test]
    fn copy_keeps_the_streams() {
        let src = source("http://iptv.example/live/1.ts");
        let plan = plan(StreamAction::Copy, StreamAction::Copy);
        let args = joined(&live_hls_args(&LiveJob {
            source: &src,
            out_dir: Path::new("/transcode/live/abc"),
            plan: &plan,
            segment_secs: 4,
            window: 6,
        }));
        assert!(
            args.contains("-user_agent Kodi/21 -rw_timeout 15000000"),
            "{args}"
        );
        assert!(
            args.contains(
                "-i http://iptv.example/live/1.ts -map 0:v:0? -c:v copy -map 0:a:0? -c:a copy"
            ),
            "{args}"
        );
        assert!(args.contains("-hls_time 4 -hls_list_size 6"), "{args}");
        assert!(
            args.ends_with("/transcode/live/abc/seg%d.ts /transcode/live/abc/live.m3u8"),
            "{args}"
        );
        assert!(!args.contains("libx264"));
    }

    #[test]
    fn transcode_deinterlaces_and_forces_keyframes() {
        let src = source("udp://@239.0.0.1:1234");
        let plan = plan(StreamAction::Transcode, StreamAction::Transcode);
        let args = joined(&live_hls_args(&LiveJob {
            source: &src,
            out_dir: Path::new("/t"),
            plan: &plan,
            segment_secs: 4,
            window: 6,
        }));
        assert!(
            !args.contains("-user_agent"),
            "HTTP options only for HTTP: {args}"
        );
        assert!(args.contains("-c:v libx264"), "{args}");
        assert!(
            args.contains(
                "yadif=mode=send_frame:parity=auto:deint=interlaced,scale=-2:min(ih/,720)"
            ),
            "{args}"
        );
        assert!(
            args.contains("-force_key_frames expr:gte(t,n_forced*4)"),
            "{args}"
        );
        assert!(args.contains("-c:a aac -ac 2"), "{args}");
    }

    #[test]
    fn segment_names() {
        assert_eq!(segment_number("seg12.ts"), Some(12));
        assert_eq!(segment_number("seg.ts"), None);
        assert_eq!(segment_number("../seg1.ts"), None);
        assert_eq!(segment_number("seg1.ts.tmp"), None);
        let playlist = "#EXTM3U\n#EXTINF:4.0,\nseg7.ts\n#EXTINF:4.0,\nseg8.ts\n";
        assert_eq!(
            playlist_segments(playlist).collect::<Vec<_>>(),
            ["seg7.ts", "seg8.ts"]
        );
    }
}
