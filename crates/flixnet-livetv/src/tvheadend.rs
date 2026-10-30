//! tvheadend as a tuner and a guide source, over its HTTP API (no HTSP):
//!
//! * channels: `GET /api/channel/grid` (uuid, name, number, icon);
//! * streams: `GET /stream/channel/<uuid>?profile=pass` (MPEG-TS as received);
//! * guide: `GET /api/epg/events/grid`, paged.
//!
//! Accounts use HTTP basic authentication (tvheadend: "Authentifizierungstyp: Plain
//! und Digest"); ffmpeg gets the credentials in the stream URL. Guide channels are
//! the channel uuids, which the imported channels carry as `tvg-id`, so matching is
//! exact.

use flixnet_db::livetv::{NewChannel, NewEpgChannel, NewProgram};
use serde_json::Value;

use crate::guide::{GuideData, GuideSource};
use crate::tuner::{Lineup, Tuner, TunerError, TunerInfo, TunerKind};

/// Events per guide page.
const PAGE: usize = 2000;
/// Pages at most (a guide never has more than this many events).
const MAX_PAGES: usize = 200;

#[derive(Debug, Clone)]
pub struct Tvheadend {
    /// `http://host:9981` (path prefix allowed).
    pub url: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum TvheadendError {
    #[error("tvheadend nicht erreichbar: {0}")]
    Http(#[from] reqwest::Error),
    #[error("tvheadend: Anmeldung abgelehnt")]
    Unauthorized,
    #[error("tvheadend: unerwartete Antwort")]
    Invalid,
}

impl Tvheadend {
    fn base(&self) -> &str {
        self.url.trim().trim_end_matches('/')
    }

    async fn get(
        &self,
        client: &reqwest::Client,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Value, TvheadendError> {
        let mut request = client.get(format!("{}{path}", self.base())).query(query);
        if let Some(user) = &self.username {
            request = request.basic_auth(user, self.password.as_deref());
        }
        let response = request.send().await?;
        if matches!(response.status().as_u16(), 401 | 403) {
            return Err(TvheadendError::Unauthorized);
        }
        Ok(response.error_for_status()?.json().await?)
    }

    /// The channels (enabled ones).
    pub async fn channels(&self, client: &reqwest::Client) -> Result<Vec<Value>, TvheadendError> {
        let body = self
            .get(
                client,
                "/api/channel/grid",
                &[("start", "0".into()), ("limit", "100000".into())],
            )
            .await?;
        let entries = body
            .get("entries")
            .and_then(Value::as_array)
            .ok_or(TvheadendError::Invalid)?;
        Ok(entries
            .iter()
            .filter(|c| c.get("enabled").and_then(Value::as_bool) != Some(false))
            .cloned()
            .collect())
    }

    /// Stream URL of a channel, with the credentials ffmpeg needs.
    pub fn stream_url(&self, uuid: &str) -> Option<String> {
        if !uuid.chars().all(|c| c.is_ascii_alphanumeric()) || uuid.is_empty() {
            return None;
        }
        let mut url = reqwest::Url::parse(self.base()).ok()?;
        if let Some(user) = &self.username {
            url.set_username(user).ok()?;
            url.set_password(self.password.as_deref()).ok()?;
        }
        let path = format!("{}/stream/channel/{uuid}", url.path().trim_end_matches('/'));
        url.set_path(&path);
        url.set_query(Some("profile=pass"));
        Some(url.to_string())
    }

    /// An icon path of the channel grid as an absolute URL.
    fn icon_url(&self, channel: &Value) -> Option<String> {
        let icon = channel
            .get("icon_public_url")
            .or_else(|| channel.get("icon"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())?;
        if icon.starts_with("http://") || icon.starts_with("https://") {
            Some(icon.to_owned())
        } else if icon.contains("://") {
            None // file://, picon:// …
        } else {
            Some(format!("{}/{}", self.base(), icon.trim_start_matches('/')))
        }
    }
}

fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn number_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Number(n) => n.as_f64().filter(|n| *n > 0.0).map(|n| {
            if n.fract() == 0.0 {
                format!("{}", n as i64)
            } else {
                n.to_string()
            }
        }),
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        _ => None,
    }
}

impl Tuner for Tvheadend {
    fn kind(&self) -> TunerKind {
        TunerKind::Tvheadend
    }

    async fn lineup(&self, client: &reqwest::Client) -> Result<Lineup, TunerError> {
        let entries = self.channels(client).await?;
        let mut channels = Vec::with_capacity(entries.len());
        for (position, c) in entries.iter().enumerate() {
            let (Some(uuid), Some(name)) = (text(c.get("uuid")), text(c.get("name"))) else {
                continue;
            };
            let Some(stream_url) = self.stream_url(&uuid) else {
                continue;
            };
            let number = number_text(c.get("number"));
            channels.push(NewChannel {
                sort_order: number
                    .as_deref()
                    .and_then(|n| n.parse::<f64>().ok())
                    .unwrap_or(1_000_000.0 + position as f64),
                remote_id: uuid.clone(),
                number,
                logo_url: self.icon_url(c),
                // Channel tags are uuids in the grid; no group.
                group_title: None,
                is_hd: name
                    .split(|ch: char| !ch.is_alphanumeric())
                    .any(|w| w.eq_ignore_ascii_case("hd")),
                name,
                stream_url,
                tvg_id: Some(uuid),
                tvg_name: None,
                video_codec: None,
                audio_codec: None,
            });
        }
        Ok(Lineup {
            info: TunerInfo::default(),
            channels,
        })
    }
}

impl GuideSource for Tvheadend {
    async fn fetch(&self, client: &reqwest::Client) -> anyhow::Result<GuideData> {
        let channels: Vec<NewEpgChannel> = self
            .channels(client)
            .await?
            .iter()
            .filter_map(|c| {
                let uuid = text(c.get("uuid"))?;
                let mut names: Vec<String> = text(c.get("name")).into_iter().collect();
                names.extend(number_text(c.get("number")));
                Some((uuid, names, self.icon_url(c)))
            })
            .collect();
        let mut programs = Vec::new();
        for page in 0..MAX_PAGES {
            let body = self
                .get(
                    client,
                    "/api/epg/events/grid",
                    &[
                        ("start", (page * PAGE).to_string()),
                        ("limit", PAGE.to_string()),
                    ],
                )
                .await?;
            let entries = body
                .get("entries")
                .and_then(Value::as_array)
                .ok_or(TvheadendError::Invalid)?;
            programs.extend(entries.iter().filter_map(event));
            let total = body.get("totalCount").and_then(Value::as_u64).unwrap_or(0) as usize;
            if entries.len() < PAGE || (page + 1) * PAGE >= total {
                break;
            }
        }
        Ok(GuideData { channels, programs })
    }
}

/// A guide program from an EPG event.
fn event(e: &Value) -> Option<NewProgram> {
    let channel_id = text(e.get("channelUuid"))?;
    let start = e.get("start").and_then(Value::as_i64)?;
    let stop = e.get("stop").and_then(Value::as_i64).unwrap_or(start);
    let season = e
        .get("seasonNumber")
        .and_then(Value::as_i64)
        .filter(|n| *n > 0);
    let episode = e
        .get("episodeNumber")
        .and_then(Value::as_i64)
        .filter(|n| *n > 0);
    Some(NewProgram {
        channel_id,
        start_ms: start * 1000,
        end_ms: stop * 1000,
        title: text(e.get("title"))?,
        subtitle: text(e.get("subtitle")),
        description: text(e.get("description")).or_else(|| text(e.get("summary"))),
        categories: e
            .get("category")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|c| text(Some(c)))
            .collect(),
        season,
        episode,
        episode_label: text(e.get("episodeOnscreen")).or(match (season, episode) {
            (Some(s), Some(ep)) => Some(format!("S{s:02}E{ep:02}")),
            _ => None,
        }),
        year: e
            .get("copyright_year")
            .and_then(Value::as_i64)
            .filter(|y| *y > 1800),
        icon_url: text(e.get("image")).filter(|u| u.starts_with("http")),
        rating: e
            .get("ageRating")
            .and_then(Value::as_i64)
            .filter(|a| *a > 0)
            .map(|a| format!("FSK {a}")),
        is_new: e.get("new").and_then(Value::as_i64).unwrap_or(0) != 0,
        series_id: text(e.get("serieslinkUri")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_urls_carry_credentials_and_reject_odd_ids() {
        let t = Tvheadend {
            url: "http://tvh.local:9981/".into(),
            username: Some("felix".into()),
            password: Some("p@ss".into()),
        };
        assert_eq!(
            t.stream_url("0123abcd").as_deref(),
            Some("http://felix:p%40ss@tvh.local:9981/stream/channel/0123abcd?profile=pass")
        );
        assert_eq!(t.stream_url("../x"), None);
        let open = Tvheadend {
            url: "http://tvh.local:9981/tvh".into(),
            username: None,
            password: None,
        };
        assert_eq!(
            open.stream_url("ab").as_deref(),
            Some("http://tvh.local:9981/tvh/stream/channel/ab?profile=pass")
        );
    }
}
