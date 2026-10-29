//! HDHomeRun network tuners.
//!
//! * Discovery: a UDP broadcast to port 65001 in SiliconDust's packet format
//!   (`type | length | tags | CRC-32`); devices answer with their id, tuner count and
//!   base URL. Devices that do not answer (other subnet, Docker bridge) are added by
//!   address instead.
//! * `discover.json` describes a device, `lineup.json` lists its channels with their
//!   stream URLs (`http://<ip>:5004/auto/v<number>`).

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::net::UdpSocket;

use crate::http::{self, FetchError};

/// UDP port HDHomeRun devices listen on for discovery.
pub const DISCOVERY_PORT: u16 = 65001;

const TYPE_DISCOVER_REQ: u16 = 0x0002;
const TYPE_DISCOVER_RPY: u16 = 0x0003;
const TAG_DEVICE_TYPE: u8 = 0x01;
const TAG_DEVICE_ID: u8 = 0x02;
const TAG_TUNER_COUNT: u8 = 0x10;
const TAG_LINEUP_URL: u8 = 0x27;
const TAG_BASE_URL: u8 = 0x2A;
const DEVICE_TYPE_TUNER: u32 = 0x0000_0001;
const WILDCARD: u32 = 0xFFFF_FFFF;

/// `discover.json`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct DeviceInfo {
    #[serde(default)]
    pub friendly_name: Option<String>,
    #[serde(default)]
    pub model_number: Option<String>,
    #[serde(rename = "DeviceID", default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub tuner_count: Option<i64>,
    #[serde(rename = "BaseURL", default)]
    pub base_url: Option<String>,
    #[serde(rename = "LineupURL", default)]
    pub lineup_url: Option<String>,
}

/// One `lineup.json` entry.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LineupEntry {
    #[serde(rename = "GuideNumber")]
    pub guide_number: String,
    #[serde(rename = "GuideName", default)]
    pub guide_name: Option<String>,
    #[serde(rename = "URL")]
    pub url: String,
    #[serde(rename = "HD", default)]
    pub hd: Option<i64>,
    #[serde(rename = "VideoCodec", default)]
    pub video_codec: Option<String>,
    #[serde(rename = "AudioCodec", default)]
    pub audio_codec: Option<String>,
    #[serde(rename = "DRM", default)]
    pub drm: Option<i64>,
}

/// ffprobe codec name for a lineup's `VideoCodec`.
pub fn video_codec(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_uppercase().as_str() {
        "MPEG2" => "mpeg2video",
        "H264" | "AVC" => "h264",
        "HEVC" | "H265" => "hevc",
        _ => return None,
    })
}

/// ffprobe codec name for a lineup's `AudioCodec`.
pub fn audio_codec(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_uppercase().as_str() {
        "AC3" => "ac3",
        "EAC3" => "eac3",
        "AAC" => "aac",
        "MPEG" | "MP2" => "mp2",
        "MP3" => "mp3",
        _ => return None,
    })
}

/// `192.168.1.50`, `192.168.1.50:8080` or a full URL → the device's base URL.
pub fn base_url(address: &str) -> String {
    let trimmed = address.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix("/discover.json").unwrap_or(trimmed);
    if http::is_http(trimmed) {
        trimmed.to_owned()
    } else {
        format!("http://{trimmed}")
    }
}

pub async fn device_info(
    client: &reqwest::Client,
    address: &str,
) -> Result<DeviceInfo, FetchError> {
    http::fetch_json(client, &format!("{}/discover.json", base_url(address))).await
}

/// The device's channels (`LineupURL` from `discover.json`, else `/lineup.json`).
pub async fn lineup(
    client: &reqwest::Client,
    address: &str,
    info: &DeviceInfo,
) -> Result<Vec<LineupEntry>, FetchError> {
    let url = info
        .lineup_url
        .clone()
        .unwrap_or_else(|| format!("{}/lineup.json", base_url(address)));
    let entries: Vec<LineupEntry> = http::fetch_json(client, &url).await?;
    // DRM-protected channels cannot be played outside the vendor's apps.
    Ok(entries
        .into_iter()
        .filter(|e| e.drm.unwrap_or(0) == 0)
        .collect())
}

// ---- UDP discovery -----------------------------------------------------------------

