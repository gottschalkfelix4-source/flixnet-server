//! Schedules Direct (JSON API 20141201) as a guide source.
//!
//! 1. `POST /token` with the user name and the SHA-1 of the password → token.
//! 2. `GET /lineups/<lineup>` → stations (the guide channels) and their numbers.
//! 3. `POST /schedules` for the stations and the coming days (in batches) → airings
//!    with program ids.
//! 4. `POST /programs` for the program ids (in batches) → titles, episode details,
//!    descriptions, genres and ratings.
//!
//! The password is stored only as its SHA-1 (what the API wants anyway). The
//! account's lineups are listed for the settings page (`GET /lineups`).

use std::collections::{HashMap, HashSet};

use flixnet_db::livetv::{NewEpgChannel, NewProgram};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha1::{Digest, Sha1};

use crate::guide::{GuideData, GuideSource};

/// The public API.
pub const DEFAULT_BASE: &str = "https://json.schedulesdirect.org/20141201";
/// Settings key of [`SdCredentials`].
pub const SETTINGS_KEY: &str = "livetv.schedules_direct";

/// Stations per `/schedules` request and programs per `/programs` request (the API
/// allows 5000 each).
const STATION_BATCH: usize = 500;
const PROGRAM_BATCH: usize = 500;
/// Days of guide fetched.
const DAYS: i64 = 7;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdCredentials {
    pub username: String,
    /// SHA-1 of the password, lower-case hex.
    pub password_sha1: String,
    /// Another API base (tests); `None` = [`DEFAULT_BASE`].
    #[serde(default)]
    pub base_url: Option<String>,
}

impl SdCredentials {
    pub fn base(&self) -> &str {
        self.base_url
            .as_deref()
            .unwrap_or(DEFAULT_BASE)
            .trim_end_matches('/')
    }
}

/// SHA-1 hex of a password, as Schedules Direct expects it.
pub fn password_hash(password: &str) -> String {
    hex::encode(Sha1::digest(password.as_bytes()))
}

#[derive(Debug, thiserror::Error)]
pub enum SdError {
    #[error("Schedules Direct nicht erreichbar: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Schedules Direct: {0}")]
    Api(String),
    #[error("Schedules Direct: Zugangsdaten fehlen")]
    MissingCredentials,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    #[serde(default)]
    code: i64,
    token: Option<String>,
    message: Option<String>,
}

/// Signs in; returns the token.
pub async fn token(
    client: &reqwest::Client,
    credentials: &SdCredentials,
) -> Result<String, SdError> {
    if credentials.username.is_empty() || credentials.password_sha1.is_empty() {
        return Err(SdError::MissingCredentials);
    }
    let response: TokenResponse = client
        .post(format!("{}/token", credentials.base()))
        .json(&serde_json::json!({
            "username": credentials.username,
            "password": credentials.password_sha1,
        }))
        .send()
        .await?
        .json()
        .await?;
    match response.token {
        Some(token) if response.code == 0 => Ok(token),
        _ => Err(SdError::Api(
            response
                .message
                .unwrap_or_else(|| "Anmeldung abgelehnt".to_owned()),
        )),
    }
}

/// Errors come as `{ "code": 4001, "message": "…" }` with any status.
async fn json(response: reqwest::Response) -> Result<Value, SdError> {
    let status = response.status();
    let body: Value = response.json().await?;
    let code = body.get("code").and_then(Value::as_i64).unwrap_or(0);
    if !status.is_success() || (body.is_object() && code != 0 && body.get("message").is_some()) {
        return Err(SdError::Api(
            body.get("message")
                .and_then(Value::as_str)
                .unwrap_or("unbekannter Fehler")
                .to_owned(),
        ));
    }
    Ok(body)
}

/// A lineup of the account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SdLineup {
    pub lineup: String,
    pub name: Option<String>,
    pub location: Option<String>,
}

