//! Media handling: ffprobe/ffmpeg are always run as separate processes (never linked),
//! so a crashing transcode cannot take the server down and the binary can be swapped
//! (jellyfin-ffmpeg in Docker). Arguments come only from typed values and database
//! paths — never from raw request strings.

pub mod probe;
mod process;

pub use probe::{Chapter, Ffprobe, MediaInfo, ProbeError, StreamInfo, StreamKind};
