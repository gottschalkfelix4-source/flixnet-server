//! Release tags: resolution, source, codecs, HDR, languages, edition, group.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    BluRay,
    Remux,
    WebDl,
    WebRip,
    Hdtv,
    Dvd,
    Vhs,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReleaseTags {
    /// Vertical resolution: 480, 576, 720, 1080, 2160.
    pub resolution: Option<u16>,
    pub source: Option<Source>,
    /// Normalized: h264, h265, av1, vp9, xvid, mpeg2.
    pub video_codec: Option<&'static str>,
    /// Normalized: aac, ac3, eac3, dts, dts-hd, truehd, flac, opus, mp3.
    pub audio_codec: Option<&'static str>,
    pub audio_channels: Option<&'static str>,
    pub atmos: bool,
    /// hdr10, hdr10+, dv, hlg
    pub hdr: Vec<&'static str>,
    /// ISO 639-2 codes; "DL" (dual language) means German + original.
    pub languages: Vec<&'static str>,
    pub dual_language: bool,
    pub edition: Option<String>,
    pub is_3d: bool,
    pub proper: bool,
    pub release_group: Option<String>,
}

/// One classified token.
enum Tag {
    Resolution(u16),
    Source(Source),
    Video(&'static str),
    Audio(&'static str),
    Channels(&'static str),
    Atmos,
    Hdr(&'static str),
    Lang(&'static str),
    DualLanguage,
    Edition(&'static str),
    ThreeD,
    Proper,
    /// Known release noise without meaning for us (e.g. "internal", "complete").
    Noise,
}

fn classify(token: &str) -> Option<Tag> {
    let t = token.to_ascii_lowercase();
    let t = t.trim_matches(|c: char| matches!(c, '-' | '+' | ',' | '.'));
    // Strip a trailing release group: "x264-GRP" → "x264".
    let core = t.split('-').next().unwrap_or(t);
    Some(match core {
        "480p" | "480i" => Tag::Resolution(480),
        "576p" | "576i" => Tag::Resolution(576),
        "720p" | "720i" => Tag::Resolution(720),
        "1080p" | "1080i" => Tag::Resolution(1080),
        "2160p" | "4k" | "uhd" => Tag::Resolution(2160),
        "bluray" | "blu" | "bdrip" | "brrip" | "bd" | "bd25" | "bd50" => {
            Tag::Source(Source::BluRay)
        }
        "remux" => Tag::Source(Source::Remux),
        "web" | "webdl" | "amzn" | "nf" | "dsnp" | "atvp" | "hmax" => Tag::Source(Source::WebDl),
        "webrip" => Tag::Source(Source::WebRip),
        "hdtv" | "pdtv" | "dvb" | "sat" => Tag::Source(Source::Hdtv),
        "dvd" | "dvdrip" | "dvdr" | "dvd9" | "dvd5" => Tag::Source(Source::Dvd),
        "vhs" | "vhsrip" => Tag::Source(Source::Vhs),
        "x264" | "h264" | "avc" => Tag::Video("h264"),
        "x265" | "h265" | "hevc" => Tag::Video("h265"),
        "av1" => Tag::Video("av1"),
        "vp9" => Tag::Video("vp9"),
        "xvid" | "divx" => Tag::Video("xvid"),
        "mpeg2" | "mpeg" => Tag::Video("mpeg2"),
        "aac" => Tag::Audio("aac"),
        "ac3" | "dd" => Tag::Audio("ac3"),
        "eac3" | "ddp" | "dd+" => Tag::Audio("eac3"),
        "dts" => Tag::Audio("dts"),
        "dtshd" | "dts-hd" | "dtsma" | "dts-x" | "dtsx" => Tag::Audio("dts-hd"),
        "truehd" => Tag::Audio("truehd"),
        "flac" => Tag::Audio("flac"),
        "opus" => Tag::Audio("opus"),
        "mp3" => Tag::Audio("mp3"),
        "atmos" => Tag::Atmos,
        "hdr" | "hdr10" => Tag::Hdr("hdr10"),
        "hdr10+" | "hdr10plus" => Tag::Hdr("hdr10+"),
        "dv" | "dovi" | "dolbyvision" => Tag::Hdr("dv"),
        "hlg" => Tag::Hdr("hlg"),
        "german" | "deutsch" | "ger" | "gerdub" => Tag::Lang("deu"),
        "english" | "eng" => Tag::Lang("eng"),
        "french" | "fre" | "vff" | "truefrench" => Tag::Lang("fra"),
        "spanish" | "spa" => Tag::Lang("spa"),
        "italian" | "ita" => Tag::Lang("ita"),
        "japanese" | "jap" | "jpn" => Tag::Lang("jpn"),
        "dl" | "ml" | "multi" | "dual" => Tag::DualLanguage,
        "extended" => Tag::Edition("Extended"),
        "uncut" => Tag::Edition("Uncut"),
        "unrated" => Tag::Edition("Unrated"),
        "theatrical" => Tag::Edition("Theatrical"),
        "remastered" => Tag::Edition("Remastered"),
        "imax" => Tag::Edition("IMAX"),
        "dc" | "directors" => Tag::Edition("Director's Cut"),
        "3d" | "hsbs" | "hou" | "sbs" => Tag::ThreeD,
        "proper" | "repack" | "rerip" => Tag::Proper,
        "internal" | "complete" | "limited" | "subbed" | "dubbed" | "ac3d" | "ld" | "md" | "ws"
        | "custom" | "readnfo" | "festival" | "dsnp+" => Tag::Noise,
        _ => {
            // Channel layouts: "5·1", "dd5·1", "ddp5·1", "aac2·0", "7·1"
            let channels = |s: &str| match s {
                "1·0" => Some("1.0"),
                "2·0" => Some("2.0"),
                "5·1" => Some("5.1"),
                "6·1" => Some("6.1"),
                "7·1" => Some("7.1"),
                _ => None,
            };
            if let Some(ch) = channels(core) {
                return Some(Tag::Channels(ch));
            }
            for (prefix, codec) in [
                ("ddp", "eac3"),
                ("dd+", "eac3"),
                ("eac3", "eac3"),
                ("dd", "ac3"),
                ("ac3", "ac3"),
                ("aac", "aac"),
                ("dts", "dts"),
                ("truehd", "truehd"),
                ("flac", "flac"),
                ("opus", "opus"),
            ] {
                if let Some(rest) = core.strip_prefix(prefix)
                    && let Some(ch) = channels(rest)
                {
                    // Represent as audio; channel layout is secondary detail.
                    let _ = ch;
                    return Some(Tag::Audio(codec));
                }
            }
            return None;
        }
    })
}

/// Resolution, source, codecs, HDR, 3D: tags that only ever follow the year.
pub fn is_technical_tag(token: &str) -> bool {
    matches!(
        classify(token),
        Some(
            Tag::Resolution(_)
                | Tag::Source(_)
                | Tag::Video(_)
                | Tag::Audio(_)
                | Tag::Channels(_)
                | Tag::Hdr(_)
                | Tag::ThreeD
                | Tag::Atmos
        )
    ) || token.eq_ignore_ascii_case("web-dl")
}

/// True for tokens that end a title (resolution, source, codec, language …).
pub fn is_tag_token(token: &str) -> bool {
    classify(token).is_some() || token.eq_ignore_ascii_case("web-dl")
}

/// Parses every tag in a space-separated text; unknown words are ignored.
pub fn parse(text: &str) -> ReleaseTags {
    let mut tags = ReleaseTags::default();
    let tokens: Vec<&str> = text.split(' ').filter(|t| !t.is_empty()).collect();

    // Release group: "-GRP" glued to the last token ("x264-FLX") or a lone last "-GRP".
    if let Some(last) = tokens.last()
        && let Some((_, group)) = last.rsplit_once('-')
        && !group.is_empty()
        && group.len() <= 20
        && group.chars().all(|c| c.is_ascii_alphanumeric())
        && classify(group).is_none()
    {
        tags.release_group = Some(group.to_owned());
    }

    let mut previous: Option<&str> = None;
    for token in &tokens {
        let lower = token.to_ascii_lowercase();
        if lower == "web-dl" {
            tags.source = Some(Source::WebDl);
        }
        // "Director's Cut", "Directors Cut"
        if lower == "cut"
            && previous.is_some_and(|p| p.to_ascii_lowercase().starts_with("director"))
        {
            tags.edition = Some("Director's Cut".to_owned());
        }
        // Channel layout glued to the codec: "DDP5.1", "DD2.0".
        if let Some(rest) = lower
            .strip_prefix("ddp")
            .or_else(|| lower.strip_prefix("dd"))
            && let Some(ch) = rest.split('-').next().and_then(|c| match c {
                "5·1" => Some("5.1"),
                "7·1" => Some("7.1"),
                "2·0" => Some("2.0"),
                _ => None,
            })
        {
            tags.audio_channels = Some(ch);
        }
        match classify(token) {
            Some(Tag::Resolution(r)) => tags.resolution = Some(r),
            Some(Tag::Source(s)) => {
                // Remux wins over BluRay; WebRip/WebDl keep the most specific.
                if tags.source != Some(Source::Remux) {
                    tags.source = Some(s);
                }
            }
            Some(Tag::Video(v)) => tags.video_codec = Some(v),
            Some(Tag::Audio(a)) => {
                if tags.audio_codec.is_none() || a == "truehd" || a == "dts-hd" {
                    tags.audio_codec = Some(a);
                }
            }
            Some(Tag::Channels(c)) => tags.audio_channels = Some(c),
            Some(Tag::Atmos) => tags.atmos = true,
            Some(Tag::Hdr(h)) => {
                if !tags.hdr.contains(&h) {
                    tags.hdr.push(h);
                }
            }
            Some(Tag::Lang(l)) => {
                if !tags.languages.contains(&l) {
                    tags.languages.push(l);
                }
            }
            Some(Tag::DualLanguage) => tags.dual_language = true,
            Some(Tag::Edition(e)) => {
                if tags.edition.is_none() {
                    tags.edition = Some(e.to_owned());
                }
            }
            Some(Tag::ThreeD) => tags.is_3d = true,
            Some(Tag::Proper) => tags.proper = true,
            Some(Tag::Noise) | None => {}
        }
        previous = Some(token);
    }
    // "German DL" means German plus the original language.
    if tags.dual_language && tags.languages.is_empty() {
        tags.languages.push("deu");
    }
    tags
}
