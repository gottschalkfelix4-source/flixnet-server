//! Kodi-style `.nfo` files (`movie.nfo`, `tvshow.nfo`, `<episode>.nfo`).
//!
//! Real-world NFOs are messy (a URL after the root element, HTML entities, unknown
//! tags), so this is a tolerant event-based reader that builds a small tree and
//! picks the fields Flixnet understands.

use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::events::Event;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NfoActor {
    pub name: String,
    pub role: Option<String>,
    pub order: Option<i64>,
    pub thumb: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Nfo {
    /// Root element: movie | tvshow | episodedetails | season.
    pub kind: String,
    pub title: Option<String>,
    pub original_title: Option<String>,
    pub sort_title: Option<String>,
    pub year: Option<i64>,
    pub plot: Option<String>,
    pub tagline: Option<String>,
    pub runtime_minutes: Option<i64>,
    pub mpaa: Option<String>,
    pub rating: Option<f64>,
    /// YYYY-MM-DD
    pub premiered: Option<String>,
    pub tmdb: Option<String>,
    pub imdb: Option<String>,
    pub tvdb: Option<String>,
    pub genres: Vec<String>,
    pub studios: Vec<String>,
    pub tags: Vec<String>,
    pub directors: Vec<String>,
    pub writers: Vec<String>,
    pub actors: Vec<NfoActor>,
    pub collection: Option<String>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    /// Poster/fanart URLs listed in the NFO (`<thumb>`, `<fanart><thumb>`).
    pub poster_urls: Vec<String>,
    pub fanart_urls: Vec<String>,
}

#[derive(Debug, Default)]
struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<Node>,
}

impl Node {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn child(&self, name: &str) -> Option<&Node> {
        self.children
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    }

    fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children
            .iter()
            .filter(move |c| c.name.eq_ignore_ascii_case(name))
    }

    /// Trimmed text of a child, `None` if missing or empty.
    fn text_of(&self, name: &str) -> Option<String> {
        self.child(name).and_then(|c| clean(&c.text))
    }
}

fn clean(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
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

/// Builds the element tree of the first root element; ignores anything after it.
fn tree(xml: &str) -> Option<Node> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    reader.config_mut().check_end_names = false;
    let mut stack: Vec<Node> = Vec::new();

    let start = |e: &quick_xml::events::BytesStart<'_>| Node {
        name: e.local_name().as_ref().to_lowercase(),
        attrs: e
            .attributes()
            .filter_map(Result::ok)
            .map(|a| {
                (
                    a.key.local_name().as_ref().to_owned(),
                    a.normalized_value(XmlVersion::Explicit1_0)
                        .map(|v| v.into_owned())
                        .unwrap_or_default(),
                )
            })
            .collect(),
        ..Default::default()
    };

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => stack.push(start(&e)),
            Ok(Event::Empty(e)) => {
                let node = start(&e);
                match stack.last_mut() {
                    Some(parent) => parent.children.push(node),
                    None => return Some(node),
                }
            }
            Ok(Event::Text(t)) => {
                if let Some(node) = stack.last_mut() {
                    node.text.push_str(&t.xml10_content());
                }
            }
            Ok(Event::CData(c)) => {
                if let Some(node) = stack.last_mut() {
                    node.text.push_str(&c.xml10_content());
                }
            }
            // Entities arrive as separate events: "Tom &amp; Jerry" = Text, Ref, Text.
            Ok(Event::GeneralRef(r)) => {
                if let Some(node) = stack.last_mut()
                    && let Some(resolved) = entity(&r.xml10_content())
                {
                    node.text.push_str(&resolved);
                }
            }
            Ok(Event::End(_)) => {
                let node = stack.pop()?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(node),
                    None => return Some(node), // root closed: ignore trailing URLs etc.
                }
            }
            Ok(Event::Eof) => {
                // Unclosed document: fold whatever we have into the root.
                while stack.len() > 1 {
                    let node = stack.pop()?;
                    stack.last_mut()?.children.push(node);
                }
                return stack.pop();
            }
            Ok(_) => {}
            Err(err) => {
                tracing::debug!(%err, "nfo is not well-formed; using what was read");
                while stack.len() > 1 {
                    let node = stack.pop()?;
                    stack.last_mut()?.children.push(node);
                }
                return stack.pop();
            }
        }
    }
}

fn parse_date(text: &str) -> Option<String> {
    let date = text.trim().get(..10)?;
    let bytes = date.as_bytes();
    (bytes[4] == b'-' && bytes[7] == b'-' && date.chars().filter(char::is_ascii_digit).count() == 8)
        .then(|| date.to_owned())
}

