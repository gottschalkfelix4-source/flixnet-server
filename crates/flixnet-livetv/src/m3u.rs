//! Extended M3U playlists as IPTV providers publish them:
//!
//! ```text
//! #EXTM3U url-tvg="http://example.com/guide.xml.gz"
//! #EXTINF:-1 tvg-id="daserste.de" tvg-logo="http://…/ard.png" group-title="Vollprogramm",Das Erste HD
//! #EXTVLCOPT:http-user-agent=Kodi/21
//! http://example.com/live/1.ts
//! ```
//!
//! Tolerant by design: unknown tags are skipped, attribute values may contain commas
//! and the channel name is whatever follows the first comma outside quotes.

/// One `#EXTINF` entry with its stream URL.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct M3uEntry {
    pub name: String,
    pub url: String,
    pub tvg_id: Option<String>,
    pub tvg_name: Option<String>,
    pub tvg_logo: Option<String>,
    /// `tvg-chno` / `channel-number`
    pub number: Option<String>,
    /// `group-title`, else a preceding `#EXTGRP`
    pub group: Option<String>,
    /// `#EXTVLCOPT:http-user-agent=…`
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct M3uPlaylist {
    /// Guide URLs announced in the header (`url-tvg`, `x-tvg-url`), comma-separated
    /// lists split up.
    pub guide_urls: Vec<String>,
    pub entries: Vec<M3uEntry>,
}

/// `key="value"` pairs (also unquoted `key=value`) up to the first comma outside
/// quotes; returns the attributes and the rest after that comma.
fn attributes(text: &str) -> (Vec<(String, String)>, &str) {
    let bytes = text.as_bytes();
    let mut attrs = Vec::new();
    let mut i = 0;
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            return (attrs, "");
        }
        if bytes[i] == b',' {
            return (attrs, &text[i + 1..]);
        }
        let key_start = i;
        while i < bytes.len() && !matches!(bytes[i], b'=' | b',' | b' ' | b'\t') {
            i += 1;
        }
        let key = text[key_start..i].to_ascii_lowercase();
        if i >= bytes.len() || bytes[i] != b'=' {
            // A bare word (e.g. the duration) without a value.
            continue;
        }
        i += 1; // '='
        let value = if bytes.get(i) == Some(&b'"') {
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] != b'"' {
                i += 1;
            }
            let value = &text[start..i];
            i = (i + 1).min(bytes.len());
            value
        } else {
            let start = i;
            while i < bytes.len() && !matches!(bytes[i], b',' | b' ' | b'\t') {
                i += 1;
            }
            &text[start..i]
        };
        attrs.push((key, value.trim().to_owned()));
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

pub fn parse(text: &str) -> M3uPlaylist {
    let mut playlist = M3uPlaylist::default();
    let mut pending: Option<M3uEntry> = None;
    let mut group: Option<String> = None;
    let mut user_agent: Option<String> = None;

    // A byte order mark sometimes precedes `#EXTM3U`.
    let text = text.trim_start_matches('\u{feff}');
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(header) = line.strip_prefix("#EXTM3U") {
            let (attrs, _) = attributes(header);
            for (key, value) in attrs {
                if matches!(key.as_str(), "url-tvg" | "x-tvg-url" | "tvg-url") {
                    playlist.guide_urls.extend(
                        value
                            .split(',')
                            .map(str::trim)
                            .filter(|u| !u.is_empty())
                            .map(str::to_owned),
                    );
                }
            }
        } else if let Some(info) = line.strip_prefix("#EXTINF:") {
            let (attrs, name) = attributes(info);
            let get =
                |key: &str| non_empty(attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()));
            pending = Some(M3uEntry {
                name: name.trim().to_owned(),
                url: String::new(),
                tvg_id: get("tvg-id"),
                tvg_name: get("tvg-name"),
                tvg_logo: get("tvg-logo").or_else(|| get("logo")),
                number: get("tvg-chno").or_else(|| get("channel-number")),
                group: get("group-title"),
                user_agent: None,
            });
            group = None;
            user_agent = None;
        } else if let Some(name) = line.strip_prefix("#EXTGRP:") {
            group = non_empty(Some(name.trim().to_owned()));
        } else if let Some(option) = line.strip_prefix("#EXTVLCOPT:") {
            if let Some((key, value)) = option.split_once('=')
                && key.trim().eq_ignore_ascii_case("http-user-agent")
            {
                user_agent = non_empty(Some(value.trim().to_owned()));
            }
        } else if line.starts_with('#') {
            // Other tags (#EXTREM, #KODIPROP, …) are not needed.
        } else if let Some(mut entry) = pending.take() {
            entry.url = line.to_owned();
            if entry.group.is_none() {
                entry.group = group.take();
            }
            entry.user_agent = user_agent.take();
            if entry.name.is_empty() {
                entry.name = entry
                    .tvg_name
                    .clone()
                    .unwrap_or_else(|| format!("Sender {}", playlist.entries.len() + 1));
            }
            playlist.entries.push(entry);
        }
        // A URL without #EXTINF is not a channel we can name; skipped.
    }
    playlist
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAYLIST: &str = "\u{feff}#EXTM3U url-tvg=\"http://epg.example/guide.xml.gz, http://epg.example/b.xml\"
#EXTINF:-1 tvg-id=\"daserste.de\" tvg-name=\"Das Erste HD\" tvg-logo=\"http://logo.example/ard.png\" tvg-chno=\"1\" group-title=\"Vollprogramm, öffentlich\",Das Erste HD
http://iptv.example/live/1.ts