/// A device that answered the discovery broadcast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    pub device_id: String,
    pub base_url: String,
    pub tuner_count: Option<i64>,
    pub lineup_url: Option<String>,
    pub address: IpAddr,
}

/// IEEE CRC-32 (as Ethernet and zlib), which the protocol appends little-endian.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn push_tag(out: &mut Vec<u8>, tag: u8, value: &[u8]) {
    out.push(tag);
    let len = value.len();
    if len <= 127 {
        out.push(len as u8);
    } else {
        out.push((len as u8 & 0x7F) | 0x80);
        out.push((len >> 7) as u8);
    }
    out.extend_from_slice(value);
}

fn packet(kind: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&kind.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    out.extend_from_slice(payload);
    let crc = crc32(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    out
}

/// The discovery request: "every tuner, any device id".
pub fn discover_request() -> Vec<u8> {
    let mut payload = Vec::new();
    push_tag(
        &mut payload,
        TAG_DEVICE_TYPE,
        &DEVICE_TYPE_TUNER.to_be_bytes(),
    );
    push_tag(&mut payload, TAG_DEVICE_ID, &WILDCARD.to_be_bytes());
    packet(TYPE_DISCOVER_REQ, &payload)
}

/// Tags of a packet of the given type with a valid CRC.
fn tags(data: &[u8], kind: u16) -> Option<Vec<(u8, &[u8])>> {
    if data.len() < 8 {
        return None;
    }
    let (body, crc) = data.split_at(data.len() - 4);
    if crc32(body).to_le_bytes() != crc {
        return None;
    }
    if u16::from_be_bytes([body[0], body[1]]) != kind {
        return None;
    }
    let len = usize::from(u16::from_be_bytes([body[2], body[3]]));
    let payload = body.get(4..4 + len)?;
    let mut out = Vec::new();
    let mut i = 0;
    while i < payload.len() {
        let tag = payload[i];
        let mut len = usize::from(*payload.get(i + 1)?);
        i += 2;
        if len & 0x80 != 0 {
            len = (len & 0x7F) | (usize::from(*payload.get(i)?) << 7);
            i += 1;
        }
        out.push((tag, payload.get(i..i + len)?));
        i += len;
    }
    Some(out)
}

/// Builds a discovery reply (used by tests and fake devices).
pub fn discover_reply(device_id: u32, tuner_count: u8, base_url: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    push_tag(
        &mut payload,
        TAG_DEVICE_TYPE,
        &DEVICE_TYPE_TUNER.to_be_bytes(),
    );
    push_tag(&mut payload, TAG_DEVICE_ID, &device_id.to_be_bytes());
    push_tag(&mut payload, TAG_TUNER_COUNT, &[tuner_count]);
    push_tag(&mut payload, TAG_BASE_URL, base_url.as_bytes());
    push_tag(
        &mut payload,
        TAG_LINEUP_URL,
        format!("{base_url}/lineup.json").as_bytes(),
    );
    packet(TYPE_DISCOVER_RPY, &payload)
}

/// Whether a packet is a discovery request (fake devices).
pub fn is_discover_request(data: &[u8]) -> bool {
    tags(data, TYPE_DISCOVER_REQ).is_some()
}

pub fn parse_reply(data: &[u8], from: IpAddr) -> Option<Discovered> {
    let tags = tags(data, TYPE_DISCOVER_RPY)?;
    let mut device_type = None;
    let mut device_id = None;
    let mut tuner_count = None;
    let mut base = None;
    let mut lineup_url = None;
    for (tag, value) in tags {
        match tag {
            TAG_DEVICE_TYPE if value.len() == 4 => {
                device_type = Some(u32::from_be_bytes(value.try_into().ok()?));
            }
            TAG_DEVICE_ID if value.len() == 4 => {
                device_id = Some(u32::from_be_bytes(value.try_into().ok()?));
            }
            TAG_TUNER_COUNT if value.len() == 1 => tuner_count = Some(i64::from(value[0])),
            TAG_BASE_URL => base = Some(String::from_utf8_lossy(value).into_owned()),
            TAG_LINEUP_URL => lineup_url = Some(String::from_utf8_lossy(value).into_owned()),
            _ => {}
        }
    }
    if device_type.is_some_and(|t| t != DEVICE_TYPE_TUNER) {
        return None; // a storage device (DVR), not a tuner
    }
    Some(Discovered {
        device_id: format!("{:08X}", device_id?),
        // Old firmware sends no base URL; its web server is on port 80.
        base_url: base.unwrap_or_else(|| format!("http://{from}")),
        tuner_count,
        lineup_url,
        address: from,
    })
}

/// Broadcasts a discovery request to `targets` (normally
/// `255.255.255.255:65001`) and collects the answers for `wait`.
pub async fn discover(targets: &[SocketAddr], wait: Duration) -> std::io::Result<Vec<Discovered>> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).await?;
    socket.set_broadcast(true)?;
    let request = discover_request();
    for target in targets {
        if let Err(err) = socket.send_to(&request, target).await {
            tracing::debug!(%target, %err, "HDHomeRun discovery send failed");
        }
    }
    let deadline = Instant::now() + wait;
    let mut found: Vec<Discovered> = Vec::new();
    let mut buf = [0u8; 2048];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left, socket.recv_from(&mut buf)).await {
            Ok(Ok((len, from))) => {
                if let Some(device) = parse_reply(&buf[..len], from.ip())
                    && !found.iter().any(|d| d.device_id == device.device_id)
                {
                    found.push(device);
                }
            }
            // Windows reports ICMP "port unreachable" from an earlier send as an error.
            Ok(Err(err)) => tracing::debug!(%err, "HDHomeRun discovery receive failed"),
            Err(_) => break,
        }
    }
    Ok(found)
}

