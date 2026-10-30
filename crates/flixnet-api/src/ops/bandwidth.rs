//! Bytes served by the media routes (direct streams, HLS transcodes, live TV), per
//! stream and in total, with the rate over the last few seconds.
//!
//! A middleware wraps the response bodies of those routes and counts every frame as
//! it is sent, so the numbers reflect what actually left the server. Traffic is split
//! into local (home network) and remote clients and kept as a history: per second for
//! the last two minutes, per minute for the last day.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use http_body_util::BodyExt;

use crate::AppState;

/// Seconds the current rate is averaged over.
const WINDOW_SECS: u64 = 10;
/// Streams without traffic for this long are forgotten.
const IDLE: Duration = Duration::from_secs(3600);
/// Per-second history kept for the real-time chart.
const SECONDS_KEPT: u64 = 120;
/// Per-minute history kept for the hour and day charts.
const MINUTES_KEPT: u64 = 24 * 60;

/// Bytes of one time slot, local and remote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Split {
    local: u64,
    remote: u64,
}

/// A history of fixed-size slots (`(slot number, bytes)`), oldest first.
#[derive(Default)]
struct Slots {
    slots: VecDeque<(u64, Split)>,
}

impl Slots {
    fn add(&mut self, slot: u64, bytes: u64, remote: bool, keep: u64) {
        match self.slots.back_mut() {
            Some((s, _)) if *s == slot => {}
            _ => self.slots.push_back((slot, Split::default())),
        }
        let split = &mut self.slots.back_mut().expect("just pushed").1;
        if remote {
            split.remote += bytes;
        } else {
            split.local += bytes;
        }
        while self.slots.front().is_some_and(|(s, _)| *s + keep <= slot) {
            self.slots.pop_front();
        }
    }

    fn get(&self, slot: u64) -> Split {
        self.slots
            .iter()
            .find(|(s, _)| *s == slot)
            .map_or_else(Split::default, |(_, v)| *v)
    }
}

#[derive(Default)]
struct Window {
    /// (second since start, bytes in that second)
    buckets: VecDeque<(u64, u64)>,
}

impl Window {
    fn add(&mut self, second: u64, bytes: u64) {
        match self.buckets.back_mut() {
            Some((s, b)) if *s == second => *b += bytes,
            _ => self.buckets.push_back((second, bytes)),
        }
        self.trim(second);
    }

    fn trim(&mut self, now: u64) {
        while self
            .buckets
            .front()
            .is_some_and(|(s, _)| *s + WINDOW_SECS <= now)
        {
            self.buckets.pop_front();
        }
    }

    fn rate(&self, now: u64) -> u64 {
        let sum: u64 = self
            .buckets
            .iter()
            .filter(|(s, _)| *s + WINDOW_SECS > now)
            .map(|(_, b)| b)
            .sum();
        sum / WINDOW_SECS
    }
}

struct Stream {
    bytes: u64,
    window: Window,
    last: Instant,
}

struct Meter {
    start: Instant,
    /// Wall clock at `start` (unix ms), to date the history.
    start_unix_ms: i64,
    streams: HashMap<String, Stream>,
    total: u64,
    window: Window,
    seconds: Slots,
    minutes: Slots,
}

/// Counts bytes per stream key (`file:<id>`, `transcode:<session>`, `live:<session>`).
#[derive(Clone)]
pub struct BandwidthMeter {
    inner: Arc<Mutex<Meter>>,
}

impl Default for BandwidthMeter {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Meter {
                start: Instant::now(),
                start_unix_ms: flixnet_db::now_ms(),
                streams: HashMap::new(),
                total: 0,
                window: Window::default(),
                seconds: Slots::default(),
                minutes: Slots::default(),
            })),
        }
    }
}

/// Totals of one stream or of everything.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub bytes: u64,
    pub bytes_per_second: u64,
}

impl BandwidthMeter {
    /// Counts `bytes` sent for stream `key` to a local or a `remote` client.
    pub fn record(&self, key: &str, bytes: u64, remote: bool) {
        if bytes == 0 {
            return;
        }
        let mut meter = self.inner.lock().expect("bandwidth lock");
        let now = Instant::now();
        let second = now.duration_since(meter.start).as_secs();
        meter.total += bytes;
        meter.window.add(second, bytes);
        meter.seconds.add(second, bytes, remote, SECONDS_KEPT);
        meter.minutes.add(second / 60, bytes, remote, MINUTES_KEPT);
        if !meter.streams.contains_key(key) {
            meter
                .streams
                .retain(|_, s| now.duration_since(s.last) < IDLE);
        }
        let stream = meter.streams.entry(key.to_owned()).or_insert(Stream {
            bytes: 0,
            window: Window::default(),
            last: now,
        });
        stream.bytes += bytes;
        stream.window.add(second, bytes);
        stream.last = now;
    }

    fn second(meter: &Meter) -> u64 {
        Instant::now().duration_since(meter.start).as_secs()
    }

    /// One stream (zero if it never sent anything).
    pub fn stream(&self, key: &str) -> Usage {
        let meter = self.inner.lock().expect("bandwidth lock");
        let now = Self::second(&meter);
        meter
            .streams
            .get(key)
            .map_or_else(Usage::default, |s| Usage {
                bytes: s.bytes,
                bytes_per_second: s.window.rate(now),
            })
    }

    /// Everything since the server started.
    pub fn total(&self) -> Usage {
        let meter = self.inner.lock().expect("bandwidth lock");
        let now = Self::second(&meter);
        Usage {
            bytes: meter.total,
            bytes_per_second: meter.window.rate(now),
        }
    }
}