/// The lineups added to the account.
pub async fn lineups(
    client: &reqwest::Client,
    credentials: &SdCredentials,
) -> Result<Vec<SdLineup>, SdError> {
    let token = token(client, credentials).await?;
    let body = json(
        client
            .get(format!("{}/lineups", credentials.base()))
            .header("token", token)
            .send()
            .await?,
    )
    .await?;
    Ok(body
        .get("lineups")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|l| {
            Some(SdLineup {
                lineup: l.get("lineup")?.as_str()?.to_owned(),
                name: l.get("name").and_then(Value::as_str).map(str::to_owned),
                location: l.get("location").and_then(Value::as_str).map(str::to_owned),
            })
        })
        .collect())
}

/// `2026-10-30T19:15:00Z` → unix ms.
fn parse_air_time(text: &str) -> Option<i64> {
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 14 {
        return None;
    }
    crate::xmltv::parse_time(&format!("{} +0000", &digits[..14]))
}

fn text_of(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// German text first, then English, then whatever there is.
fn description(program: &Value) -> Option<String> {
    let descriptions = program.get("descriptions")?;
    for key in ["description1000", "description100"] {
        let list = descriptions.get(key).and_then(Value::as_array)?;
        let pick = |lang: &str| {
            list.iter()
                .find(|d| d.get("descriptionLanguage").and_then(Value::as_str) == Some(lang))
        };
        if let Some(d) = pick("de").or_else(|| pick("en")).or_else(|| list.first())
            && let Some(text) = text_of(d.get("description"))
        {
            return Some(text);
        }
    }
    None
}

/// `EP000000060003` → `SH00000006` (the series of an episode).
pub fn series_of(program_id: &str) -> Option<String> {
    (program_id.len() >= 10 && (program_id.starts_with("EP") || program_id.starts_with("SH")))
        .then(|| format!("SH{}", &program_id[2..10]))
}

/// The guide program of an airing plus its program details.
fn program(station: &str, airing: &Value, details: Option<&Value>) -> Option<NewProgram> {
    let id = airing.get("programID")?.as_str()?;
    let start_ms = parse_air_time(airing.get("airDateTime")?.as_str()?)?;
    let duration = airing.get("duration").and_then(Value::as_i64).unwrap_or(0);
    let details = details?;
    let title = details
        .get("titles")
        .and_then(Value::as_array)
        .and_then(|t| t.first())
        .and_then(|t| text_of(t.get("title120")))?;
    let gracenote = details
        .get("metadata")
        .and_then(Value::as_array)
        .and_then(|m| m.iter().find_map(|m| m.get("Gracenote")));
    let season = gracenote
        .and_then(|g| g.get("season"))
        .and_then(Value::as_i64);
    let episode = gracenote
        .and_then(|g| g.get("episode"))
        .and_then(Value::as_i64);
    let rating = airing
        .get("ratings")
        .or_else(|| details.get("contentRating"))
        .and_then(Value::as_array)
        .and_then(|r| r.first())
        .and_then(|r| text_of(r.get("code")))
        // A bare number from the FSK reads as an age.
        .map(|code| {
            if code.chars().all(|c| c.is_ascii_digit()) {
                format!("FSK {code}")
            } else {
                code
            }
        });
    Some(NewProgram {
        channel_id: station.to_owned(),
        start_ms,
        end_ms: start_ms + duration.max(0) * 1000,
        title,
        subtitle: text_of(details.get("episodeTitle150")),
        description: description(details),
        categories: details
            .get("genres")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|g| text_of(Some(g)))
            .collect(),
        season,
        episode,
        episode_label: match (season, episode) {
            (Some(s), Some(e)) => Some(format!("S{s:02}E{e:02}")),
            _ => None,
        },
        year: details
            .get("movie")
            .and_then(|m| m.get("year"))
            .and_then(|y| {
                y.as_str()
                    .and_then(|s| s.parse().ok())
                    .or_else(|| y.as_i64())
            }),
        icon_url: None,
        rating,
        is_new: airing.get("new").and_then(Value::as_bool).unwrap_or(false),
        series_id: series_of(id),
    })
}

