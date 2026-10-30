//! DIDL-Lite: the XML that ContentDirectory `Browse` returns (escaped inside SOAP).

use std::fmt::Write;

/// Escapes text for XML content and attribute values.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Control characters are not allowed in XML 1.0.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// `H:MM:SS.mmm`, the duration format of `res@duration`.
pub fn duration(ms: i64) -> String {
    let ms = ms.max(0);
    format!(
        "{}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// MIME type of a media file by extension.
pub fn mime_type(extension: &str) -> &'static str {
    match extension.to_ascii_lowercase().as_str() {
        "mkv" => "video/x-matroska",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "ts" | "m2ts" | "mts" => "video/mp2t",
        "webm" => "video/webm",
        "mpg" | "mpeg" => "video/mpeg",
        "wmv" => "video/x-ms-wmv",
        "mp3" => "audio/mpeg",
        "m4a" | "m4b" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

/// `protocolInfo` of a streamed resource: plain HTTP, byte seeks allowed, streaming
/// and background transfer modes.
pub fn protocol_info(mime: &str) -> String {
    format!(
        "http-get:*:{mime}:DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000"
    )
}

/// A container (folder-like object) in a Browse result.
#[derive(Debug, Clone, PartialEq)]
pub struct Container {
    pub id: String,
    pub parent_id: String,
    pub title: String,
    /// e.g. `object.container.storageFolder`.
    pub class: &'static str,
    pub child_count: Option<i64>,
    pub album_art: Option<String>,
}

/// A playable resource of an item.
#[derive(Debug, Clone, PartialEq)]
pub struct Resource {
    pub url: String,
    pub mime: &'static str,
    pub size: Option<i64>,
    pub duration_ms: Option<i64>,
    pub resolution: Option<(i64, i64)>,
}

/// A playable object (film, episode, picture, audiobook part).
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: String,
    pub parent_id: String,
    pub title: String,
    /// e.g. `object.item.videoItem.movie`.
    pub class: &'static str,
    /// `YYYY-MM-DD` or `YYYY`.
    pub date: Option<String>,
    pub creator: Option<String>,
    pub album: Option<String>,
    pub description: Option<String>,
    pub album_art: Option<String>,
    pub resource: Option<Resource>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    Container(Container),
    Item(Item),
}

fn element(out: &mut String, name: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|v| !v.is_empty()) {
        let _ = write!(out, "<{name}>{}</{name}>", escape(value));
    }
}

/// A DIDL-Lite document with the given objects.
pub fn didl(objects: &[Object]) -> String {
    let mut out = String::from(
        r#"<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/" xmlns:dlna="urn:schemas-dlna-org:metadata-1-0/">"#,
    );
    for object in objects {
        match object {
            Object::Container(c) => {
                let _ = write!(
                    out,
                    r#"<container id="{}" parentID="{}" restricted="1" searchable="0""#,
                    escape(&c.id),
                    escape(&c.parent_id)
                );
                if let Some(count) = c.child_count {
                    let _ = write!(out, r#" childCount="{count}""#);
                }
                out.push('>');
                element(&mut out, "dc:title", Some(&c.title));
                element(&mut out, "upnp:class", Some(c.class));
                element(&mut out, "upnp:albumArtURI", c.album_art.as_deref());
                out.push_str("</container>");
            }
            Object::Item(i) => {
                let _ = write!(
                    out,
                    r#"<item id="{}" parentID="{}" restricted="1">"#,
                    escape(&i.id),
                    escape(&i.parent_id)
                );
                element(&mut out, "dc:title", Some(&i.title));
                element(&mut out, "upnp:class", Some(i.class));
                element(&mut out, "dc:date", i.date.as_deref());
                element(&mut out, "dc:creator", i.creator.as_deref());
                element(&mut out, "upnp:artist", i.creator.as_deref());
                element(&mut out, "upnp:album", i.album.as_deref());
                element(&mut out, "dc:description", i.description.as_deref());
                element(&mut out, "upnp:albumArtURI", i.album_art.as_deref());
                if let Some(res) = &i.resource {
                    let _ = write!(
                        out,
                        r#"<res protocolInfo="{}""#,
                        escape(&protocol_info(res.mime))
                    );
                    if let Some(size) = res.size {
                        let _ = write!(out, r#" size="{size}""#);
                    }
                    if let Some(ms) = res.duration_ms.filter(|d| *d > 0) {
                        let _ = write!(out, r#" duration="{}""#, duration(ms));
                    }
                    if let Some((w, h)) = res.resolution.filter(|(w, h)| *w > 0 && *h > 0) {
                        let _ = write!(out, r#" resolution="{w}x{h}""#);
                    }
                    let _ = write!(out, ">{}</res>", escape(&res.url));
                }
                out.push_str("</item>");
            }
        }
    }
    out.push_str("</DIDL-Lite>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_and_formats() {
        assert_eq!(
            escape(r#"Tom & Jerry <"Folge" 1>'"#),
            "Tom &amp; Jerry &lt;&quot;Folge&quot; 1&gt;&apos;"
        );
        assert_eq!(escape("a\u{1}b"), "ab");
        assert_eq!(duration(0), "0:00:00.000");
        assert_eq!(duration(7_384_005), "2:03:04.005");
        assert_eq!(mime_type("MKV"), "video/x-matroska");
        assert_eq!(mime_type("m4b"), "audio/mp4");
        assert_eq!(mime_type("xyz"), "application/octet-stream");
    }

    #[test]
    fn containers_and_items_render_as_didl_lite() {
        let xml = didl(&[
            Object::Container(Container {
                id: "lib:1".into(),
                parent_id: "0".into(),
                title: "Filme & Serien".into(),
                class: "object.container.storageFolder",
                child_count: Some(3),
                album_art: None,
            }),
            Object::Item(Item {
                id: "item:5".into(),
                parent_id: "lib:1".into(),
                title: "Matrix".into(),
                class: "object.item.videoItem.movie",
                date: Some("1999-03-31".into()),
                creator: None,
                album: None,
                description: Some("Neo <3".into()),
                album_art: Some("http://h/art/1?a=1&b=2".into()),
                resource: Some(Resource {
                    url: "http://192.168.1.2:8099/media/9?u=1&exp=2&sig=ab".into(),
                    mime: "video/x-matroska",
                    size: Some(1234),
                    duration_ms: Some(8_160_000),
                    resolution: Some((1920, 800)),
                }),
            }),
        ]);
        assert!(
            xml.starts_with("<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\"")
        );
        assert!(xml.contains(
            r#"<container id="lib:1" parentID="0" restricted="1" searchable="0" childCount="3"><dc:title>Filme &amp; Serien</dc:title><upnp:class>object.container.storageFolder</upnp:class></container>"#
        ), "{xml}");
        assert!(xml.contains(r#"<item id="item:5" parentID="lib:1" restricted="1"><dc:title>Matrix</dc:title><upnp:class>object.item.videoItem.movie</upnp:class><dc:date>1999-03-31</dc:date><dc:description>Neo &lt;3</dc:description><upnp:albumArtURI>http://h/art/1?a=1&amp;b=2</upnp:albumArtURI>"#), "{xml}");
        assert!(xml.contains(
            r#"<res protocolInfo="http-get:*:video/x-matroska:DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000" size="1234" duration="2:16:00.000" resolution="1920x800">http://192.168.1.2:8099/media/9?u=1&amp;exp=2&amp;sig=ab</res></item>"#
        ), "{xml}");
        assert!(xml.ends_with("</DIDL-Lite>"));
        // The document is well-formed XML.
        let mut reader = quick_xml::Reader::from_str(&xml);
        loop {
            match reader.read_event().expect("well-formed") {
                quick_xml::events::Event::Eof => break,
                _ => continue,
            }
        }
    }
}