/// Parses an NFO; `None` if it contains no XML root at all (e.g. a bare URL).
pub fn parse(xml: &str) -> Option<Nfo> {
    let root = tree(xml)?;
    let mut nfo = Nfo {
        kind: root.name.clone(),
        title: root.text_of("title"),
        original_title: root.text_of("originaltitle"),
        sort_title: root.text_of("sorttitle"),
        year: root
            .text_of("year")
            .and_then(|y| y.parse().ok())
            .filter(|y| (1800..=2200).contains(y)),
        plot: root.text_of("plot").or_else(|| root.text_of("outline")),
        tagline: root.text_of("tagline"),
        runtime_minutes: root
            .text_of("runtime")
            .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok())
            .filter(|m| *m > 0),
        mpaa: root
            .text_of("mpaa")
            .or_else(|| root.text_of("certification")),
        premiered: ["premiered", "releasedate", "aired"]
            .iter()
            .find_map(|tag| root.text_of(tag))
            .and_then(|d| parse_date(&d)),
        season: root.text_of("season").and_then(|s| s.parse().ok()),
        episode: root.text_of("episode").and_then(|e| e.parse().ok()),
        ..Default::default()
    };

    // Ratings: <ratings><rating default="true"><value>7.9</value></rating></ratings>
    // or legacy <rating>7.9</rating>.
    nfo.rating = root
        .child("ratings")
        .and_then(|ratings| {
            let all: Vec<&Node> = ratings.children_named("rating").collect();
            all.iter()
                .find(|r| r.attr("default") == Some("true"))
                .or_else(|| all.first())
                .and_then(|r| r.text_of("value"))
        })
        .or_else(|| root.text_of("rating"))
        .and_then(|v| v.replace(',', ".").parse::<f64>().ok())
        .filter(|v| v.is_finite() && (0.0..=10.0).contains(v));

    // IDs: <uniqueid type="tmdb">603</uniqueid>, <tmdbid>, <imdbid>, <id>.
    for uid in root.children_named("uniqueid") {
        let Some(value) = clean(&uid.text) else {
            continue;
        };
        match uid.attr("type").map(str::to_ascii_lowercase).as_deref() {
            Some("tmdb") => nfo.tmdb = Some(value),
            Some("imdb") => nfo.imdb = Some(value),
            Some("tvdb") => nfo.tvdb = Some(value),
            _ => {}
        }
    }
    nfo.tmdb = nfo.tmdb.or_else(|| root.text_of("tmdbid"));
    nfo.tvdb = nfo.tvdb.or_else(|| root.text_of("tvdbid"));
    nfo.imdb = nfo.imdb.or_else(|| root.text_of("imdbid"));
    if let Some(id) = root.text_of("id") {
        if id.starts_with("tt") {
            nfo.imdb.get_or_insert(id);
        } else if id.bytes().all(|b| b.is_ascii_digit()) {
            match nfo.kind.as_str() {
                "movie" => nfo.tmdb.get_or_insert(id),
                _ => nfo.tvdb.get_or_insert(id),
            };
        }
    }

    let texts = |name: &str| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for node in root.children_named(name) {
            // "Action / Adventure" in one tag is common.
            for part in node.text.split(['/', '|']) {
                if let Some(value) = clean(part)
                    && !out.contains(&value)
                {
                    out.push(value);
                }
            }
        }
        out
    };
    nfo.genres = texts("genre");
    nfo.studios = texts("studio");
    nfo.tags = texts("tag");
    nfo.directors = texts("director");
    nfo.writers = texts("credits");
    if nfo.writers.is_empty() {
        nfo.writers = texts("writer");
    }

    nfo.actors = root
        .children_named("actor")
        .filter_map(|actor| {
            Some(NfoActor {
                name: actor.text_of("name")?,
                role: actor.text_of("role"),
                order: actor.text_of("order").and_then(|o| o.parse().ok()),
                thumb: actor.text_of("thumb"),
            })
        })
        .collect();

    nfo.collection = root
        .child("set")
        .and_then(|set| set.text_of("name").or_else(|| clean(&set.text)));

    for thumb in root.children_named("thumb") {
        let Some(url) = clean(&thumb.text) else {
            continue;
        };
        match thumb.attr("aspect") {
            Some("poster") | None => nfo.poster_urls.push(url),
            _ => {}
        }
    }
    if let Some(fanart) = root.child("fanart") {
        nfo.fanart_urls = fanart
            .children_named("thumb")
            .filter_map(|t| clean(&t.text))
            .collect();
    }
    Some(nfo)
}

