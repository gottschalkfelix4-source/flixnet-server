//! Small helpers: Jellyfin's tick and date formats, case-insensitive query parameters,
//! request bodies in any key casing, and JSON responses without `null` members.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use flixnet_api::error::ApiError;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Jellyfin counts time in ticks of 100 ns.
pub const TICKS_PER_MS: i64 = 10_000;

pub fn ticks(ms: i64) -> i64 {
    ms.saturating_mul(TICKS_PER_MS)
}

pub fn ms(ticks: i64) -> i64 {
    ticks / TICKS_PER_MS
}

/// Unix milliseconds as Jellyfin writes dates: `2024-05-01T12:34:56.0000000Z`.
pub fn iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}0000Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        ms.rem_euclid(1000)
    )
}

/// A `YYYY-MM-DD` premiere date as an ISO-8601 timestamp (midnight UTC).
pub fn iso_date(date: &str) -> Option<String> {
    let date = date.get(..10)?;
    let bytes = date.as_bytes();
    let digits = |r: std::ops::Range<usize>| bytes[r].iter().all(u8::is_ascii_digit);
    (digits(0..4) && bytes[4] == b'-' && digits(5..7) && bytes[7] == b'-' && digits(8..10))
        .then(|| format!("{date}T00:00:00.0000000Z"))
}

/// Days since 1970-01-01 → (year, month, day) (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `PlaySessionId` / `playSessionId` → `play_session_id`.
pub fn snake_case(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    let mut out = String::with_capacity(key.len() + 4);
    for (i, c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() && i > 0 && chars[i - 1] != '_' {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(char::is_ascii_lowercase);
            if prev.is_ascii_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_ascii_uppercase() && next_lower)
            {
                out.push('_');
            }
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

pub(crate) fn snake_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (snake_case(&k), snake_keys(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(snake_keys).collect()),
        other => other,
    }
}

/// A JSON body whose keys may come in any casing (Jellyfin's own server reads them
/// case-insensitively); keys are turned into snake_case before deserializing. An
/// empty body counts as `{}`.
pub struct JfJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for JfJson<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(|_| bad_request("Ungültiger Inhalt."))?;
        let value = if bytes.iter().all(u8::is_ascii_whitespace) {
            Value::Object(Default::default())
        } else {
            serde_json::from_slice(&bytes).map_err(|_| bad_request("Ungültiges JSON."))?
        };
        serde_json::from_value(snake_keys(value))
            .map(JfJson)
            .map_err(|err| bad_request(format!("Ungültige Angaben: {err}")))
    }
}

pub fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", message)
}

/// Query parameters with case-insensitive names (`ParentId` = `parentId`); repeated
/// parameters are joined with commas.
#[derive(Debug, Default, Clone)]
pub struct Params(HashMap<String, String>);

impl Params {
    pub fn parse(query: Option<&str>) -> Self {
        let mut map: HashMap<String, String> = HashMap::new();
        for pair in query.unwrap_or_default().split('&') {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let key = percent_decode(key).to_ascii_lowercase();
            if key.is_empty() {
                continue;
            }
            let value = percent_decode(value);
            map.entry(key)
                .and_modify(|v| {
                    v.push(',');
                    v.push_str(&value);
                })
                .or_insert(value);
        }
        Self(map)
    }