#EXTINF:-1 tvg-id=\"\" group-title=Sport,Sport1
#EXTVLCOPT:http-user-agent=Kodi/21.0
http://iptv.example/live/2.m3u8
#EXTINF:0,
#EXTGRP:Nachrichten
udp://@239.0.0.1:1234
http://iptv.example/orphan.ts
#EXTINF:-1 channel-number=\"7\" tvg-name=\"Arte\",ARTE, Kultur
rtp://239.0.0.7:5000
";

    #[test]
    fn parses_attributes_names_and_urls() {
        let playlist = parse(PLAYLIST);
        assert_eq!(
            playlist.guide_urls,
            [
                "http://epg.example/guide.xml.gz",
                "http://epg.example/b.xml"
            ]
        );
        assert_eq!(playlist.entries.len(), 4, "{:#?}", playlist.entries);

        let ard = &playlist.entries[0];
        assert_eq!(ard.name, "Das Erste HD");
        assert_eq!(ard.tvg_id.as_deref(), Some("daserste.de"));
        assert_eq!(ard.tvg_name.as_deref(), Some("Das Erste HD"));
        assert_eq!(ard.tvg_logo.as_deref(), Some("http://logo.example/ard.png"));
        assert_eq!(ard.number.as_deref(), Some("1"));
        assert_eq!(ard.group.as_deref(), Some("Vollprogramm, öffentlich"));
        assert_eq!(ard.url, "http://iptv.example/live/1.ts");

        let sport = &playlist.entries[1];
        assert_eq!(sport.name, "Sport1");
        assert_eq!(sport.tvg_id, None, "empty attributes are absent");
        assert_eq!(sport.group.as_deref(), Some("Sport"));
        assert_eq!(sport.user_agent.as_deref(), Some("Kodi/21.0"));

        let news = &playlist.entries[2];
        assert_eq!(news.name, "Sender 3", "nameless entries get a placeholder");
        assert_eq!(news.group.as_deref(), Some("Nachrichten"));
        assert_eq!(news.url, "udp://@239.0.0.1:1234");

        let arte = &playlist.entries[3];
        assert_eq!(
            arte.name, "ARTE, Kultur",
            "commas after the first stay in the name"
        );
        assert_eq!(arte.number.as_deref(), Some("7"));
    }

    #[test]
    fn windows_line_endings_and_missing_header() {
        let playlist = parse("#EXTINF:-1,Eins\r\nhttp://a/1\r\n#EXTINF:-1,Zwei\r\nhttp://a/2\r\n");
        assert_eq!(playlist.entries.len(), 2);
        assert_eq!(playlist.entries[1].name, "Zwei");
        assert_eq!(playlist.entries[1].url, "http://a/2");
    }
}
