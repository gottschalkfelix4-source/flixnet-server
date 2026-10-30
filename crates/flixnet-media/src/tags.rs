//! Descriptive tags of a file (ID3, MP4 atoms, Vorbis comments) as ffprobe reports
//! them: title, album, artist, composer, …

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;

use serde::Deserialize;

use crate::Ffprobe;
use crate::probe::ProbeError;
use crate::process::command;

/// Tag name (lowercase) → value. Container tags win over stream tags (Ogg and Opus
/// keep theirs on the audio stream).
pub type Tags = BTreeMap<String, String>;

#[derive(Deserialize)]
struct RawTags {
    #[serde(default)]
    format: Option<RawTagged>,
    #[serde(default)]
    streams: Vec<RawStream>,
}

#[derive(Deserialize, Default)]
struct RawTagged {
    #[serde(default)]
    tags: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct RawStream {
    codec_type: Option<String>,
    #[serde(default)]
    tags: BTreeMap<String, String>,
}

/// Parses `ffprobe -show_format -show_streams` JSON into tags.
pub fn parse_tags(json: &[u8]) -> Result<Tags, ProbeError> {
    let raw: RawTags = serde_json::from_slice(json)?;
    let mut tags = Tags::new();
    let audio = raw
        .streams
        .into_iter()
        .find(|s| s.codec_type.as_deref() == Some("audio"));
    let sources = [
        raw.format.map(|f| f.tags).unwrap_or_default(),
        audio.map(|a| a.tags).unwrap_or_default(),
    ];
    for source in sources {
        for (key, value) in source {
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            tags.entry(key.to_ascii_lowercase())
                .or_insert_with(|| value.to_owned());
        }
    }
    Ok(tags)
}

impl Ffprobe {
    /// The file's tags (container and first audio stream).
    pub async fn tags(&self, file: &Path) -> Result<Tags, ProbeError> {
        let mut cmd = command(&self.path);
        cmd.args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            "-i",
        ])
        .arg(file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let child = cmd.spawn().map_err(|source| ProbeError::Spawn {
            path: self.path.clone(),
            source,
        })?;
        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| ProbeError::Timeout(self.timeout))?
            .map_err(|source| ProbeError::Spawn {
                path: self.path.clone(),
                source,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = stderr.lines().last().unwrap_or("unknown error").trim();
            return Err(ProbeError::Failed(message.to_owned()));
        }
        parse_tags(&output.stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_tags_win_and_keys_are_lowercase() {
        let json = br#"{
            "streams": [
                {"codec_type": "video", "tags": {"title": "Cover"}},
                {"codec_type": "audio", "tags": {"ARTIST": "Stream-Autor", "LANGUAGE": "deu", "comment": " "}}
            ],
            "format": {"tags": {"album": "Das Buch", "Artist": "Anna Autorin", "composer": "Nils Sprecher"}}
        }"#;
        let tags = parse_tags(json).unwrap();
        assert_eq!(tags["album"], "Das Buch");
        assert_eq!(tags["artist"], "Anna Autorin");
        assert_eq!(tags["composer"], "Nils Sprecher");
        assert_eq!(tags["language"], "deu");
        assert!(!tags.contains_key("comment"), "blank values are dropped");
        assert!(!tags.contains_key("title"), "video stream tags are ignored");
        assert!(parse_tags(b"{}").unwrap().is_empty());
    }
}
