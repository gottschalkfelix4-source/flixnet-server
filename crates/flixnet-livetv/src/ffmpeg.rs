//! ffmpeg/ffprobe arguments for live streams, built only from typed values: the plan
//! from the decision engine, paths chosen by the server and a stream URL that passed
//! [`crate::tuner::is_stream_url`] (network protocols only; `-protocol_whitelist`
//! keeps nested playlists from reaching local files either).
//!
//! The output is a rolling HLS playlist of MPEG-TS segments (`ffmpeg.m3u8`,
//! `seg{n}.ts`, numbered from `start_number` so restarts continue the sequence). The
//! stream manager indexes the segments and deletes them itself once they fall out of
//! the timeshift buffer. Copied streams keep the source's keyframes; transcodes
//! deinterlace (broadcast TV is mostly interlaced), keep the aspect ratio and force a
//! keyframe per segment — on the GPU encoder flixnet-media selected, if any.

use std::ffi::OsString;
use std::path::Path;

use flixnet_media::decision::{PlaybackPlan, StreamAction};
use flixnet_media::hardware::VideoEncoder;

use crate::tuner::StreamSource;

/// Playlist ffmpeg keeps up to date (read by the manager, never served).
pub const FFMPEG_PLAYLIST: &str = "ffmpeg.m3u8";

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

/// Completed segments of ffmpeg's playlist: `(number, duration in seconds)`.
pub fn playlist_entries(playlist: &str) -> Vec<(u64, f64)> {
    let mut duration = None;
    let mut out = Vec::new();
    for line in playlist.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            duration = rest
                .split(',')
                .next()
                .and_then(|d| d.trim().parse::<f64>().ok())
                .filter(|d| d.is_finite() && *d >= 0.0);
        } else if let Some(n) = segment_number(line) {
            out.push((n, duration.take().unwrap_or(0.0)));
        }
    }
    out
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
    /// Segments listed in ffmpeg's playlist (the manager keeps more on disk).
    pub list_size: u32,
    /// Number of the first segment of this run.
    pub start_number: u64,
    /// Encoder for a video transcode (unused when the video is copied).
    pub encoder: &'a VideoEncoder,
}

fn strings(values: &[&str]) -> impl Iterator<Item = OsString> {
    values
        .iter()
        .map(OsString::from)
        .collect::<Vec<_>>()
        .into_iter()
}

