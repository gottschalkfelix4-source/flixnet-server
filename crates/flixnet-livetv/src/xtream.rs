//! Xtream Codes accounts: `player_api.php` for the account and its live streams;
//! streams play from `{server}/live/{user}/{password}/{stream_id}.ts`.
//!
//! Panels disagree on types (numbers sometimes arrive as strings), so the JSON is
//! read field by field.

use reqwest::Url;
use serde_json::Value;

use crate::http::{self, FetchError};

#[derive(Debug, thiserror::Error)]
pub enum XtreamError {
    #[error("invalid server URL")]
    InvalidUrl,
    #[error("account rejected by the server")]
    Unauthorized,
    #[error("unexpected answer: {0}")]
    Format(&'static str),
    #[error(transparent)]
    Fetch(#[from] FetchError),
}

#[derive(Debug, Clone, PartialEq)]
pub struct XtreamAccount {
    /// Streams the account may run at once (`None` = not stated).
    pub max_connections: Option<i64>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct XtreamStream {
    pub stream_id: String,
    pub num: Option<String>,
    pub name: String,
    pub icon: Option<String>,
    pub epg_channel_id: Option<String>,
    pub category: Option<String>,
}

/// A number or string field as text.
fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Reads the `player_api.php` account answer.
pub fn parse_account(value: &Value) -> Result<XtreamAccount, XtreamError> {
    let info = value
        .get("user_info")
        .ok_or(XtreamError::Format("user_info missing"))?;
    if int(&info["auth"]) == Some(0) {
        return Err(XtreamError::Unauthorized);
    }
    Ok(XtreamAccount {
        max_connections: int(&info["max_connections"]).filter(|n| *n > 0),
        status: text(&info["status"]),
    })
}

/// Reads `get_live_streams` with category names from `get_live_categories`.
pub fn parse_streams(streams: &Value, categories: &Value) -> Vec<XtreamStream> {
    let names: Vec<(String, String)> = categories
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|c| Some((text(&c["category_id"])?, text(&c["category_name"])?)))
                .collect()
        })
        .unwrap_or_default();
    streams
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|s| {
                    let stream_id = text(&s["stream_id"])?;
                    let category = text(&s["category_id"]).and_then(|id| {
                        names.iter().find(|(c, _)| *c == id).map(|(_, n)| n.clone())
                    });
                    Some(XtreamStream {
                        name: text(&s["name"]).unwrap_or_else(|| format!("Sender {stream_id}")),
                        num: text(&s["num"]),
                        icon: text(&s["stream_icon"]),
                        epg_channel_id: text(&s["epg_channel_id"]),
                        category,
                        stream_id,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The server's base URL without a trailing `player_api.php` or slash.
fn base(server: &str) -> Result<Url, XtreamError> {
    let trimmed = server.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix("/player_api.php").unwrap_or(trimmed);
    let url = Url::parse(trimmed).map_err(|_| XtreamError::InvalidUrl)?;
    if !matches!(url.scheme(), "http" | "https") || url.cannot_be_a_base() {
        return Err(XtreamError::InvalidUrl);
    }
    Ok(url)
}

fn api_url(
    server: &str,
    username: &str,
    password: &str,
    action: Option<&str>,
) -> Result<Url, XtreamError> {
    let mut url = base(server)?;
    url.path_segments_mut()
        .map_err(|_| XtreamError::InvalidUrl)?
        .pop_if_empty()
        .push("player_api.php");
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("username", username);
        query.append_pair("password", password);
        if let Some(action) = action {
            query.append_pair("action", action);
        }
    }
    Ok(url)
}

/// Playback URL of a live stream (MPEG-TS).
pub fn stream_url(
    server: &str,
    username: &str,
    password: &str,
    stream_id: &str,
) -> Result<String, XtreamError> {
    let mut url = base(server)?;
    let file = format!("{stream_id}.ts");
    url.path_segments_mut()
        .map_err(|_| XtreamError::InvalidUrl)?
        .pop_if_empty()
        .extend(["live", username, password, file.as_str()]);
    Ok(url.into())
}

pub async fn account(
    client: &reqwest::Client,
    server: &str,
    username: &str,
    password: &str,
) -> Result<XtreamAccount, XtreamError> {
    let url = api_url(server, username, password, None)?;
    let value: Value = http::fetch_json(client, url.as_str()).await?;
    parse_account(&value)
}

pub async fn live_streams(
    client: &reqwest::Client,
    server: &str,
    username: &str,
    password: &str,
) -> Result<Vec<XtreamStream>, XtreamError> {
    let categories: Value = http::fetch_json(
        client,
        api_url(server, username, password, Some("get_live_categories"))?.as_str(),
    )
    .await
    .unwrap_or(Value::Null);
    let streams: Value = http::fetch_json(
        client,
        api_url(server, username, password, Some("get_live_streams"))?.as_str(),
    )
    .await?;
    if !streams.is_array() {
        return Err(XtreamError::Format("stream list is not an array"));
    }
    Ok(parse_streams(&streams, &categories))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn account_with_string_numbers() {
        let account = parse_account(&json!({
            "user_info": { "auth": 1, "status": "Active", "max_connections": "2" },
            "server_info": { "url": "example.com" }
        }))
        .unwrap();
        assert_eq!(account.max_connections, Some(2));
        assert_eq!(account.status.as_deref(), Some("Active"));
        assert!(matches!(
            parse_account(&json!({ "user_info": { "auth": 0 } })),
            Err(XtreamError::Unauthorized)
        ));
    }

    #[test]
    fn streams_with_categories() {
        let streams = parse_streams(
            &json!([
                { "num": 1, "name": "Das Erste HD", "stream_id": 101, "stream_icon": "http://i/1.png",
                  "epg_channel_id": "daserste.de", "category_id": "5" },
                { "num": "2", "name": "ZDF", "stream_id": "102", "stream_icon": "", "epg_channel_id": null,
                  "category_id": 9 },
                { "name": "ohne id" }
            ]),
            &json!([{ "category_id": "5", "category_name": "DE | Vollprogramm" }]),
        );
        assert_eq!(streams.len(), 2);
        assert_eq!(streams[0].stream_id, "101");
        assert_eq!(streams[0].num.as_deref(), Some("1"));
        assert_eq!(streams[0].category.as_deref(), Some("DE | Vollprogramm"));
        assert_eq!(streams[0].epg_channel_id.as_deref(), Some("daserste.de"));
        assert_eq!(streams[1].icon, None);
        assert_eq!(streams[1].category, None);
    }

    #[test]
    fn urls_encode_credentials() {
        let url = stream_url("http://tv.example:8080/", "max", "p@ss/wort", "101").unwrap();
        assert_eq!(url, "http://tv.example:8080/live/max/p@ss%2Fwort/101.ts");
        let api = api_url(
            "http://tv.example:8080/player_api.php",
            "max",
            "a&b",
            Some("get_live_streams"),
        )
        .unwrap();
        assert_eq!(
            api.as_str(),
            "http://tv.example:8080/player_api.php?username=max&password=a%26b&action=get_live_streams"
        );
        assert!(stream_url("ftp://x", "a", "b", "1").is_err());
    }
}
