//! Streaming XMLTV parser.
//!
//! Guide files run to hundreds of megabytes, so they are read event by event and
//! every `<channel>` and `<programme>` is handed to a callback as soon as it closes;
//! memory stays flat. Gzip-compressed files are recognised by their magic bytes.
//!
//! Times look like `20261029201500 +0100` (seconds and offset optional; without an
//! offset UTC is assumed). Programs without `stop` end where the channel's next
//! program starts.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use flixnet_db::livetv::NewProgram;
use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};

#[derive(Debug, thiserror::Error)]
pub enum XmltvError {
    #[error("guide file unreadable: {0}")]
    Io(#[from] std::io::Error),
    #[error("guide file is not valid XML: {0}")]
    Xml(String),
}

/// A `<channel>`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct XmltvChannel {
    pub id: String,
    pub names: Vec<String>,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum XmltvItem {
    Channel(XmltvChannel),
    Program(NewProgram),
}

/// Unix milliseconds of an XMLTV time.
pub fn parse_time(text: &str) -> Option<i64> {
    let text = text.trim();
    let digits_end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let digits = &text[..digits_end];
    if digits.len() < 8 {
        return None;
    }
    let field = |range: std::ops::Range<usize>| -> Option<i64> {
        digits.get(range).map_or(Some(0), |s| s.parse().ok())
    };
    let year = field(0..4)?;
    let month = field(4..6)?;
    let day = field(6..8)?;
    let hour = field(8..10)?;
    let minute = field(10..12)?;
    let second = field(12..14)?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let offset_minutes = parse_offset(text[digits_end..].trim())?;
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_minutes * 60;
    Some(seconds * 1000)
}

/// `+0100`, `-05:30`, `Z`, `UTC`, `GMT` or nothing → minutes east of UTC.
fn parse_offset(text: &str) -> Option<i64> {
    if text.is_empty() || matches!(text.to_ascii_uppercase().as_str(), "Z" | "UTC" | "GMT") {
        return Some(0);
    }
    let (sign, rest) = match text.as_bytes()[0] {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    let digits: String = rest.chars().filter(char::is_ascii_digit).collect();
    if digits.len() != 4 || rest.chars().any(|c| !c.is_ascii_digit() && c != ':') {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = digits[2..].parse().ok()?;
    Some(sign * (hours * 60 + minutes))
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `xmltv_ns` episode numbers (`season.episode.part`, zero-based, each maybe `x/y`).
fn parse_xmltv_ns(text: &str) -> (Option<i64>, Option<i64>) {
    let mut parts = text.split('.');
    let number = |part: Option<&str>| -> Option<i64> {
        let value = part?.split('/').next()?.trim();
        value.parse::<i64>().ok().map(|n| n + 1)
    };
    let season = number(parts.next());
    let episode = number(parts.next());
    (season, episode)
}

/// `S01E05`, `S1 E5`, `1x05` → (season, episode).
fn parse_onscreen(text: &str) -> (Option<i64>, Option<i64>) {
    let upper = text.to_ascii_uppercase();
    let number_after = |marker: char| -> Option<i64> {
        let start = upper.find(marker)? + 1;
        let digits: String = upper[start..]
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    };
    if upper.contains('S') && upper.contains('E') {
        return (number_after('S'), number_after('E'));
    }
    if let Some((season, episode)) = upper.split_once('X')
        && let (Ok(s), Ok(e)) = (season.trim().parse(), episode.trim().parse())
    {
        return (Some(s), Some(e));
    }
    (None, None)
}

fn entity(name: &str) -> Option<String> {
    Some(match name {
        "amp" => "&".into(),
        "lt" => "<".into(),
        "gt" => ">".into(),
        "quot" => "\"".into(),
        "apos" => "'".into(),
        "nbsp" => "\u{a0}".into(),
        _ => {
            let code = name.strip_prefix('#')?;
            let value = match code.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => code.parse().ok()?,
            };
            char::from_u32(value)?.to_string()
        }
    })
}

fn attr(e: &BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes()
        .filter_map(Result::ok)
        .find(|a| a.key.local_name().as_ref() == name)
        .and_then(|a| {
            a.normalized_value(XmlVersion::Explicit1_0)
                .ok()
                .map(|v| v.trim().to_owned())
        })
        .filter(|v| !v.is_empty())
}

/// The element whose text is being collected.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Field {
    DisplayName,
    Title,
    SubTitle,
    Desc,
    Category,
    EpisodeNum,
    Date,
    RatingValue,
}

#[derive(Default)]
struct ProgramDraft {
    program: NewProgram,
    stop: Option<i64>,
    titles: Vec<(Option<String>, String)>,
    onscreen: Option<String>,
    rating_system: Option<String>,
}

/// Preferred language of titles and descriptions when a guide offers several.
const PREFERRED_LANG: &str = "de";

fn pick(values: &[(Option<String>, String)]) -> Option<String> {
    values
        .iter()
        .find(|(lang, _)| lang.as_deref() == Some(PREFERRED_LANG))
        .or_else(|| values.first())
        .map(|(_, v)| v.clone())
}

/// Opens a guide file, decompressing gzip transparently.
pub fn open(path: &Path) -> std::io::Result<Box<dyn BufRead + Send>> {
    let mut file = BufReader::with_capacity(256 * 1024, File::open(path)?);
    let gzip = file.fill_buf()?.starts_with(&[0x1f, 0x8b]);
    Ok(if gzip {
        Box::new(BufReader::with_capacity(
            256 * 1024,
            flate2::bufread::MultiGzDecoder::new(file),
        ))
    } else {
        Box::new(file)
    })
}

/// Reads a whole guide, calling `each` per channel and program (in file order;
/// programs without `stop` once their successor is known). Returns early with
/// `Ok` when `each` returns `false`.
pub fn parse<R: BufRead>(
    input: R,
    mut each: impl FnMut(XmltvItem) -> bool,
) -> Result<(), XmltvError> {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().trim_text(false);
    reader.config_mut().check_end_names = false;

    let mut buf = Vec::with_capacity(4096);
    let mut channel: Option<XmltvChannel> = None;
    let mut draft: Option<ProgramDraft> = None;
    let mut field: Option<(Field, Option<String>)> = None;
    let mut episode_system: Option<String> = None;
    let mut text = String::new();
    // Programs waiting for their successor to learn when they end.
    let mut open_ended: HashMap<String, NewProgram> = HashMap::new();
    let mut in_rating = false;

    // Emits a finished program, resolving open ends on its channel.
    let mut emit =
        |program: NewProgram, has_stop: bool, each: &mut dyn FnMut(XmltvItem) -> bool| -> bool {
            if let Some(mut previous) = open_ended.remove(&program.channel_id) {
                previous.end_ms = program.start_ms;
                if previous.end_ms > previous.start_ms && !each(XmltvItem::Program(previous)) {
                    return false;
                }
            }
            if has_stop {
                each(XmltvItem::Program(program))
            } else {
                open_ended.insert(program.channel_id.clone(), program);
                true
            }
        };

    loop {
        buf.clear();
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| XmltvError::Xml(e.to_string()))?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(event, Event::Empty(_));
                let name = e.local_name();
                match name.as_ref() {
                    "channel" if draft.is_none() => {
                        let Some(id) = attr(e, "id") else {
                            continue;
                        };
                        let new = XmltvChannel {
                            id,
                            ..Default::default()
                        };
                        if empty {
                            if !each(XmltvItem::Channel(new)) {
                                return Ok(());
                            }
                        } else {
                            channel = Some(new);
                        }
                    }
                    "programme" => {
                        let (Some(channel_id), Some(start)) = (
                            attr(e, "channel"),
                            attr(e, "start").as_deref().and_then(parse_time),
                        ) else {
                            continue;
                        };
                        let stop = attr(e, "stop").as_deref().and_then(parse_time);
                        if !empty {
                            draft = Some(ProgramDraft {
                                program: NewProgram {
                                    channel_id,
                                    start_ms: start,
                                    end_ms: stop.unwrap_or(start),
                                    ..Default::default()
                                },
                                stop,
                                ..Default::default()
                            });
                        }
                    }
                    "display-name" if channel.is_some() => {
                        field = Some((Field::DisplayName, None));
                    }
                    "icon" => {
                        let src = attr(e, "src");
                        if let Some(c) = channel.as_mut() {
                            c.icon = c.icon.take().or(src);
                        } else if let Some(d) = draft.as_mut() {
                            d.program.icon_url = d.program.icon_url.take().or(src);
                        }
                    }
                    "new" => {
                        if let Some(d) = draft.as_mut() {
                            d.program.is_new = true;
                        }
                    }
                    "rating" if draft.is_some() => {
                        in_rating = !empty;
                        if let Some(d) = draft.as_mut() {
                            d.rating_system = attr(e, "system");
                        }
                    }
                    other if draft.is_some() && !empty => {
                        let lang = attr(e, "lang");
                        field = match other {
                            "title" => Some((Field::Title, lang)),
                            "sub-title" => Some((Field::SubTitle, lang)),
                            "desc" => Some((Field::Desc, lang)),
                            "category" => Some((Field::Category, lang)),
                            "episode-num" => {
                                episode_system = attr(e, "system");
                                Some((Field::EpisodeNum, None))
                            }
                            "date" => Some((Field::Date, None)),
                            "value" if in_rating => Some((Field::RatingValue, None)),
                            _ => None,
                        };
                    }
                    _ => {}
                }
                text.clear();
            }
            Event::Text(t) => {
                if field.is_some() {
                    text.push_str(&t.xml10_content());
                }
            }
            Event::CData(c) => {
                if field.is_some() {
                    text.push_str(&c.xml10_content());
                }
            }
            Event::GeneralRef(r) => {
                if field.is_some()
                    && let Some(resolved) = entity(&r.xml10_content())
                {
                    text.push_str(&resolved);
                }
            }
            Event::End(e) => {
                let value = text.trim().to_owned();
                text.clear();
                match e.local_name().as_ref() {
                    "channel" => {
                        if let Some(done) = channel.take()
                            && !each(XmltvItem::Channel(done))
                        {
                            return Ok(());
                        }
                    }
                    "programme" => {
                        if let Some(d) = draft.take() {
                            let mut program = d.program;
                            let Some(title) = pick(&d.titles) else {
                                continue;
                            };
                            program.title = title;
                            if program.episode.is_none()
                                && let Some(onscreen) = &d.onscreen
                            {
                                let (season, episode) = parse_onscreen(onscreen);
                                program.season = program.season.or(season);
                                program.episode = episode;
                            }
                            program.episode_label =
                                d.onscreen
                                    .or_else(|| match (program.season, program.episode) {
                                        (Some(s), Some(e)) => Some(format!("S{s:02}E{e:02}")),
                                        (None, Some(e)) => Some(format!("Folge {e}")),
                                        _ => None,
                                    });
                            if program.rating.is_some()
                                && let Some(system) = d.rating_system
                                && !program.rating.as_deref().unwrap_or("").starts_with(&system)
                            {
                                program.rating = program.rating.map(|r| format!("{system} {r}"));
                            }
                            let has_stop = d.stop.is_some_and(|s| s > program.start_ms);
                            if !emit(program, has_stop, &mut each) {
                                return Ok(());
                            }
                        }
                    }
                    "rating" => in_rating = false,
                    _ => {
                        if let Some((kind, lang)) = field.take()
                            && !value.is_empty()
                        {
                            if let Some(c) = channel.as_mut() {
                                if kind == Field::DisplayName && !c.names.contains(&value) {
                                    c.names.push(value);
                                }
                            } else if let Some(d) = draft.as_mut() {
                                let p = &mut d.program;
                                match kind {
                                    Field::Title => d.titles.push((lang, value)),
                                    Field::SubTitle => {
                                        if p.subtitle.is_none()
                                            || lang.as_deref() == Some(PREFERRED_LANG)
                                        {
                                            p.subtitle = Some(value);
                                        }
                                    }
                                    Field::Desc => {
                                        if p.description.is_none()
                                            || lang.as_deref() == Some(PREFERRED_LANG)
                                        {
                                            p.description = Some(value);
                                        }
                                    }
                                    Field::Category => {
                                        if !p.categories.contains(&value) {
                                            p.categories.push(value);
                                        }
                                    }
                                    Field::EpisodeNum => match episode_system.as_deref() {
                                        Some("xmltv_ns") => {
                                            let (season, episode) = parse_xmltv_ns(&value);
                                            p.season = season;
                                            p.episode = episode;
                                        }
                                        Some("onscreen") | None => d.onscreen = Some(value),
                                        _ => {}
                                    },
                                    Field::Date => {
                                        p.year = value.get(..4).and_then(|y| y.parse().ok());
                                    }
                                    Field::RatingValue => p.rating = Some(value),
                                    Field::DisplayName => {}
                                }
                            }
                        }
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    // Last programs without an end: assume half an hour rather than dropping them.
    let mut rest: Vec<NewProgram> = open_ended.into_values().collect();
    rest.sort_by_key(|p| p.start_ms);
    for mut program in rest {
        program.end_ms = program.start_ms + 30 * 60 * 1000;
        if !each(XmltvItem::Program(program)) {
            break;
        }
    }
    Ok(())
}

/// Parses a guide held in memory (tests, small files).
pub fn parse_all(input: impl Read) -> Result<(Vec<XmltvChannel>, Vec<NewProgram>), XmltvError> {
    let mut channels = Vec::new();
    let mut programs = Vec::new();
    parse(BufReader::new(input), |item| {
        match item {
            XmltvItem::Channel(c) => channels.push(c),
            XmltvItem::Program(p) => programs.push(p),
        }
        true
    })?;
    Ok((channels, programs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_with_offsets() {
        // 2026-10-29 20:15 CET = 19:15 UTC.
        let utc = parse_time("20261029191500 +0000").unwrap();
        assert_eq!(utc, 1_793_301_300_000);
        assert_eq!(parse_time("20261029201500 +0100"), Some(utc));
        assert_eq!(parse_time("20261029141500 -0500"), Some(utc));
        assert_eq!(parse_time("20261029191500"), Some(utc), "no offset = UTC");
        assert_eq!(parse_time("202610291915 Z"), Some(utc), "seconds optional");
        assert_eq!(parse_time("20261029194500 +00:30"), Some(utc));
        assert_eq!(parse_time("19700101000000 +0000"), Some(0));
        // Leap day and a date before 1970.
        assert_eq!(parse_time("20240229000000"), Some(1_709_164_800_000));
        assert_eq!(parse_time("19691231235959 +0000"), Some(-1000));
        assert_eq!(parse_time("2026"), None);
        assert_eq!(parse_time("20261329000000"), None);
        assert_eq!(parse_time("20261029191500 CET"), None);
    }

    #[test]
    fn episode_numbers() {
        assert_eq!(parse_xmltv_ns("0.4."), (Some(1), Some(5)));
        assert_eq!(parse_xmltv_ns("2/5.11/22.0/1"), (Some(3), Some(12)));
        assert_eq!(parse_xmltv_ns(".7."), (None, Some(8)));
        assert_eq!(parse_onscreen("S01E05"), (Some(1), Some(5)));
        assert_eq!(parse_onscreen("S2 E13"), (Some(2), Some(13)));
        assert_eq!(parse_onscreen("3x07"), (Some(3), Some(7)));
    }

    const GUIDE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE tv SYSTEM "xmltv.dtd">
<tv generator-info-name="test">
  <channel id="daserste.de">
    <display-name lang="de">Das Erste</display-name>
    <display-name>ARD</display-name>
    <display-name>1</display-name>
    <icon src="http://logo.example/ard.png" />
  </channel>
  <channel id="zdf.de"><display-name>ZDF</display-name></channel>
  <programme start="20261029200000 +0100" stop="20261029201500 +0100" channel="daserste.de">
    <title lang="en">Daily News</title>
    <title lang="de">Tagesschau</title>
    <sub-title>Nachrichten &amp; Wetter</sub-title>
    <desc lang="de">Aktuelles aus aller Welt.</desc>
    <category lang="de">Nachrichten</category>
    <category lang="de">Nachrichten</category>
    <icon src="http://img.example/ts.jpg"/>
    <new />
  </programme>
  <programme start="20261029201500 +0100" channel="daserste.de">
    <title>Tatort</title>
    <episode-num system="xmltv_ns">54.1199.</episode-num>
    <date>2026</date>
    <rating system="FSK"><value>12</value></rating>
  </programme>
  <programme start="20261029214500 +0100" stop="20261029224500 +0100" channel="daserste.de">
    <title><![CDATA[Hart aber <fair>]]></title>
    <episode-num system="onscreen">S03E07</episode-num>
  </programme>
  <programme start="20261029200000 +0100" channel="zdf.de">
    <title>heute journal</title>
  </programme>
  <programme start="20261029200000 +0100" stop="20261029210000 +0100" channel="zdf.de"/>
</tv>
"#;

    #[test]
    fn parses_channels_and_programs() {
        let (channels, programs) = parse_all(GUIDE.as_bytes()).unwrap();
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0].id, "daserste.de");
        assert_eq!(channels[0].names, ["Das Erste", "ARD", "1"]);
        assert_eq!(
            channels[0].icon.as_deref(),
            Some("http://logo.example/ard.png")
        );

        let at =
            |h: i64, m: i64| parse_time("20261029000000 +0100").unwrap() + (h * 60 + m) * 60_000;
        let titles: Vec<_> = programs.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(
            titles,
            ["Tagesschau", "Tatort", "Hart aber <fair>", "heute journal"]
        );

        let news = &programs[0];
        assert_eq!((news.start_ms, news.end_ms), (at(20, 0), at(20, 15)));
        assert_eq!(news.subtitle.as_deref(), Some("Nachrichten & Wetter"));
        assert_eq!(
            news.description.as_deref(),
            Some("Aktuelles aus aller Welt.")
        );
        assert_eq!(news.categories, ["Nachrichten"]);
        assert_eq!(news.icon_url.as_deref(), Some("http://img.example/ts.jpg"));
        assert!(news.is_new);

        let tatort = &programs[1];
        assert_eq!(
            tatort.end_ms,
            at(21, 45),
            "ends when the next program starts"
        );
        assert_eq!((tatort.season, tatort.episode), (Some(55), Some(1200)));
        assert_eq!(tatort.episode_label.as_deref(), Some("S55E1200"));
        assert_eq!(tatort.year, Some(2026));
        assert_eq!(tatort.rating.as_deref(), Some("FSK 12"));
        assert!(!tatort.is_new);

        let talk = &programs[2];
        assert_eq!((talk.season, talk.episode), (Some(3), Some(7)));
        assert_eq!(talk.episode_label.as_deref(), Some("S03E07"));

        let last = &programs[3];
        assert_eq!(last.channel_id, "zdf.de");
        assert_eq!(last.end_ms, at(20, 30), "open end at EOF: half an hour");
    }

    #[test]
    fn gzip_files_are_detected() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("guide.xml.gz");
        let mut encoder = flate2::write::GzEncoder::new(
            File::create(&path).unwrap(),
            flate2::Compression::fast(),
        );
        encoder.write_all(GUIDE.as_bytes()).unwrap();
        encoder.finish().unwrap();

        let mut count = 0;
        parse(open(&path).unwrap(), |item| {
            count += usize::from(matches!(item, XmltvItem::Program(_)));
            true
        })
        .unwrap();
        assert_eq!(count, 4);

        let plain = dir.path().join("guide.xml");
        std::fs::write(&plain, GUIDE).unwrap();
        let (_, programs) = parse_all(open(&plain).unwrap()).unwrap();
        assert_eq!(programs.len(), 4);
    }

    #[test]
    fn broken_xml_is_an_error() {
        assert!(parse_all("<tv><programme start=\"2026".as_bytes()).is_err());
    }
}