/// Time range of a bandwidth chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    /// Last two minutes, per second.
    RealTime,
    /// Last hour, per minute.
    Hour,
    /// Last 24 hours, per 15 minutes.
    Day,
}

/// One point of a bandwidth chart: average rate over the slot ending at `at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    /// End of the slot (unix ms).
    pub at: i64,
    pub local_bytes_per_second: u64,
    pub remote_bytes_per_second: u64,
}

impl BandwidthMeter {
    /// The chart for `range`, oldest first, empty slots included. Returns the slot
    /// length in seconds and the points.
    pub fn history(&self, range: Range) -> (u64, Vec<Point>) {
        let meter = self.inner.lock().expect("bandwidth lock");
        let now = Self::second(&meter);
        let (slot_secs, count): (u64, u64) = match range {
            Range::RealTime => (1, SECONDS_KEPT),
            Range::Hour => (60, 60),
            Range::Day => (15 * 60, 96),
        };
        let current = now / slot_secs;
        let points = (0..count)
            .rev()
            .filter_map(|back| current.checked_sub(back))
            .map(|slot| {
                let split = match range {
                    Range::RealTime => meter.seconds.get(slot),
                    Range::Hour => meter.minutes.get(slot),
                    Range::Day => (slot * 15..slot * 15 + 15).fold(Split::default(), |acc, m| {
                        let v = meter.minutes.get(m);
                        Split {
                            local: acc.local + v.local,
                            remote: acc.remote + v.remote,
                        }
                    }),
                };
                Point {
                    at: meter.start_unix_ms + ((slot + 1) * slot_secs * 1000) as i64,
                    local_bytes_per_second: split.local / slot_secs,
                    remote_bytes_per_second: split.remote / slot_secs,
                }
            })
            .collect();
        (slot_secs, points)
    }
}

/// Clients in the home network: loopback, private and link-local addresses.
pub fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_local(IpAddr::V4(v4)),
            None => {
                let first = v6.segments()[0];
                v6.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
            }
        },
    }
}

/// Is the client of `request` outside the home network? Behind a reverse proxy the
/// forwarded address counts (the proxy itself is usually local).
fn is_remote(request: &Request) -> bool {
    let headers = request.headers();
    let forwarded = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .or_else(|| headers.get("x-real-ip").and_then(|v| v.to_str().ok()))
        .and_then(|v| v.trim().parse::<IpAddr>().ok());
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());
    // Unknown (tests, in-process calls): local.
    forwarded.or(peer).is_some_and(|ip| !is_local(ip))
}

/// The stream a media request belongs to; `None` for everything else.
pub fn stream_key(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/api/v1/")?;
    let mut parts = rest.split('/');
    match (parts.next()?, parts.next()?) {
        ("stream", file) if !file.is_empty() => Some(format!("file:{file}")),
        ("transcode", session) if parts.next().is_some() => Some(format!("transcode:{session}")),
        ("livetv", "sessions") => {
            let session = parts.next()?;
            parts.next()?;
            Some(format!("live:{session}"))
        }
        _ => None,
    }
}

/// Middleware: counts the response bytes of media routes.
pub(crate) async fn track(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let Some(key) = stream_key(request.uri().path()) else {
        return next.run(request).await;
    };
    let remote = is_remote(&request);
    let response = next.run(request).await;
    let meter = state.ops.bandwidth.clone();
    response.map(move |body| {
        Body::new(body.map_frame(move |frame| {
            if let Some(data) = frame.data_ref() {
                meter.record(&key, data.len() as u64, remote);
            }
            frame
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_of_media_routes() {
        assert_eq!(stream_key("/api/v1/stream/12"), Some("file:12".into()));
        assert_eq!(
            stream_key("/api/v1/transcode/abc/3.m4s"),
            Some("transcode:abc".into())
        );
        assert_eq!(
            stream_key("/api/v1/livetv/sessions/xyz/live.m3u8"),
            Some("live:xyz".into())
        );
        assert_eq!(stream_key("/api/v1/transcode/abc"), None, "stop request");
        assert_eq!(stream_key("/api/v1/items"), None);
        assert_eq!(stream_key("/index.html"), None);
    }

    #[test]
    fn counts_per_stream_and_total() {
        let meter = BandwidthMeter::default();
        meter.record("file:1", 1000, false);
        meter.record("file:1", 500, true);
        meter.record("transcode:a", 250, false);
        assert_eq!(meter.stream("file:1").bytes, 1500);
        assert_eq!(meter.stream("file:1").bytes_per_second, 150);
        assert_eq!(meter.stream("missing"), Usage::default());
        assert_eq!(meter.total().bytes, 1750);
        assert_eq!(meter.total().bytes_per_second, 175);

        // The real-time chart ends with the current second, split local/remote.
        let (slot, points) = meter.history(Range::RealTime);
        assert_eq!(slot, 1);
        let last = points.last().unwrap();
        assert_eq!(
            (last.local_bytes_per_second, last.remote_bytes_per_second),
            (1250, 500)
        );
        let (slot, hour) = meter.history(Range::Hour);
        assert_eq!(slot, 60);
        assert!(hour.len() <= 60);
        assert!(hour.windows(2).all(|w| w[1].at - w[0].at == 60_000));
    }

    #[test]
    fn home_network_addresses_are_local() {
        for ip in [
            "127.0.0.1",
            "192.168.188.20",
            "10.0.0.5",
            "172.19.16.1",
            "169.254.1.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.1.2",
        ] {
            assert!(is_local(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "100.64.0.1", "2a00:1450::1", "::ffff:1.1.1.1"] {
            assert!(!is_local(ip.parse().unwrap()), "{ip}");
        }
    }
}
