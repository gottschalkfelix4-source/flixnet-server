//! Live TV: tuner sources (M3U, Xtream Codes, HDHomeRun) behind one [`tuner::Tuner`]
//! trait, the XMLTV guide (streaming parser, channel matching, refresh) and live
//! streams — one ffmpeg per watched channel producing a rolling HLS window that all
//! viewers of that channel share.
//!
//! Imports and guide refreshes run as jobs ([`jobs`]); the API crate drives
//! [`stream::LiveTvManager`] for playback.

pub mod ffmpeg;
pub mod guide;
pub mod hdhomerun;
pub mod http;
pub mod import;
pub mod jobs;
pub mod m3u;
pub mod matching;
mod process;
pub mod stream;
pub mod tuner;
pub mod xmltv;
pub mod xtream;

pub use stream::{LiveError, LiveSettings, LiveTvManager};
pub use tuner::{StreamSource, TunerKind};