/// A lineup of Schedules Direct.
pub struct SchedulesDirect {
    pub credentials: SdCredentials,
    pub lineup: String,
    /// First day fetched (unix ms; usually now).
    pub from_ms: i64,
}

impl GuideSource for SchedulesDirect {
    async fn fetch(&self, client: &reqwest::Client) -> anyhow::Result<GuideData> {
        let base = self.credentials.base();
        let token = token(client, &self.credentials).await?;
        let lineup = self.lineup.trim();
        if lineup.is_empty() || lineup.contains(['/', '?', '#']) {
            anyhow::bail!("Schedules Direct: ungültiges Lineup");
        }
        let body = json(
            client
                .get(format!("{base}/lineups/{lineup}"))
                .header("token", &token)
                .send()
                .await?,
        )
        .await?;
        let numbers: HashMap<String, String> = body
            .get("map")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|m| {
                let station = m.get("stationID")?.as_str()?.to_owned();
                let number =
                    text_of(m.get("channel")).or_else(|| text_of(m.get("logicalChannelNumber")))?;
                Some((station, number))
            })
            .collect();
        let mut channels: Vec<NewEpgChannel> = Vec::new();
        for station in body
            .get("stations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(id) = text_of(station.get("stationID")) else {
                continue;
            };
            let mut names: Vec<String> = [station.get("name"), station.get("callsign")]
                .into_iter()
                .filter_map(text_of)
                .collect();
            if let Some(number) = numbers.get(&id) {
                names.push(number.clone());
            }
            names.dedup();
            let logo = station
                .get("stationLogo")
                .and_then(Value::as_array)
                .and_then(|l| l.first())
                .or_else(|| station.get("logo"))
                .and_then(|l| text_of(l.get("URL")));
            channels.push((id, names, logo));
        }

        // Schedules for the coming days, in batches of stations.
        let dates: Vec<String> = (0..DAYS)
            .map(|d| {
                let (y, m, day) = crate::stream::civil_from_days((self.from_ms / 86_400_000) + d);
                format!("{y:04}-{m:02}-{day:02}")
            })
            .collect();
        let mut airings: Vec<(String, Value)> = Vec::new();
        for batch in channels.chunks(STATION_BATCH) {
            let request: Vec<Value> = batch
                .iter()
                .map(|(id, _, _)| serde_json::json!({ "stationID": id, "date": dates }))
                .collect();
            let body = json(
                client
                    .post(format!("{base}/schedules"))
                    .header("token", &token)
                    .json(&request)
                    .send()
                    .await?,
            )
            .await?;
            for schedule in body.as_array().into_iter().flatten() {
                let Some(station) = text_of(schedule.get("stationID")) else {
                    continue;
                };
                for airing in schedule
                    .get("programs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    airings.push((station.clone(), airing.clone()));
                }
            }
        }

        // Program details, in batches.
        let ids: Vec<String> = airings
            .iter()
            .filter_map(|(_, a)| text_of(a.get("programID")))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let mut details: HashMap<String, Value> = HashMap::new();
        for batch in ids.chunks(PROGRAM_BATCH) {
            let body = json(
                client
                    .post(format!("{base}/programs"))
                    .header("token", &token)
                    .json(&batch)
                    .send()
                    .await?,
            )
            .await?;
            for program in body.as_array().into_iter().flatten() {
                if let Some(id) = text_of(program.get("programID")) {
                    details.insert(id, program.clone());
                }
            }
        }

        let programs = airings
            .iter()
            .filter_map(|(station, airing)| {
                let id = airing.get("programID").and_then(Value::as_str)?;
                program(station, airing, details.get(id))
            })
            .collect();
        Ok(GuideData { channels, programs })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_and_series_ids() {
        assert_eq!(
            password_hash("geheim"),
            "906072001efddf3e11e6d2b5782f4777fe038739"
        );
        assert_eq!(series_of("EP000000060003").as_deref(), Some("SH00000006"));
        assert_eq!(series_of("MV000000010000"), None);
        assert_eq!(
            parse_air_time("2026-10-30T19:15:00Z"),
            Some(1_793_387_700_000)
        );
    }
}
