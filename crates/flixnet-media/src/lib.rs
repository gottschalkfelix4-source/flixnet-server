//! Media handling: ffprobe/ffmpeg are always run as separate processes (never linked),
//! so a crashing transcode cannot take the server down and the binary can be swapped
//! (jellyfin-ffmpeg in Docker). Arguments come only from typed values and database
//! paths — never from raw request strings.

pub mod decision;
pub mod hardware;
pub mod hls;
pub mod probe;
mod process;
pub mod segments;
pub mod subtitles;
pub mod transcode;
pub mod transcript;
pub mod trickplay;
pub mod whisper;

pub use probe::{Chapter, Ffprobe, MediaInfo, ProbeError, StreamInfo, StreamKind};
