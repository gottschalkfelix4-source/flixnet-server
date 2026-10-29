//! The decision engine: given what a file contains, what the client can play and what
//! the user asked for, decide between Direct Play, Remux (video copied into a new
//! container) and Transcode — with the reasons, so they can be shown in the UI.
//!
//! A pure function without I/O: everything it needs is in its arguments, which keeps
//! it testable as a table over media × device profiles.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// What a client can play directly. Web clients build it from `canPlayType` /
/// `MediaSource.isTypeSupported`; native apps send a fixed one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DeviceProfile {
    /// File containers the client plays as-is (file extensions: `mp4`, `webm`, `mkv`, …).
    #[serde(default)]
    pub containers: Vec<String>,
    #[serde(default)]
    pub video_codecs: Vec<VideoCodecSupport>,
    #[serde(default)]
    pub audio_codecs: Vec<AudioCodecSupport>,
    /// Upper bound for the total bitrate (bits/s), e.g. for slow connections.
    pub max_bitrate: Option<i64>,
    /// Whether the client can switch between audio tracks of one file. Browsers
    /// cannot, so another track than the default one needs a remux.
    #[serde(default)]
    pub multiple_audio_tracks: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct VideoCodecSupport {
    /// ffprobe codec name: `h264`, `hevc`, `av1`, `vp9`, …
    pub codec: String,
    pub max_width: Option<i64>,
    pub max_height: Option<i64>,
    /// Highest bit depth (8 when omitted).
    pub max_bit_depth: Option<i64>,
    /// HDR formats shown as HDR (`hdr10`, `hlg`, `dovi`); others are tone-mapped.
    #[serde(default)]
    pub hdr: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AudioCodecSupport {
    /// ffprobe codec name: `aac`, `ac3`, `eac3`, `opus`, `flac`, `mp3`, …
    pub codec: String,
    /// Most channels played without downmix (2 when omitted).
    pub max_channels: Option<i64>,
}

/// The parts of a probed file the decision depends on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceMedia {
    /// File extension, lower case (`mkv`, `mp4`, …).
    pub container: String,
    /// Total bitrate in bits/s.
    pub bitrate: Option<i64>,
    pub video: Option<SourceVideo>,
    pub audio: Vec<SourceAudio>,
    pub subtitles: Vec<SourceSubtitle>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceVideo {
    pub codec: String,
    pub width: i64,
    pub height: i64,
    pub bit_depth: i64,
    /// `sdr`, `hdr10`, `hlg` or `dovi`.
    pub hdr: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceAudio {
    /// Stream index within the file.
    pub index: i64,
    pub codec: String,
    pub channels: i64,
    pub is_default: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceSubtitle {
    pub index: i64,
    pub codec: String,
    pub is_external: bool,
}

/// What the user picked (or `None` for the file's defaults).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PlaybackChoice {
    pub audio_index: Option<i64>,
    pub subtitle_index: Option<i64>,
    /// Quality limit chosen in the player (bits/s).
    pub max_bitrate: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlayMethod {
    /// The original file, as-is (HTTP range requests).
    DirectPlay,
    /// Video copied into a streamable container; audio copied or converted.
    Remux,
    /// Video re-encoded.
    Transcode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StreamAction {
    Copy,
    Transcode,
}

/// Why the file cannot simply be played as-is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    ContainerNotSupported,
    VideoCodecNotSupported,
    VideoResolutionNotSupported,
    VideoBitDepthNotSupported,
    /// HDR the client cannot show; the transcode tone-maps to SDR.
    VideoRangeNotSupported,
    BitrateTooHigh,
    AudioCodecNotSupported,
    AudioChannelsNotSupported,
    /// Another than the default audio track was chosen, and the client cannot switch.
    AudioTrackNotDefault,
    /// Picture-based subtitles (PGS, VobSub, DVB) are burned into the video.
    SubtitleBurnIn,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct VideoPlan {
    pub action: StreamAction,
    /// Output codec (the source codec when copied).
    pub codec: String,
    pub width: i64,
    pub height: i64,
    /// Target bitrate of a transcode (bits/s).
    pub bitrate: Option<i64>,
    /// HDR → SDR tone mapping during the transcode.
    pub tonemap: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AudioPlan {
    pub stream_index: i64,
    pub action: StreamAction,
    pub codec: String,
    pub channels: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubtitleDelivery {
    /// Sent separately as WebVTT and rendered by the player.
    External,
    /// Drawn into the video (forces a transcode).
    BurnIn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SubtitlePlan {
    pub stream_index: i64,
    pub delivery: SubtitleDelivery,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PlaybackPlan {
    pub method: PlayMethod,
    pub video: Option<VideoPlan>,
    pub audio: Option<AudioPlan>,
    pub subtitle: Option<SubtitlePlan>,
    /// Empty for Direct Play.
    pub reasons: Vec<Reason>,
}

/// Codec every client can decode, used for transcodes.
const TRANSCODE_VIDEO: &str = "h264";
const TRANSCODE_AUDIO: &str = "aac";
/// Share of the bitrate budget left for video after audio and overhead.
const AUDIO_BUDGET: i64 = 256_000;

const IMAGE_SUBTITLES: &[&str] = &["hdmv_pgs_subtitle", "dvd_subtitle", "dvb_subtitle", "xsub"];

pub fn decide(
    source: &SourceMedia,
    profile: &DeviceProfile,
    choice: PlaybackChoice,
) -> PlaybackPlan {
    let mut reasons = Vec::new();
    let max_bitrate = match (profile.max_bitrate, choice.max_bitrate) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };

    let subtitle = choice
        .subtitle_index
        .and_then(|i| source.subtitles.iter().find(|s| s.index == i))
        .map(|s| {
            let burn = !s.is_external && IMAGE_SUBTITLES.contains(&s.codec.as_str());
            SubtitlePlan {
                stream_index: s.index,
                delivery: if burn {
                    SubtitleDelivery::BurnIn
                } else {
                    SubtitleDelivery::External
                },
            }
        });
    let burn_in = subtitle.is_some_and(|s| s.delivery == SubtitleDelivery::BurnIn);
    if burn_in {
        reasons.push(Reason::SubtitleBurnIn);
    }

    let over_bitrate = matches!((source.bitrate, max_bitrate), (Some(b), Some(max)) if b > max);
    if over_bitrate {
        reasons.push(Reason::BitrateTooHigh);
    }

    // Video: can it be copied?
    let mut video_copy = !burn_in && !over_bitrate;
    let mut tonemap = false;
    if let Some(video) = &source.video {
        match profile.video_codecs.iter().find(|c| c.codec == video.codec) {
            None => {
                reasons.push(Reason::VideoCodecNotSupported);
                video_copy = false;
            }
            Some(support) => {
                let too_wide = support.max_width.is_some_and(|w| video.width > w);
                let too_high = support.max_height.is_some_and(|h| video.height > h);
                if too_wide || too_high {
                    reasons.push(Reason::VideoResolutionNotSupported);
                    video_copy = false;
                }
                if video.bit_depth > support.max_bit_depth.unwrap_or(8) {
                    reasons.push(Reason::VideoBitDepthNotSupported);
                    video_copy = false;
                }
                if video.hdr != "sdr" && !support.hdr.contains(&video.hdr) {
                    reasons.push(Reason::VideoRangeNotSupported);
                    video_copy = false;
                }
            }
        }
        // A transcode of HDR material to H.264 is always tone-mapped.
        tonemap = !video_copy && video.hdr != "sdr";
    }

    // Audio: the chosen track, else the default one, else the first.
    let default_audio = source
        .audio
        .iter()
        .find(|a| a.is_default)
        .or_else(|| source.audio.first());
    let audio = choice
        .audio_index
        .and_then(|i| source.audio.iter().find(|a| a.index == i))
        .or(default_audio);
    let mut audio_copy = true;
    let mut audio_channels = 0;
    if let Some(track) = audio {
        audio_channels = track.channels;
        match profile.audio_codecs.iter().find(|c| c.codec == track.codec) {
            None => {
                reasons.push(Reason::AudioCodecNotSupported);
                audio_copy = false;
            }
            Some(support) if track.channels > support.max_channels.unwrap_or(2) => {
                reasons.push(Reason::AudioChannelsNotSupported);
                audio_copy = false;
            }
            Some(_) => {}
        }
    }
    let audio_switch = audio.is_some()
        && audio.map(|a| a.index) != default_audio.map(|a| a.index)
        && !profile.multiple_audio_tracks;
    if audio_switch {
        reasons.push(Reason::AudioTrackNotDefault);
    }

    let container_ok = profile.containers.contains(&source.container);
    if !container_ok {
        reasons.push(Reason::ContainerNotSupported);
    }

    let method = if !video_copy {
        PlayMethod::Transcode
    } else if !container_ok || !audio_copy || audio_switch {
        PlayMethod::Remux
    } else {
        PlayMethod::DirectPlay
    };
    if method == PlayMethod::DirectPlay {
        reasons.clear();
    }

    let video = source.video.as_ref().map(|v| {
        if video_copy {
            VideoPlan {
                action: StreamAction::Copy,
                codec: v.codec.clone(),
                width: v.width,
                height: v.height,
                bitrate: None,
                tonemap: false,
            }
        } else {
            let support = profile
                .video_codecs
                .iter()
                .find(|c| c.codec == TRANSCODE_VIDEO);
            let bitrate = max_bitrate.map(|max| (max - AUDIO_BUDGET).max(500_000));
            let (width, height) = transcode_size(v, support, bitrate);
            VideoPlan {
                action: StreamAction::Transcode,
                codec: TRANSCODE_VIDEO.to_owned(),
                width,
                height,
                bitrate,
                tonemap,
            }
        }
    });

    let audio = audio.map(|track| {
        if audio_copy {
            AudioPlan {
                stream_index: track.index,
                action: StreamAction::Copy,
                codec: track.codec.clone(),
                channels: track.channels,
            }
        } else {
            let max = profile
                .audio_codecs
                .iter()
                .find(|c| c.codec == TRANSCODE_AUDIO)
                .and_then(|c| c.max_channels)
                .unwrap_or(2);
            AudioPlan {
                stream_index: track.index,
                action: StreamAction::Transcode,
                codec: TRANSCODE_AUDIO.to_owned(),
                channels: audio_channels.clamp(1, max.min(6)),
            }
        }
    });

    PlaybackPlan {
        method,
        video,
        audio,
        subtitle,
        reasons,
    }
}

/// Output size of a video transcode: within the client's limits and a height that
/// suits the bitrate, keeping the aspect ratio (even dimensions for the encoder).
fn transcode_size(
    source: &SourceVideo,
    support: Option<&VideoCodecSupport>,
    bitrate: Option<i64>,
) -> (i64, i64) {
    let ladder_height = match bitrate {
        Some(b) if b < 1_500_000 => 480,
        Some(b) if b < 3_000_000 => 720,
        Some(b) if b < 12_000_000 => 1080,
        _ => 2160,
    };
    let max_height = support
        .and_then(|s| s.max_height)
        .unwrap_or(2160)
        .min(ladder_height);
    let max_width = support.and_then(|s| s.max_width).unwrap_or(3840);
    let (w, h) = (source.width.max(2), source.height.max(2));
    let scale = (max_height as f64 / h as f64)
        .min(max_width as f64 / w as f64)
        .min(1.0);
    let even = |v: f64| ((v / 2.0).round() as i64 * 2).max(2);
    (even(w as f64 * scale), even(h as f64 * scale))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(
        codec: &str,
        width: i64,
        height: i64,
        bit_depth: i64,
        hdr: &str,
    ) -> Option<SourceVideo> {
        Some(SourceVideo {
            codec: codec.into(),
            width,
            height,
            bit_depth,
            hdr: hdr.into(),
        })
    }

    fn audio(index: i64, codec: &str, channels: i64, is_default: bool) -> SourceAudio {
        SourceAudio {
            index,
            codec: codec.into(),
            channels,
            is_default,
        }
    }

    fn media(container: &str, video: Option<SourceVideo>, audio: Vec<SourceAudio>) -> SourceMedia {
        SourceMedia {
            container: container.into(),
            bitrate: Some(8_000_000),
            video,
            audio,
            subtitles: vec![
                SourceSubtitle {
                    index: 5,
                    codec: "subrip".into(),
                    is_external: false,
                },
                SourceSubtitle {
                    index: 6,
                    codec: "hdmv_pgs_subtitle".into(),
                    is_external: false,
                },
            ],
        }
    }

    fn vc(codec: &str, max_bit_depth: i64, hdr: &[&str]) -> VideoCodecSupport {
        VideoCodecSupport {
            codec: codec.into(),
            max_width: Some(3840),
            max_height: Some(2160),
            max_bit_depth: Some(max_bit_depth),
            hdr: hdr.iter().map(|h| (*h).to_owned()).collect(),
        }
    }

    fn ac(codec: &str, max_channels: i64) -> AudioCodecSupport {
        AudioCodecSupport {
            codec: codec.into(),
            max_channels: Some(max_channels),
        }
    }

    /// Chrome on an SDR screen: no HEVC, no AC-3, Matroska not listed.
    fn chrome() -> DeviceProfile {
        DeviceProfile {
            containers: vec!["mp4".into(), "m4v".into(), "webm".into()],
            video_codecs: vec![vc("h264", 8, &[]), vc("vp9", 10, &[]), vc("av1", 10, &[])],
            audio_codecs: vec![ac("aac", 6), ac("mp3", 2), ac("opus", 6), ac("flac", 6)],
            max_bitrate: None,
            multiple_audio_tracks: false,
        }
    }

    /// Safari on an HDR screen: HEVC incl. HDR10/Dolby Vision, AC-3/E-AC-3.
    fn safari_hdr() -> DeviceProfile {
        DeviceProfile {
            containers: vec!["mp4".into(), "m4v".into(), "mov".into()],
            video_codecs: vec![
                vc("h264", 8, &[]),
                vc("hevc", 10, &["hdr10", "hlg", "dovi"]),
            ],
            audio_codecs: vec![ac("aac", 6), ac("ac3", 6), ac("eac3", 6)],
            max_bitrate: None,
            multiple_audio_tracks: false,
        }
    }

    /// A native app that plays nearly everything (like Infuse or a TV app).
    fn native() -> DeviceProfile {
        DeviceProfile {
            containers: vec!["mkv".into(), "mp4".into(), "ts".into()],
            video_codecs: vec![vc("h264", 10, &[]), vc("hevc", 10, &["hdr10", "hlg"])],
            audio_codecs: vec![
                ac("aac", 8),
                ac("ac3", 6),
                ac("eac3", 8),
                ac("truehd", 8),
                ac("dts", 8),
            ],
            max_bitrate: None,
            multiple_audio_tracks: true,
        }
    }

    fn method_and_reasons(plan: &PlaybackPlan) -> (PlayMethod, Vec<Reason>) {
        (plan.method, plan.reasons.clone())
    }

    #[test]
    fn decision_table() {
        use PlayMethod::*;
        use Reason::*;
        let h264_mp4 = media(
            "mp4",
            video("h264", 1920, 1080, 8, "sdr"),
            vec![audio(1, "aac", 2, true)],
        );
        let h264_mkv_ac3 = media(
            "mkv",
            video("h264", 1920, 1080, 8, "sdr"),
            vec![audio(1, "ac3", 6, true), audio(2, "aac", 2, false)],
        );
        let hevc_hdr_mkv = media(
            "mkv",
            video("hevc", 3840, 2160, 10, "hdr10"),
            vec![audio(1, "eac3", 6, true)],
        );
        let vp9_webm = media(
            "webm",
            video("vp9", 1280, 720, 8, "sdr"),
            vec![audio(1, "opus", 2, true)],
        );
        let mpeg4_avi = media(
            "avi",
            video("mpeg4", 720, 404, 8, "sdr"),
            vec![audio(1, "mp3", 2, true)],
        );

        let none = PlaybackChoice::default();
        type Case<'a> = (
            &'a str,
            &'a SourceMedia,
            DeviceProfile,
            PlaybackChoice,
            PlayMethod,
            Vec<Reason>,
        );
        let cases: Vec<Case> = vec![
            (
                "h264/aac mp4 in Chrome",
                &h264_mp4,
                chrome(),
                none,
                DirectPlay,
                vec![],
            ),
            (
                "h264/aac mp4 in Safari",
                &h264_mp4,
                safari_hdr(),
                none,
                DirectPlay,
                vec![],
            ),
            (
                "vp9 webm in Chrome",
                &vp9_webm,
                chrome(),
                none,
                DirectPlay,
                vec![],
            ),
            (
                "vp9 webm in Safari",
                &vp9_webm,
                safari_hdr(),
                none,
                Transcode,
                vec![
                    VideoCodecNotSupported,
                    AudioCodecNotSupported,
                    ContainerNotSupported,
                ],
            ),
            (
                "mkv with AC-3 in Chrome",
                &h264_mkv_ac3,
                chrome(),
                none,
                Remux,
                vec![AudioCodecNotSupported, ContainerNotSupported],
            ),
            (
                "mkv with AC-3 natively",
                &h264_mkv_ac3,
                native(),
                none,
                DirectPlay,
                vec![],
            ),
            (
                "second audio track in Chrome",
                &h264_mkv_ac3,
                chrome(),
                PlaybackChoice {
                    audio_index: Some(2),
                    ..none
                },
                Remux,
                vec![AudioTrackNotDefault, ContainerNotSupported],
            ),
            (
                "second audio track natively",
                &h264_mkv_ac3,
                native(),
                PlaybackChoice {
                    audio_index: Some(2),
                    ..none
                },
                DirectPlay,
                vec![],
            ),
            (
                "4K HDR10 HEVC in Chrome",
                &hevc_hdr_mkv,
                chrome(),
                none,
                Transcode,
                vec![
                    VideoCodecNotSupported,
                    AudioCodecNotSupported,
                    ContainerNotSupported,
                ],
            ),
            (
                "4K HDR10 HEVC in Safari",
                &hevc_hdr_mkv,
                safari_hdr(),
                none,
                Remux,
                vec![ContainerNotSupported],
            ),
            (
                "4K HDR10 HEVC natively",
                &hevc_hdr_mkv,
                native(),
                none,
                DirectPlay,
                vec![],
            ),
            (
                "MPEG-4 Part 2 avi natively",
                &mpeg4_avi,
                native(),
                none,
                Transcode,
                vec![
                    VideoCodecNotSupported,
                    AudioCodecNotSupported,
                    ContainerNotSupported,
                ],
            ),
            (
                "text subtitles stay external",
                &h264_mp4,
                chrome(),
                PlaybackChoice {
                    subtitle_index: Some(5),
                    ..none
                },
                DirectPlay,
                vec![],
            ),
            (
                "PGS subtitles are burned in",
                &h264_mp4,
                chrome(),
                PlaybackChoice {
                    subtitle_index: Some(6),
                    ..none
                },
                Transcode,
                vec![SubtitleBurnIn],
            ),
            (
                "quality limit below the file's bitrate",
                &h264_mp4,
                chrome(),
                PlaybackChoice {
                    max_bitrate: Some(4_000_000),
                    ..none
                },
                Transcode,
                vec![BitrateTooHigh],
            ),
        ];
        for (name, source, profile, choice, method, reasons) in cases {
            let plan = decide(source, &profile, choice);
            assert_eq!(method_and_reasons(&plan), (method, reasons), "{name}");
        }
    }

    #[test]
    fn hdr_without_hdr_support_is_tonemapped() {
        let source = media(
            "mkv",
            video("hevc", 3840, 2160, 10, "hdr10"),
            vec![audio(1, "aac", 2, true)],
        );
        let mut sdr_hevc = native();
        sdr_hevc.video_codecs = vec![vc("h264", 8, &[]), vc("hevc", 10, &[])];
        let plan = decide(&source, &sdr_hevc, PlaybackChoice::default());
        assert_eq!(plan.method, PlayMethod::Transcode);
        assert_eq!(plan.reasons, [Reason::VideoRangeNotSupported]);
        let video = plan.video.unwrap();
        assert!(video.tonemap);
        assert_eq!(
            (video.codec.as_str(), video.width, video.height),
            ("h264", 3840, 2160)
        );
        // Audio is fine and copied.
        assert_eq!(plan.audio.unwrap().action, StreamAction::Copy);
    }

    #[test]
    fn transcodes_fit_bitrate_and_client_limits() {
        let source = media(
            "mkv",
            video("hevc", 3840, 1600, 10, "sdr"),
            vec![audio(1, "dts", 8, true)],
        );
        let plan = decide(
            &source,
            &chrome(),
            PlaybackChoice {
                max_bitrate: Some(2_000_000),
                ..Default::default()
            },
        );
        let video = plan.video.unwrap();
        // 2 Mbit/s → 720p ladder step, aspect ratio 2.4:1 kept, even sizes.
        assert_eq!((video.width, video.height), (1728, 720));
        assert_eq!(video.bitrate, Some(1_744_000));
        assert!(!video.tonemap);
        // DTS 7.1 → AAC 5.1 (Chrome's AAC limit is 6 channels).
        let audio = plan.audio.unwrap();
        assert_eq!(
            (audio.action, audio.codec.as_str(), audio.channels),
            (StreamAction::Transcode, "aac", 6)
        );
    }

    #[test]
    fn audio_only_and_no_audio_files() {
        let silent = media("mp4", video("h264", 640, 360, 8, "sdr"), vec![]);
        let plan = decide(&silent, &chrome(), PlaybackChoice::default());
        assert_eq!(plan.method, PlayMethod::DirectPlay);
        assert!(plan.audio.is_none());

        let music = media("flac", None, vec![audio(0, "flac", 2, true)]);
        let mut profile = chrome();
        profile.containers.push("flac".into());
        let plan = decide(&music, &profile, PlaybackChoice::default());
        assert_eq!(plan.method, PlayMethod::DirectPlay);
        assert!(plan.video.is_none());
    }

    #[test]
    fn unknown_choices_fall_back_to_defaults() {
        let source = media(
            "mp4",
            video("h264", 1920, 1080, 8, "sdr"),
            vec![audio(1, "aac", 2, false), audio(2, "aac", 2, true)],
        );
        let plan = decide(
            &source,
            &chrome(),
            PlaybackChoice {
                audio_index: Some(99),
                subtitle_index: Some(99),
                max_bitrate: None,
            },
        );
        assert_eq!(plan.method, PlayMethod::DirectPlay);
        assert_eq!(plan.audio.unwrap().stream_index, 2, "the default track");
        assert!(plan.subtitle.is_none());
    }
}