pub fn live_hls_args(job: &LiveJob<'_>) -> Vec<OsString> {
    let segment = job.segment_secs.max(1).to_string();
    let transcode_video = job
        .plan
        .video
        .as_ref()
        .filter(|v| v.action == StreamAction::Transcode);
    // Deinterlace (broadcast TV), keep the aspect ratio, never upscale.
    let filters = transcode_video.map(|video| {
        let height = if video.height > 0 { video.height } else { 1080 };
        format!(
            "yadif=mode=send_frame:parity=auto:deint=interlaced,\
             scale=-2:min(ih\\,{height}),format=yuv420p"
        )
    });
    let hardware = match (transcode_video, job.encoder, &filters) {
        (Some(video), VideoEncoder::Hardware(hw), Some(filters)) => {
            Some(flixnet_media::hls::hw_live_video_args(hw, video, filters))
        }
        _ => None,
    };

    let mut args: Vec<OsString> = Vec::new();
    args.extend(strings(&[
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-y",
    ]));
    if let Some(hw) = &hardware {
        args.extend(hw.input.iter().cloned());
    }
    args.extend(input_args(job.source));
    args.extend(strings(&[
        "-fflags",
        "+genpts+discardcorrupt",
        "-analyzeduration",
        "3000000",
        "-probesize",
        "5000000",
        "-i",
    ]));
    args.push(OsString::from(&job.source.url));

    match &job.plan.video {
        Some(video) => {
            args.extend(strings(&["-map", "0:v:0?"]));
            match video.action {
                StreamAction::Copy => args.extend(strings(&["-c:v", "copy"])),
                StreamAction::Transcode => {
                    let keyframes = format!("expr:gte(t,n_forced*{segment})");
                    match &hardware {
                        Some(hw) => {
                            args.extend(hw.output.iter().cloned());
                            args.extend(strings(&["-force_key_frames", &keyframes]));
                        }
                        None => {
                            let filters = filters.as_deref().unwrap_or("format=yuv420p");
                            args.extend(strings(&[
                                "-c:v",
                                "libx264",
                                "-preset",
                                "veryfast",
                                "-crf",
                                "23",
                                "-profile:v",
                                "high",
                                "-vf",
                                filters,
                                "-sc_threshold",
                                "0",
                                "-force_key_frames",
                                &keyframes,
                            ]));
                            if let Some(bitrate) = video.bitrate {
                                args.extend(strings(&[
                                    "-maxrate",
                                    &bitrate.to_string(),
                                    "-bufsize",
                                    &(bitrate * 2).to_string(),
                                ]));
                            }
                        }
                    }
                }
            }
        }
        None => args.extend(strings(&["-vn"])),
    }
    match &job.plan.audio {
        Some(audio) => {
            args.extend(strings(&["-map", "0:a:0?"]));
            match audio.action {
                StreamAction::Copy => args.extend(strings(&["-c:a", "copy"])),
                StreamAction::Transcode => {
                    let channels = audio.channels.clamp(1, 2);
                    args.extend(strings(&[
                        "-c:a",
                        "aac",
                        "-ac",
                        &channels.to_string(),
                        "-b:a",
                        "192000",
                    ]));
                }
            }
        }
        None => args.extend(strings(&["-an"])),
    }
    args.extend(strings(&[
        "-sn",
        "-dn",
        "-max_muxing_queue_size",
        "4096",
        "-f",
        "hls",
        "-hls_time",
        &segment,
        "-hls_list_size",
        &job.list_size.max(2).to_string(),
        "-start_number",
        &job.start_number.to_string(),
        // The manager deletes segments once they leave the timeshift buffer.
        "-hls_flags",
        "independent_segments+temp_file+omit_endlist",
        "-hls_segment_type",
        "mpegts",
        "-hls_allow_cache",
        "0",
        "-hls_segment_filename",
    ]));
    args.push(job.out_dir.join("seg%d.ts").into_os_string());
    args.push(job.out_dir.join(FFMPEG_PLAYLIST).into_os_string());
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use flixnet_media::decision::{AudioPlan, PlayMethod, VideoPlan};
    use flixnet_media::hardware::{Accel, HwEncoder, Tonemap};

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

    fn job<'a>(
        src: &'a StreamSource,
        plan: &'a PlaybackPlan,
        encoder: &'a VideoEncoder,
        dir: &'a Path,
    ) -> LiveJob<'a> {
        LiveJob {
            source: src,
            out_dir: dir,
            plan,
            segment_secs: 4,
            list_size: 30,
            start_number: 0,
            encoder,
        }
    }

    #[test]
    fn copy_keeps_the_streams() {
        let src = source("http://iptv.example/live/1.ts");
        let plan = plan(StreamAction::Copy, StreamAction::Copy);
        let encoder = VideoEncoder::Software;
        let mut job = job(&src, &plan, &encoder, Path::new("/transcode/live/abc"));
        job.start_number = 451;
        let args = joined(&live_hls_args(&job));
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
        assert!(
            args.contains("-hls_time 4 -hls_list_size 30 -start_number 451"),
            "{args}"
        );
        assert!(!args.contains("delete_segments"), "the manager deletes");
        assert!(
            args.ends_with("/transcode/live/abc/seg%d.ts /transcode/live/abc/ffmpeg.m3u8"),
            "{args}"
        );
        assert!(!args.contains("libx264"));
    }

    #[test]
    fn transcode_deinterlaces_and_forces_keyframes() {
        let src = source("udp://@239.0.0.1:1234");
        let plan = plan(StreamAction::Transcode, StreamAction::Transcode);
        let encoder = VideoEncoder::Software;
        let args = joined(&live_hls_args(&job(&src, &plan, &encoder, Path::new("/t"))));
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
    fn transcodes_use_the_selected_gpu_encoder() {
        let src = source("http://iptv.example/live/3.ts");
        let plan = plan(StreamAction::Transcode, StreamAction::Copy);
        let hw = |accel| {
            VideoEncoder::Hardware(HwEncoder {
                accel,
                device: "/dev/dri/renderD128".into(),
                low_power: false,
                hw_decode: true,
                tonemap: Tonemap::Software,
            })
        };
        let vaapi = hw(Accel::Vaapi);
        let args = joined(&live_hls_args(&job(&src, &plan, &vaapi, Path::new("/t"))));
        assert!(
            args.starts_with(
                "-hide_banner -loglevel error -nostdin -y -init_hw_device vaapi=va:/dev/dri/renderD128 -filter_hw_device va -protocol_whitelist"
            ),
            "{args}"
        );
        assert!(
            !args.contains("-hwaccel vaapi"),
            "live video is decoded on the CPU: {args}"
        );
        assert!(
            args.contains("format=yuv420p,format=nv12,hwupload -c:v h264_vaapi"),
            "{args}"
        );
        assert!(
            args.contains("-force_key_frames expr:gte(t,n_forced*4)"),
            "{args}"
        );
        assert!(!args.contains("libx264"), "{args}");

        let nvenc = hw(Accel::Nvenc);
        let args = joined(&live_hls_args(&job(&src, &plan, &nvenc, Path::new("/t"))));
        assert!(args.contains("-c:v h264_nvenc"), "{args}");
        assert!(!args.contains("-hwaccel cuda"), "{args}");

        // Copied video never touches the GPU.
        let copy = self::plan(StreamAction::Copy, StreamAction::Copy);
        let args = joined(&live_hls_args(&job(&src, &copy, &vaapi, Path::new("/t"))));
        assert!(!args.contains("init_hw_device"), "{args}");
    }

    #[test]
    fn segment_names_and_playlist_entries() {
        assert_eq!(segment_number("seg12.ts"), Some(12));
        assert_eq!(segment_number("seg.ts"), None);
        assert_eq!(segment_number("../seg1.ts"), None);
        assert_eq!(segment_number("seg1.ts.tmp"), None);
        let playlist = "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:7\n#EXTINF:4.004000,\nseg7.ts\n#EXTINF:3.98,\nseg8.ts\n";
        assert_eq!(
            playlist_segments(playlist).collect::<Vec<_>>(),
            ["seg7.ts", "seg8.ts"]
        );
        assert_eq!(playlist_entries(playlist), [(7, 4.004), (8, 3.98)]);
    }
}
