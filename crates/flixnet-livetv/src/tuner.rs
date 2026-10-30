//! The tuner abstraction: every source of live channels (M3U playlist, Xtream Codes
//! account, HDHomeRun device, tvheadend server) answers the same two questions — what
//! is it (device id, how many streams at once) and which channels does it offer.
//! Playing a channel needs only its stream URL, which the lineup provides.

use std::collections::HashMap;
use std::future::Future;

use flixnet_db::livetv::{NewChannel, TunerHost};

use crate::http::{self, FetchError};
use crate::tvheadend::{Tvheadend, TvheadendError};
use crate::xtream::{self, XtreamError};
use crate::{hdhomerun, m3u};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunerKind {
    M3u,
    Xtream,
    HdHomeRun,
    Tvheadend,
}

impl TunerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TunerKind::M3u => "m3u",
            TunerKind::Xtream => "xtream",
            TunerKind::HdHomeRun => "hdhomerun",
            TunerKind::Tvheadend => "tvheadend",
        }
    }

    pub fn parse(kind: &str) -> Option<Self> {
        Some(match kind {
            "m3u" => TunerKind::M3u,
            "xtream" => TunerKind::Xtream,
            "hdhomerun" => TunerKind::HdHomeRun,
            "tvheadend" => TunerKind::Tvheadend,
            _ => return None,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TunerError {
    #[error("Quelle nicht erreichbar: {0}")]
    Fetch(#[from] FetchError),
    #[error("Xtream: {0}")]
    Xtream(#[from] XtreamError),
    #[error("{0}")]
    Tvheadend(#[from] TvheadendError),
    #[error("Zugangsdaten fehlen")]
    MissingCredentials,
    #[error("unbekannter Tuner-Typ {0}")]
    UnknownKind(String),
}

/// What a tuner says about itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TunerInfo {
    pub device_id: Option<String>,
    /// Streams that may run at once (`None` = no limit known).
    pub tuner_count: Option<i64>,
}

/// Channels plus device details, read in one go.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Lineup {
    pub info: TunerInfo,
    pub channels: Vec<NewChannel>,
}

/// A source of live channels.
pub trait Tuner: Send + Sync {
    fn kind(&self) -> TunerKind;

    /// Reads the device/account and its channel lineup.
    fn lineup(
        &self,
        client: &reqwest::Client,
    ) -> impl Future<Output = Result<Lineup, TunerError>> + Send;
}

/// Where a channel's stream comes from (what ffmpeg reads).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamSource {
    pub url: String,
    pub user_agent: Option<String>,
}

/// Stream URLs ffmpeg may open: network protocols only (no `file:`, `concat:` and
/// the like, which would let a playlist read local files).
pub fn is_stream_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    [
        "http://", "https://", "udp://", "rtp://", "rtsp://", "rtmp://",
    ]
    .iter()
    .any(|scheme| lower.starts_with(scheme))
        && !url.chars().any(char::is_control)
}

/// Numeric order of a channel number (`"2.1"` → 2.1); unnumbered channels sort after
/// all numbered ones, in source order.
fn sort_order(number: Option<&str>, position: usize) -> f64 {
    number
        .and_then(|n| n.trim().parse::<f64>().ok())
        .filter(|n| n.is_finite())
        .unwrap_or(1_000_000.0 + position as f64)
}

/// Gives repeated ids a stable suffix in source order (`id`, `id#2`, …).
fn unique_ids(ids: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    ids.map(|id| {
        let count = seen.entry(id.clone()).or_insert(0);
        *count += 1;
        if *count == 1 {
            id
        } else {
            format!("{id}#{count}")
        }
    })
    .collect()
}

fn looks_hd(name: &str) -> bool {
    name.split(|c: char| !c.is_alphanumeric())
        .any(|w| matches!(w.to_ascii_lowercase().as_str(), "hd" | "fhd" | "uhd" | "4k"))
}

// ---- M3U ---------------------------------------------------------------------------

pub struct M3uTuner {
    /// Playlist URL or file path.
    pub location: String,
    pub user_agent: Option<String>,
}

/// Channels of a parsed playlist (entries with unusable URLs are left out).
pub fn m3u_channels(playlist: &m3u::M3uPlaylist) -> Vec<NewChannel> {
    let entries: Vec<_> = playlist
        .entries
        .iter()
        .filter(|e| is_stream_url(&e.url))
        .collect();
    let ids = unique_ids(entries.iter().map(|e| {
        e.tvg_id
            .clone()
            .unwrap_or_else(|| format!("name:{}", e.name.to_lowercase()))
    }));
    entries
        .into_iter()
        .zip(ids)
        .enumerate()
        .map(|(position, (e, remote_id))| NewChannel {
            remote_id,
            number: e.number.clone(),
            sort_order: sort_order(e.number.as_deref(), position),
            name: e.name.clone(),
            logo_url: e.tvg_logo.clone(),
            group_title: e.group.clone(),
            stream_url: e.url.clone(),
            tvg_id: e.tvg_id.clone(),
            tvg_name: e.tvg_name.clone(),
            is_hd: looks_hd(&e.name),
            video_codec: None,
            audio_codec: None,
        })
        .collect()
}

impl Tuner for M3uTuner {
    fn kind(&self) -> TunerKind {
        TunerKind::M3u
    }

    async fn lineup(&self, client: &reqwest::Client) -> Result<Lineup, TunerError> {
        let text = http::fetch_text(client, &self.location, self.user_agent.as_deref()).await?;
        Ok(Lineup {
            info: TunerInfo::default(),
            channels: m3u_channels(&m3u::parse(&text)),
        })
    }
}

// ---- Xtream Codes ------------------------------------------------------------------

pub struct XtreamTuner {
    pub server: String,
    pub username: String,
    pub password: String,
}

impl Tuner for XtreamTuner {
    fn kind(&self) -> TunerKind {
        TunerKind::Xtream
    }

    async fn lineup(&self, client: &reqwest::Client) -> Result<Lineup, TunerError> {
        let account = xtream::account(client, &self.server, &self.username, &self.password).await?;
        let streams =
            xtream::live_streams(client, &self.server, &self.username, &self.password).await?;
        let mut channels = Vec::with_capacity(streams.len());
        for (position, s) in streams.iter().enumerate() {
            channels.push(NewChannel {
                remote_id: s.stream_id.clone(),
                number: s.num.clone(),
                sort_order: sort_order(s.num.as_deref(), position),
                name: s.name.clone(),
                logo_url: s.icon.clone(),
                group_title: s.category.clone(),
                stream_url: xtream::stream_url(
                    &self.server,
                    &self.username,
                    &self.password,
                    &s.stream_id,
                )?,
                tvg_id: s.epg_channel_id.clone(),
                tvg_name: None,
                is_hd: looks_hd(&s.name),
                video_codec: None,
                audio_codec: None,
            });
        }
        Ok(Lineup {
            info: TunerInfo {
                device_id: None,
                tuner_count: account.max_connections,
            },
            channels,
        })
    }
}

// ---- HDHomeRun ---------------------------------------------------------------------

pub struct HdHomeRunTuner {
    /// IP address or URL of the device.
    pub address: String,
}

/// Channels of a lineup (codecs as the device announces them).
pub fn hdhomerun_channels(entries: &[hdhomerun::LineupEntry]) -> Vec<NewChannel> {
    entries
        .iter()
        .filter(|e| is_stream_url(&e.url))
        .enumerate()
        .map(|(position, e)| NewChannel {
            remote_id: e.guide_number.clone(),
            number: Some(e.guide_number.clone()),
            sort_order: sort_order(Some(&e.guide_number), position),
            name: e
                .guide_name
                .clone()
                .unwrap_or_else(|| format!("Kanal {}", e.guide_number)),
            logo_url: None,
            group_title: None,
            stream_url: e.url.clone(),
            tvg_id: None,
            tvg_name: None,
            is_hd: e.hd.unwrap_or(0) != 0,
            video_codec: e
                .video_codec
                .as_deref()
                .and_then(hdhomerun::video_codec)
                .map(str::to_owned),
            audio_codec: e
                .audio_codec
                .as_deref()
                .and_then(hdhomerun::audio_codec)
                .map(str::to_owned),
        })
        .collect()
}

impl Tuner for HdHomeRunTuner {
    fn kind(&self) -> TunerKind {
        TunerKind::HdHomeRun
    }

    async fn lineup(&self, client: &reqwest::Client) -> Result<Lineup, TunerError> {
        let info = hdhomerun::device_info(client, &self.address).await?;
        let entries = hdhomerun::lineup(client, &self.address, &info).await?;
        Ok(Lineup {
            info: TunerInfo {
                device_id: info.device_id.clone(),
                tuner_count: info.tuner_count.filter(|n| *n > 0),
            },
            channels: hdhomerun_channels(&entries),
        })
    }
}

// ---- Any tuner ---------------------------------------------------------------------

/// The tuner a stored host describes.
pub enum AnyTuner {
    M3u(M3uTuner),
    Xtream(XtreamTuner),
    HdHomeRun(HdHomeRunTuner),
    Tvheadend(Tvheadend),
}

impl AnyTuner {
    pub fn from_host(host: &TunerHost) -> Result<Self, TunerError> {
        match TunerKind::parse(&host.kind) {
            Some(TunerKind::M3u) => Ok(AnyTuner::M3u(M3uTuner {
                location: host.url.clone(),
                user_agent: host.user_agent.clone(),
            })),
            Some(TunerKind::Xtream) => Ok(AnyTuner::Xtream(XtreamTuner {
                server: host.url.clone(),
                username: host
                    .username
                    .clone()
                    .ok_or(TunerError::MissingCredentials)?,
                password: host
                    .password
                    .clone()
                    .ok_or(TunerError::MissingCredentials)?,
            })),
            Some(TunerKind::HdHomeRun) => Ok(AnyTuner::HdHomeRun(HdHomeRunTuner {
                address: host.url.clone(),
            })),
            Some(TunerKind::Tvheadend) => Ok(AnyTuner::Tvheadend(Tvheadend {
                url: host.url.clone(),
                username: host.username.clone().filter(|u| !u.is_empty()),
                password: host.password.clone(),
            })),
            None => Err(TunerError::UnknownKind(host.kind.clone())),
        }
    }
}

impl Tuner for AnyTuner {
    fn kind(&self) -> TunerKind {
        match self {
            AnyTuner::M3u(t) => t.kind(),
            AnyTuner::Xtream(t) => t.kind(),
            AnyTuner::HdHomeRun(t) => t.kind(),
            AnyTuner::Tvheadend(t) => t.kind(),
        }
    }

    async fn lineup(&self, client: &reqwest::Client) -> Result<Lineup, TunerError> {
        match self {
            AnyTuner::M3u(t) => t.lineup(client).await,
            AnyTuner::Xtream(t) => t.lineup(client).await,
            AnyTuner::HdHomeRun(t) => t.lineup(client).await,
            AnyTuner::Tvheadend(t) => t.lineup(client).await,
        }
    }
}

/// Where to read a channel of a host from.
pub fn stream_source(host: &TunerHost, stream_url: &str) -> StreamSource {
    StreamSource {
        url: stream_url.to_owned(),
        user_agent: host.user_agent.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_channels_get_stable_unique_ids() {
        let playlist = m3u::parse(
            "#EXTM3U
#EXTINF:-1 tvg-id=\"daserste.de\" tvg-chno=\"1\",Das Erste HD
http://a/1.ts
#EXTINF:-1 tvg-id=\"daserste.de\",Das Erste SD
http://a/2.ts
#EXTINF:-1,ZDF
http://a/3.ts
#EXTINF:-1,Lokale Datei
file:///etc/passwd
",
        );
        let channels = m3u_channels(&playlist);
        let ids: Vec<_> = channels.iter().map(|c| c.remote_id.as_str()).collect();
        assert_eq!(ids, ["daserste.de", "daserste.de#2", "name:zdf"]);
        assert_eq!(channels[0].sort_order, 1.0);
        assert!(channels[1].sort_order > 1000.0, "unnumbered after numbered");
        assert!(channels[1].sort_order < channels[2].sort_order);
        assert!(channels[0].is_hd);
        assert!(!channels[1].is_hd);
    }

    #[test]
    fn only_network_stream_urls() {
        assert!(is_stream_url("http://x/1.ts"));
        assert!(is_stream_url("udp://@239.0.0.1:1234"));
        assert!(!is_stream_url("file:///etc/passwd"));
        assert!(!is_stream_url("concat:http://a|http://b"));
        assert!(!is_stream_url("/media/film.mkv"));
        assert!(!is_stream_url("http://x/\n-i"));
    }
}