    /// A non-empty value.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(|v| v.trim()).filter(|v| !v.is_empty())
    }

    pub fn i64(&self, name: &str) -> Option<i64> {
        self.get(name).and_then(|v| v.parse().ok())
    }

    pub fn bool(&self, name: &str) -> Option<bool> {
        self.get(name)
            .and_then(|v| match v.to_ascii_lowercase().as_str() {
                "true" | "1" => Some(true),
                "false" | "0" => Some(false),
                _ => None,
            })
    }

    /// Comma-separated values.
    pub fn list(&self, name: &str) -> Vec<String> {
        self.split(name, &[','])
    }

    /// Values separated by any of `separators`.
    pub fn split(&self, name: &str, separators: &[char]) -> Vec<String> {
        self.get(name)
            .map(|v| {
                v.split(separators)
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Lower-cased comma-separated values as a set (`Fields`, `Filters`, …).
    pub fn set(&self, name: &str) -> HashSet<String> {
        self.list(name)
            .into_iter()
            .map(|v| v.to_ascii_lowercase())
            .collect()
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Params {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Infallible> {
        Ok(Self::parse(parts.uri.query()))
    }
}

/// `%XX` escapes and `+` as space (query strings); invalid escapes are kept as they are.
pub fn percent_decode(input: &str) -> String {
    decode(input, true)
}

/// `%XX` escapes only (header values, where `+` is a plain character).
pub fn percent_decode_header(input: &str) -> String {
    decode(input, false)
}

fn decode(input: &str, plus_as_space: bool) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                    (Some(hi), Some(lo)) => {
                        out.push(hi << 4 | lo);
                        i += 3;
                        continue;
                    }
                    _ => out.push(b'%'),
                }
            }
            b'+' if plus_as_space => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// A JSON response like Jellyfin's: members that are `null` are left out.
pub struct Jf<T>(pub T);

impl<T: Serialize> IntoResponse for Jf<T> {
    fn into_response(self) -> Response {
        match serde_json::to_value(&self.0) {
            Ok(value) => Json(strip_nulls(value)).into_response(),
            Err(err) => ApiError::internal(err).into_response(),
        }
    }
}

pub fn strip_nulls(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, strip_nulls(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(strip_nulls).collect()),
        other => other,
    }
}

/// 32 random lowercase hex characters.
pub fn random_id() -> String {
    flixnet_core::auth::new_token().token[..32].to_owned()
}

/// 32 lowercase hex characters derived from `input` (stable ids for reported sessions).
pub fn derived_id(input: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(&Sha256::digest(input.as_bytes())[..16])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_ticks() {
        assert_eq!(iso(0), "1970-01-01T00:00:00.0000000Z");
        assert_eq!(iso(1_714_566_896_123), "2024-05-01T12:34:56.1230000Z");
        assert_eq!(iso(951_782_400_000), "2000-02-29T00:00:00.0000000Z");
        assert_eq!(
            iso_date("1999-03-31").as_deref(),
            Some("1999-03-31T00:00:00.0000000Z")
        );
        assert_eq!(iso_date("1999"), None);
        assert_eq!(iso_date("abcd-ef-gh"), None);
        assert_eq!(ticks(1500), 15_000_000);
        assert_eq!(ms(15_000_000), 1500);
    }

    #[test]
    fn keys_become_snake_case() {
        assert_eq!(snake_case("PlaySessionId"), "play_session_id");
        assert_eq!(snake_case("playSessionId"), "play_session_id");
        assert_eq!(snake_case("Pw"), "pw");
        assert_eq!(snake_case("item_id"), "item_id");
        assert_eq!(snake_case("DirectPlayProfiles"), "direct_play_profiles");
        assert_eq!(snake_case("MaxStreamingBitrate"), "max_streaming_bitrate");
    }

    #[test]
    fn query_parameters_ignore_case_and_join_repeats() {
        let p = Params::parse(Some(
            "ParentId=ab&fields=Genres&Fields=People&SearchTerm=der%20pate+2&x",
        ));
        assert_eq!(p.get("parentid"), Some("ab"));
        assert_eq!(p.list("fields"), ["Genres", "People"]);
        assert_eq!(p.get("searchterm"), Some("der pate 2"));
        assert_eq!(p.get("x"), None);
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
        assert_eq!(percent_decode("%C3%BC"), "ü");
    }

    #[test]
    fn nulls_are_dropped() {
        let v = strip_nulls(serde_json::json!({"A": null, "B": [ {"C": null, "D": 1} ]}));
        assert_eq!(v, serde_json::json!({"B": [ {"D": 1} ]}));
    }
}
