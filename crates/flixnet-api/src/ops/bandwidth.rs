//! Bytes served by the media routes (direct streams, HLS transcodes, live TV), per
//! stream and in total, with the rate over the last few seconds.
//!
//! A middleware wraps the response bodies of those routes and counts every frame as
//! it is sent, so the numbers reflect what actually left the server.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use http_body_util::BodyExt;

use crate::AppState;

/// Seconds the current rate is averaged over.
const WINDOW_SECS: u64 = 10;
/// Streams without traffic for this long are forgotten.
const IDLE: Duration = Duration::from_secs(3600);

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
    streams: HashMap<String, Stream>,
    total: u64,
    window: Window,
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
                streams: HashMap::new(),
                total: 0,
                window: Window::default(),
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
    pub fn record(&self, key: &str, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let mut meter = self.inner.lock().expect("bandwidth lock");
        let now = Instant::now();
        let second = now.duration_since(meter.start).as_secs();
        meter.total += bytes;
        meter.window.add(second, bytes);
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
    let response = next.run(request).await;
    let meter = state.ops.bandwidth.clone();
    response.map(move |body| {
        Body::new(body.map_frame(move |frame| {
            if let Some(data) = frame.data_ref() {
                meter.record(&key, data.len() as u64);
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
        meter.record("file:1", 1000);
        meter.record("file:1", 500);
        meter.record("transcode:a", 250);
        assert_eq!(meter.stream("file:1").bytes, 1500);
        assert_eq!(meter.stream("file:1").bytes_per_second, 150);
        assert_eq!(meter.stream("missing"), Usage::default());
        assert_eq!(meter.total().bytes, 1750);
        assert_eq!(meter.total().bytes_per_second, 175);
    }
}
