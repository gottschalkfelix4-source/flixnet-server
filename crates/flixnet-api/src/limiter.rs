//! In-memory brute-force protection for passwords and PINs.
//!
//! Failures are counted per key (e.g. `login:felix`, `switch:7`) inside a sliding
//! window; once the limit is hit the key is locked until the window expires.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::StatusCode;

use crate::error::ApiError;

pub struct AttemptLimiter {
    max_failures: u32,
    window: Duration,
    entries: Mutex<HashMap<String, (u32, Instant)>>,
}

impl AttemptLimiter {
    pub fn new(max_failures: u32, window: Duration) -> Self {
        Self {
            max_failures,
            window,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Errors with 429 while the key is locked.
    pub fn check(&self, key: &str) -> Result<(), ApiError> {
        let mut entries = self.entries.lock().expect("limiter lock poisoned");
        if let Some((failures, since)) = entries.get(key).copied() {
            let elapsed = since.elapsed();
            if elapsed >= self.window {
                entries.remove(key);
            } else if failures >= self.max_failures {
                let minutes = (self.window - elapsed).as_secs().div_ceil(60);
                return Err(ApiError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    "too_many_attempts",
                    format!("Zu viele Fehlversuche. Bitte in {minutes} Minuten erneut versuchen."),
                ));
            }
        }
        Ok(())
    }

    pub fn record_failure(&self, key: &str) {
        let mut entries = self.entries.lock().expect("limiter lock poisoned");
        // Drop stale entries opportunistically so the map cannot grow without bound.
        if entries.len() > 10_000 {
            let window = self.window;
            entries.retain(|_, (_, since)| since.elapsed() < window);
        }
        let entry = entries.entry(key.to_owned()).or_insert((0, Instant::now()));
        if entry.1.elapsed() >= self.window {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
    }

    pub fn reset(&self, key: &str) {
        self.entries
            .lock()
            .expect("limiter lock poisoned")
            .remove(key);
    }
}

/// Limits used by the auth endpoints.
pub struct Limits {
    /// Password sign-in, keyed by lower-cased user name.
    pub login: AttemptLimiter,
    /// PIN/password checks when switching profiles, keyed by target user id.
    pub unlock: AttemptLimiter,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            login: AttemptLimiter::new(10, Duration::from_secs(15 * 60)),
            unlock: AttemptLimiter::new(5, Duration::from_secs(15 * 60)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locks_after_max_failures_and_reset_unlocks() {
        let limiter = AttemptLimiter::new(3, Duration::from_secs(60));
        for _ in 0..3 {
            assert!(limiter.check("k").is_ok());
            limiter.record_failure("k");
        }
        let err = limiter.check("k").unwrap_err();
        assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(limiter.check("other").is_ok());

        limiter.reset("k");
        assert!(limiter.check("k").is_ok());
    }

    #[test]
    fn window_expiry_clears_failures() {
        let limiter = AttemptLimiter::new(1, Duration::from_millis(20));
        limiter.record_failure("k");
        assert!(limiter.check("k").is_err());
        std::thread::sleep(Duration::from_millis(30));
        assert!(limiter.check("k").is_ok());
    }
}