/// Minimum age for a certification: FSK, MPAA and US TV ratings.
pub fn age_from_certification(cert: &str) -> Option<i64> {
    let c = cert.trim().to_ascii_uppercase();
    let c = c
        .trim_start_matches("DE:")
        .trim_start_matches("DE-")
        .trim_start_matches("DE/")
        .trim_start_matches("US:")
        .trim_start_matches("RATED ")
        .trim();
    if let Some(rest) = c.strip_prefix("FSK") {
        return rest
            .trim_matches(|ch: char| !ch.is_ascii_digit())
            .parse()
            .ok();
    }
    Some(match c {
        "0" | "6" | "12" | "16" | "18" => c.parse().ok()?,
        "G" | "TV-Y" | "TV-G" | "U" => 0,
        "PG" | "TV-Y7" | "TV-PG" => 6,
        "PG-13" | "TV-14" | "12A" => 12,
        "R" | "TV-MA" | "15" => 16,
        "NC-17" | "X" => 18,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOVIE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes" ?>
<movie>
  <title>Der Pate</title>
  <originaltitle>The Godfather</originaltitle>
  <sorttitle>Pate</sorttitle>
  <ratings>
    <rating name="imdb" max="10"><value>9.2</value></rating>
    <rating name="themoviedb" max="10" default="true"><value>8.7</value></rating>
  </ratings>
  <year>1972</year>
  <plot>Don Vito Corleone &amp; seine Familie…</plot>
  <tagline>Ein Angebot, das man nicht ablehnen kann.</tagline>
  <runtime>175</runtime>
  <thumb aspect="poster">https://image.tmdb.org/t/p/original/poster.jpg</thumb>
  <fanart><thumb>https://image.tmdb.org/t/p/original/fanart.jpg</thumb></fanart>
  <mpaa>FSK 16</mpaa>
  <uniqueid type="imdb">tt0068646</uniqueid>
  <uniqueid type="tmdb" default="true">238</uniqueid>
  <genre>Drama</genre>
  <genre>Krimi / Thriller</genre>
  <set><name>Der Pate Filmreihe</name></set>
  <studio>Paramount</studio>
  <credits>Mario Puzo</credits>
  <director>Francis Ford Coppola</director>
  <premiered>1972-03-14</premiered>
  <actor><name>Marlon Brando</name><role>Don Vito Corleone</role><order>0</order><thumb>http://x/brando.jpg</thumb></actor>
  <actor><name>Al Pacino</name><role>Michael Corleone</role><order>1</order></actor>
</movie>
https://www.themoviedb.org/movie/238"#;

    #[test]
    fn parses_a_full_movie_nfo() {
        let nfo = parse(MOVIE).unwrap();
        assert_eq!(nfo.kind, "movie");
        assert_eq!(nfo.title.as_deref(), Some("Der Pate"));
        assert_eq!(nfo.original_title.as_deref(), Some("The Godfather"));
        assert_eq!(nfo.year, Some(1972));
        assert_eq!(nfo.rating, Some(8.7), "default rating wins");
        assert_eq!(
            nfo.plot.as_deref(),
            Some("Don Vito Corleone & seine Familie…")
        );
        assert_eq!(nfo.runtime_minutes, Some(175));
        assert_eq!(nfo.mpaa.as_deref(), Some("FSK 16"));
        assert_eq!(
            (nfo.tmdb.as_deref(), nfo.imdb.as_deref()),
            (Some("238"), Some("tt0068646"))
        );
        assert_eq!(nfo.genres, ["Drama", "Krimi", "Thriller"]);
        assert_eq!(nfo.collection.as_deref(), Some("Der Pate Filmreihe"));
        assert_eq!(nfo.directors, ["Francis Ford Coppola"]);
        assert_eq!(nfo.writers, ["Mario Puzo"]);
        assert_eq!(nfo.premiered.as_deref(), Some("1972-03-14"));
        assert_eq!(nfo.actors.len(), 2);
        assert_eq!(nfo.actors[0].role.as_deref(), Some("Don Vito Corleone"));
        assert_eq!(nfo.actors[0].thumb.as_deref(), Some("http://x/brando.jpg"));
        assert_eq!(nfo.poster_urls.len(), 1);
        assert_eq!(nfo.fanart_urls.len(), 1);
    }

    #[test]
    fn tolerates_legacy_and_broken_files() {
        let nfo = parse("<tvshow><title>Dark</title><id>334824</id><rating>8,8</rating><premiered>2017-12-01T00:00</premiered>").unwrap();
        assert_eq!(nfo.kind, "tvshow");
        assert_eq!(nfo.tvdb.as_deref(), Some("334824"));
        assert_eq!(nfo.rating, Some(8.8));
        assert_eq!(nfo.premiered.as_deref(), Some("2017-12-01"));

        let nfo = parse("<episodedetails><title>Pilot</title><season>1</season><episode>1</episode><aired>2008-01-20</aired></episodedetails>").unwrap();
        assert_eq!((nfo.season, nfo.episode), (Some(1), Some(1)));

        assert!(parse("https://www.themoviedb.org/movie/238").is_none());
        let nfo = parse("<movie><title>Ohne Ende").unwrap();
        assert_eq!(nfo.title.as_deref(), Some("Ohne Ende"));
    }

    #[test]
    fn certifications_map_to_ages() {
        assert_eq!(age_from_certification("FSK 12"), Some(12));
        assert_eq!(age_from_certification("de/16"), Some(16));
        assert_eq!(age_from_certification("DE:FSK18"), Some(18));
        assert_eq!(age_from_certification("PG-13"), Some(12));
        assert_eq!(age_from_certification("Rated R"), Some(16));
        assert_eq!(age_from_certification("TV-MA"), Some(16));
        assert_eq!(age_from_certification("G"), Some(0));
        assert_eq!(age_from_certification("Unbekannt"), None);
    }
}