/// The default discovery target (limited broadcast).
pub fn broadcast_target() -> SocketAddr {
    SocketAddr::from(([255, 255, 255, 255], DISCOVERY_PORT))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_reference() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn request_and_reply_round_trip() {
        let request = discover_request();
        assert_eq!(&request[..4], &[0x00, 0x02, 0x00, 0x0C]);
        assert!(is_discover_request(&request));

        let reply = discover_reply(0x1013_ABCD, 2, "http://192.168.1.50:80");
        let device = parse_reply(&reply, "192.168.1.50".parse().unwrap()).unwrap();
        assert_eq!(device.device_id, "1013ABCD");
        assert_eq!(device.tuner_count, Some(2));
        assert_eq!(device.base_url, "http://192.168.1.50:80");
        assert_eq!(
            device.lineup_url.as_deref(),
            Some("http://192.168.1.50:80/lineup.json")
        );

        let mut broken = reply.clone();
        broken[10] ^= 0xFF;
        assert!(parse_reply(&broken, "192.168.1.50".parse().unwrap()).is_none());
        assert!(parse_reply(&request, "192.168.1.50".parse().unwrap()).is_none());
    }

    #[test]
    fn long_tag_values() {
        let long = format!("http://{}", "a".repeat(200));
        let reply = discover_reply(1, 1, &long);
        let device = parse_reply(&reply, "10.0.0.1".parse().unwrap()).unwrap();
        assert_eq!(device.base_url, long);
    }

    #[test]
    fn addresses_become_base_urls() {
        assert_eq!(base_url("192.168.1.50"), "http://192.168.1.50");
        assert_eq!(base_url("http://10.0.0.2:8080/"), "http://10.0.0.2:8080");
        assert_eq!(base_url("http://10.0.0.2/discover.json"), "http://10.0.0.2");
        assert_eq!(video_codec("MPEG2"), Some("mpeg2video"));
        assert_eq!(audio_codec("AC3"), Some("ac3"));
    }

    #[test]
    fn lineup_json() {
        let entries: Vec<LineupEntry> = serde_json::from_str(
            r#"[{"GuideNumber":"1","GuideName":"Das Erste HD","VideoCodec":"H264","AudioCodec":"AAC","HD":1,"URL":"http://192.168.1.50:5004/auto/v1"},
                {"GuideNumber":"2.1","GuideName":"ZDF","URL":"http://192.168.1.50:5004/auto/v2.1","DRM":1}]"#,
        )
        .unwrap();
        assert_eq!(entries[0].guide_number, "1");
        assert_eq!(entries[0].hd, Some(1));
        assert_eq!(entries[1].drm, Some(1));
    }
}
