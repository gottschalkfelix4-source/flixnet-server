//! Milestone 2.1 gate: a large, deterministic table of real-world naming patterns.
//! Hand-written edge cases plus generated title × template combinations (300+ total).

use std::path::Path;

use flixnet_library::parse::{
    AirDate, ExtraKind, FileRole, Source, classify, parse_episode, parse_movie, subtitle_info,
};

// ---------------------------------------------------------------------------
// Movies: hand-written
// ---------------------------------------------------------------------------

fn movie(path: &str) -> flixnet_library::parse::ParsedMovie {
    parse_movie(Path::new(path))
}

#[track_caller]
fn assert_movie(path: &str, title: &str, year: Option<u16>) {
    let m = movie(path);
    assert_eq!((m.title.as_str(), m.year), (title, year), "path: {path}");
}

#[test]
fn movie_titles_and_years() {
    let cases: &[(&str, &str, Option<u16>)] = &[
        (
            "Testfilm Alpha (2021)/Testfilm.Alpha.2021.German.DL.1080p.WEB.H264-FLX.mp4",
            "Testfilm Alpha",
            Some(2021),
        ),
        (
            "The Matrix (1999)/The Matrix (1999).mkv",
            "The Matrix",
            Some(1999),
        ),
        (
            "The.Matrix.1999.1080p.BluRay.x264-GRP.mkv",
            "The Matrix",
            Some(1999),
        ),
        (
            "Der.Herr.der.Ringe.Die.Gefaehrten.2001.EXTENDED.German.DL.1080p.BluRay.x264-DETAiLS.mkv",
            "Der Herr der Ringe Die Gefaehrten",
            Some(2001),
        ),
        ("1917 (2019)/1917.2019.1080p.mkv", "1917", Some(2019)),
        ("1917.2019.1080p.BluRay.mkv", "1917", Some(2019)),
        (
            "2001.A.Space.Odyssey.1968.1080p.BluRay.mkv",
            "2001 A Space Odyssey",
            Some(1968),
        ),
        (
            "Blade Runner 2049 (2017)/Blade.Runner.2049.2017.2160p.WEB-DL.DDP5.1.Atmos.DV.HDR.H.265-FLUX.mkv",
            "Blade Runner 2049",
            Some(2017),
        ),
        (
            "Mission Impossible - Fallout (2018).mkv",
            "Mission Impossible - Fallout",
            Some(2018),
        ),
        (
            "Spider-Man.No.Way.Home.2021.1080p.WEBRip.x264-RARBG.mp4",
            "Spider-Man No Way Home",
            Some(2021),
        ),
        ("Up (2009)/Up.2009.mkv", "Up", Some(2009)),
        ("M (1931)/M.1931.mkv", "M", Some(1931)),
        (
            "Se7en (1995)/Se7en.1995.Remastered.mkv",
            "Se7en",
            Some(1995),
        ),
        (
            "Die Hard [1988]/Die Hard [1988].mkv",
            "Die Hard",
            Some(1988),
        ),
        ("no_year_movie_title.mkv", "no year movie title", None),
        ("Nosferatu.1922.mkv", "Nosferatu", Some(1922)),
        (
            "The 355 (2022)/The.355.2022.1080p.mkv",
            "The 355",
            Some(2022),
        ),
        (
            "Kill Bill Vol. 1 (2003)/Kill Bill Vol. 1 (2003).mkv",
            "Kill Bill Vol. 1",
            Some(2003),
        ),
        (
            "Mr. Nobody (2009)/Mr. Nobody (2009).mkv",
            "Mr. Nobody",
            Some(2009),
        ),
        (
            "Amélie (2001)/Die fabelhafte Welt der Amélie (2001).mkv",
            "Amélie",
            Some(2001),
        ),
        (
            "Leon.Der.Profi.1994.Uncut.German.AC3.DVDRip.XviD.avi",
            "Leon Der Profi",
            Some(1994),
        ),
        (
            "Star.Wars.Episode.IV.Eine.neue.Hoffnung.1977.German.DL.2160p.UHD.BluRay.HDR.x265-GRP.mkv",
            "Star Wars Episode IV Eine neue Hoffnung",
            Some(1977),
        ),
        (
            "Oppenheimer (2023)/Oppenheimer.2023.IMAX.1080p.mkv",
            "Oppenheimer",
            Some(2023),
        ),
        (
            "Dune Part Two (2024)/Dune.Part.Two.2024.1080p.mkv",
            "Dune Part Two",
            Some(2024),
        ),
        (
            "Joker (2019)/joker.2019.german.dl.dv.hdr.2160p.web.h265-grp.mkv",
            "Joker",
            Some(2019),
        ),
        (
            "The Lord of the Rings (2001)/Extras/Making Of.mkv",
            "The Lord of the Rings",
            Some(2001),
        ),
        (
            "Movie With No Year/Movie With No Year.mkv",
            "Movie With No Year",
            None,
        ),
        (
            "Paris, Texas (1984)/Paris, Texas (1984).mkv",
            "Paris, Texas",
            Some(1984),
        ),
        (
            "Ocean's Eleven (2001)/Ocean's.Eleven.2001.mkv",
            "Ocean's Eleven",
            Some(2001),
        ),
        ("WALL·E (2008)/WALL·E (2008).mkv", "WALL·E", Some(2008)),
        (
            "Fast & Furious 9 (2021)/Fast.and.Furious.9.2021.mkv",
            "Fast & Furious 9",
            Some(2021),
        ),
        (
            "Zurück in die Zukunft (1985)/Zurück.in.die.Zukunft.1985.German.mkv",
            "Zurück in die Zukunft",
            Some(1985),
        ),
        (
            "Das.Boot.1981.Directors.Cut.German.1080p.BluRay.x264.mkv",
            "Das Boot",
            Some(1981),
        ),
        (
            "The Thing (1982) (1080p BluRay x265 10bit Tigole).mkv",
            "The Thing",
            Some(1982),
        ),
        (
            "Interstellar (2014) - 2160p - HDR10 - Atmos.mkv",
            "Interstellar",
            Some(2014),
        ),
    ];
    for (path, title, year) in cases {
        assert_movie(path, title, *year);
    }
}

#[test]
fn movie_release_tags() {
    let m = movie("Testfilm Alpha (2021)/Testfilm.Alpha.2021.German.DL.1080p.WEB.H264-FLX.mp4");
    assert_eq!(m.tags.resolution, Some(1080));
    assert_eq!(m.tags.source, Some(Source::WebDl));
    assert_eq!(m.tags.video_codec, Some("h264"));
    assert!(m.tags.dual_language);
    assert_eq!(m.tags.languages, ["deu"]);
    assert_eq!(m.tags.release_group.as_deref(), Some("FLX"));

    let m = movie(
        "Avatar (2009) [imdbid-tt0499549]/Avatar.2009.2160p.UHD.BluRay.REMUX.HDR.HEVC.Atmos-GRP.mkv",
    );
    assert_eq!(m.ids.imdb.as_deref(), Some("tt0499549"));
    assert_eq!(m.tags.resolution, Some(2160));
    assert_eq!(m.tags.source, Some(Source::Remux));
    assert_eq!(m.tags.video_codec, Some("h265"));
    assert_eq!(m.tags.hdr, ["hdr10"]);
    assert!(m.tags.atmos);

    let m = movie(
        "Blade Runner 2049 (2017)/Blade.Runner.2049.2017.2160p.WEB-DL.DDP5.1.Atmos.DV.HDR.H.265-FLUX.mkv",
    );
    assert_eq!(m.tags.source, Some(Source::WebDl));
    assert_eq!(m.tags.audio_codec, Some("eac3"));
    assert_eq!(m.tags.audio_channels, Some("5.1"));
    assert_eq!(m.tags.video_codec, Some("h265"));
    assert_eq!(m.tags.hdr, ["dv", "hdr10"]);
    assert_eq!(m.tags.release_group.as_deref(), Some("FLUX"));

    let m = movie("Leon.Der.Profi.1994.Uncut.German.AC3.DVDRip.XviD.avi");
    assert_eq!(m.edition.as_deref(), Some("Uncut"));
    assert_eq!(m.tags.source, Some(Source::Dvd));
    assert_eq!(m.tags.video_codec, Some("xvid"));
    assert_eq!(m.tags.audio_codec, Some("ac3"));

    let m = movie("Toy Story 4 (2019)/Toy.Story.4.2019.3D.HSBS.1080p.BluRay.mkv");
    assert_eq!(m.title, "Toy Story 4");
    assert!(m.tags.is_3d);

    let m = movie("Das.Boot.1981.Directors.Cut.German.1080p.BluRay.x264.mkv");
    assert_eq!(m.edition.as_deref(), Some("Director's Cut"));

    let m = movie("Oppenheimer (2023)/Oppenheimer.2023.IMAX.1080p.mkv");
    assert_eq!(m.edition.as_deref(), Some("IMAX"));

    let m = movie("Movie.2020.PROPER.1080p.WEBRip.mkv");
    assert!(m.tags.proper);
    assert_eq!(m.tags.source, Some(Source::WebRip));
}

#[test]
fn movie_ids_editions_and_parts() {
    let m = movie("Inception (2010) {tmdb-27205}/Inception (2010) {tmdb-27205}.mkv");
    assert_eq!(
        (m.title.as_str(), m.year, m.ids.tmdb),
        ("Inception", Some(2010), Some(27205))
    );

    let m = movie("Heat (1995) [tmdbid-949]/Heat.1995.mkv");
    assert_eq!(m.ids.tmdb, Some(949));

    let m = movie("Heat (1995)/Heat.1995.tt0113277.mkv");
    assert_eq!(m.ids.imdb.as_deref(), Some("tt0113277"));

    let m =
        movie("Alien (1979) {edition-Director's Cut}/Alien (1979) {edition-Director's Cut}.mkv");
    assert_eq!(m.title, "Alien");
    assert_eq!(m.edition.as_deref(), Some("Director's Cut"));

    assert_eq!(movie("Movie Title (2020) - cd1.avi").part, Some(1));
    assert_eq!(
        movie("Movie Title (2020)/Movie Title (2020) - Part 2.mkv").part,
        Some(2)
    );
    assert_eq!(movie("Movie.Title.2020.Disc.3.mkv").part, Some(3));
    assert_eq!(movie("Movie.Title.2020.pt1.mkv").part, Some(1));
    assert_eq!(
        movie("Dune Part Two (2024)/Dune.Part.Two.2024.mkv").part,
        None
    );
    assert_eq!(
        movie("Movie Title (2020)/Movie Title (2020) - Part 2.mkv").title,
        "Movie Title"
    );
}

// ---------------------------------------------------------------------------
// Episodes: hand-written
// ---------------------------------------------------------------------------

fn episode(path: &str) -> flixnet_library::parse::ParsedEpisode {
    parse_episode(Path::new(path))
}

#[track_caller]
fn assert_episode(path: &str, series: &str, season: Option<u16>, episodes: &[u16]) {
    let e = episode(path);
    assert_eq!(
        (e.series_title.as_str(), e.season, e.episodes.as_slice()),
        (series, season, episodes),
        "path: {path}"
    );
}

#[test]
fn episode_numbering_patterns() {
    let cases: &[(&str, &str, Option<u16>, &[u16])] = &[
        (
            "Testserie (2022) [tvdbid-990100]/Staffel 01/Testserie.S01E01.German.DL.1080p.WEB.x264-FLX.mkv",
            "Testserie",
            Some(1),
            &[1],
        ),
        (
            "Breaking Bad/Season 01/Breaking.Bad.S01E01.Pilot.720p.BluRay.x264.mkv",
            "Breaking Bad",
            Some(1),
            &[1],
        ),
        (
            "Breaking Bad (2008)/Season 05/Breaking Bad - S05E14 - Ozymandias.mkv",
            "Breaking Bad",
            Some(5),
            &[14],
        ),
        (
            "The Office (US)/Season 2/The.Office.US.S02E01E02.mkv",
            "The Office US",
            Some(2),
            &[1, 2],
        ),
        (
            "Show/Season 1/Show.S01E01-E02.mkv",
            "Show",
            Some(1),
            &[1, 2],
        ),
        (
            "Show/Season 1/Show.S01E01-03.mkv",
            "Show",
            Some(1),
            &[1, 2, 3],
        ),
        (
            "Friends/Staffel 3/Friends - 3x05 - Folge.mkv",
            "Friends",
            Some(3),
            &[5],
        ),
        (
            "Friends/Staffel 3/Friends - 3x05-06.mkv",
            "Friends",
            Some(3),
            &[5, 6],
        ),
        (
            "Dark (2017)/Staffel 1/Dark.Staffel.1.Folge.2.German.mkv",
            "Dark",
            Some(1),
            &[2],
        ),
        (
            "Dark (2017)/Dark - Season 2 Episode 4.mkv",
            "Dark",
            Some(2),
            &[4],
        ),
        (
            "Doctor Who (2005)/Specials/Doctor.Who.S00E01.mkv",
            "Doctor Who",
            Some(0),
            &[1],
        ),
        (
            "Doctor Who (2005)/Specials/Christmas Special.mkv",
            "Doctor Who",
            Some(0),
            &[],
        ),
        ("Lost/Season 1/01 - Pilot Part 1.mkv", "Lost", Some(1), &[1]),
        ("Lost/Season 1/E05.mkv", "Lost", Some(1), &[5]),
        ("Lost/Season 1/Episode 7.mkv", "Lost", Some(1), &[7]),
        ("Lost/Staffel 1/Folge 8.mkv", "Lost", Some(1), &[8]),
        ("Westworld.S01E01.1080p.mkv", "Westworld", Some(1), &[1]),
        ("Westworld.2016.S01E01.mkv", "Westworld", Some(1), &[1]),
        (
            "Show/Season 01/Show S01 E03 Title.mkv",
            "Show",
            Some(1),
            &[3],
        ),
        ("Show/s01e04.mkv", "Show", Some(1), &[4]),
        (
            "9-1-1 (2018)/Season 1/9-1-1.S01E01.mkv",
            "9-1-1",
            Some(1),
            &[1],
        ),
        ("Show.Name.S2023E05.mkv", "Show Name", Some(2023), &[5]),
        (
            "Game of Thrones/Season 08/Game.of.Thrones.S08E06.The.Iron.Throne.1080p.AMZN.WEB-DL.DDP5.1.H.264-GoT.mkv",
            "Game of Thrones",
            Some(8),
            &[6],
        ),
        (
            "Babylon Berlin (2017)/S03/Babylon.Berlin.S03E12.German.DL.mkv",
            "Babylon Berlin",
            Some(3),
            &[12],
        ),
        (
            "Tatort/Season 2023/Tatort.S2023E01.mkv",
            "Tatort",
            Some(2023),
            &[1],
        ),
    ];
    for (path, series, season, episodes) in cases {
        assert_episode(path, series, *season, episodes);
    }
}

#[test]
fn episode_details() {
    let e = episode(
        "Testserie (2022) [tvdbid-990100]/Staffel 01/Testserie.S01E02.German.DL.1080p.WEB.x264-FLX.mkv",
    );
    assert_eq!(e.series_year, Some(2022));
    assert_eq!(e.ids.tvdb, Some(990_100));
    assert_eq!(e.tags.resolution, Some(1080));
    assert_eq!(e.tags.release_group.as_deref(), Some("FLX"));

    let e = episode("Breaking Bad/Season 01/Breaking.Bad.S01E01.Pilot.720p.BluRay.x264.mkv");
    assert_eq!(e.episode_title.as_deref(), Some("Pilot"));
    assert_eq!(e.tags.resolution, Some(720));

    let e = episode("Breaking Bad (2008)/Season 05/Breaking Bad - S05E14 - Ozymandias.mkv");
    assert_eq!(e.episode_title.as_deref(), Some("Ozymandias"));
    assert_eq!(e.series_year, Some(2008));

    let e = episode(
        "Game of Thrones/Season 08/Game.of.Thrones.S08E06.The.Iron.Throne.1080p.AMZN.WEB-DL.DDP5.1.H.264-GoT.mkv",
    );
    assert_eq!(e.episode_title.as_deref(), Some("The Iron Throne"));
    assert_eq!(e.tags.audio_codec, Some("eac3"));
    assert_eq!(e.tags.release_group.as_deref(), Some("GoT"));

    let e = episode("Lost/Season 1/01 - Pilot Part 1.mkv");
    assert_eq!(e.episode_title.as_deref(), Some("Pilot Part 1"));

    let e = episode("Westworld.2016.S01E01.mkv");
    assert_eq!(e.series_year, Some(2016));
}

#[test]
fn daily_and_absolute_episodes() {
    let e = episode("The Daily Show/The.Daily.Show.2024.03.15.Guest.Name.720p.WEB.mkv");
    assert_eq!(e.series_title, "The Daily Show");
    assert_eq!(
        e.air_date,
        Some(AirDate {
            year: 2024,
            month: 3,
            day: 15
        })
    );
    assert_eq!(e.episode_title.as_deref(), Some("Guest Name"));

    let e = episode("Tagesschau/Tagesschau 2023-12-31.mp4");
    assert_eq!(
        e.air_date,
        Some(AirDate {
            year: 2023,
            month: 12,
            day: 31
        })
    );

    let e = episode("One Piece/[SubsPlease] One Piece - 1071 (1080p) [ABCDEF].mkv");
    assert_eq!(e.series_title, "One Piece");
    assert_eq!(e.absolute, Some(1071));
    assert_eq!(e.season, None);

    let e = episode("Naruto/Naruto - 042v2 [720p].mkv");
    assert_eq!(e.absolute, Some(42));

    // A year after a dash is not an absolute episode number.
    let e = episode("Some Show/Some Show - 2019 Recap.mkv");
    assert_eq!(e.absolute, None);
}

// ---------------------------------------------------------------------------
// File roles and sidecars
// ---------------------------------------------------------------------------

#[test]
fn file_roles() {
    let cases: &[(&str, FileRole)] = &[
        ("Movie (2020)/Movie (2020).mkv", FileRole::Main),
        (
            "Movie (2020)/Movie (2020)-trailer.mkv",
            FileRole::Extra(ExtraKind::Trailer),
        ),
        (
            "Movie (2020)/Making Of-behindthescenes.mp4",
            FileRole::Extra(ExtraKind::BehindTheScenes),
        ),
        (
            "Movie (2020)/Trailers/Teaser.mkv",
            FileRole::Extra(ExtraKind::Trailer),
        ),
        (
            "Movie (2020)/Featurettes/Effects.mkv",
            FileRole::Extra(ExtraKind::Featurette),
        ),
        (
            "Movie (2020)/Deleted Scenes/Scene 1.mkv",
            FileRole::Extra(ExtraKind::DeletedScene),
        ),
        (
            "Movie (2020)/Extras/Bloopers.mkv",
            FileRole::Extra(ExtraKind::Other),
        ),
        (
            "Movie (2020)/Interviews/Director.mkv",
            FileRole::Extra(ExtraKind::Interview),
        ),
        (
            "Show/Season 1/Show.S01E01-deleted.mkv",
            FileRole::Extra(ExtraKind::DeletedScene),
        ),
        ("Movie (2020)/Sample/movie-sample.mkv", FileRole::Sample),
        ("Movie (2020)/Movie.2020.1080p.sample.mkv", FileRole::Sample),
        ("Movie (2020)/Movie (2020).nfo", FileRole::Ignored),
        ("Movie (2020)/poster.jpg", FileRole::Ignored),
        ("Movie (2020)/Movie (2020).srt", FileRole::Ignored),
        ("Movie (2020)/.hidden.mkv", FileRole::Ignored),
        (".Trash/Movie.mkv", FileRole::Ignored),
        ("@eaDir/Movie.mkv/SYNOVIDEO.mkv", FileRole::Ignored),
        ("Movie (2020)/Movie (2020).m2ts", FileRole::Main),
        ("Movie (2020)/Movie (2020).WEBM", FileRole::Main),
        ("Sampler (2019)/Sampler (2019).mkv", FileRole::Main),
    ];
    for (path, role) in cases {
        assert_eq!(classify(Path::new(path)), *role, "path: {path}");
    }
}

#[test]
fn sidecar_subtitles() {
    let video = "Testfilm Beta (2019) {tmdb-990001}";
    let info = |name: &str| subtitle_info(video, Path::new(name));
    assert_eq!(
        info("Testfilm Beta (2019) {tmdb-990001}.de.srt"),
        Some((Some("deu".into()), false, false))
    );
    assert_eq!(
        info("Testfilm Beta (2019) {tmdb-990001}.de.forced.srt"),
        Some((Some("deu".into()), true, false))
    );
    assert_eq!(
        info("Testfilm Beta (2019) {tmdb-990001}.en.sdh.srt"),
        Some((Some("eng".into()), false, true))
    );
    assert_eq!(
        info("Testfilm Beta (2019) {tmdb-990001}.eng.ass"),
        Some((Some("eng".into()), false, false))
    );
    assert_eq!(
        info("Testfilm Beta (2019) {tmdb-990001}.German.srt"),
        Some((Some("deu".into()), false, false))
    );
    assert_eq!(
        info("Testfilm Beta (2019) {tmdb-990001}.srt"),
        Some((None, false, false))
    );
    assert_eq!(info("Other Movie.de.srt"), None);
}

// ---------------------------------------------------------------------------
// Generated: titles × naming templates
// ---------------------------------------------------------------------------

const MOVIES: &[(&str, u16)] = &[
    ("Testfilm Alpha", 2021),
    ("Der Pate", 1972),
    ("Das Boot", 1981),
    ("Lola rennt", 1998),
    ("Good Bye Lenin", 2003),
    ("Das Leben der Anderen", 2006),
    ("Im Westen nichts Neues", 2022),
    ("Toni Erdmann", 2016),
    ("Der Schuh des Manitu", 2001),
    ("Fack ju Göhte", 2013),
    ("The Dark Knight", 2008),
    ("Interstellar", 2014),
    ("Pulp Fiction", 1994),
    ("Fight Club", 1999),
    ("Parasite", 2019),
    ("Arrival", 2016),
    ("Her", 2013),
    ("Heat", 1995),
    ("Alien", 1979),
    ("Coco", 2017),
    ("Gravity", 2013),
    ("Whiplash", 2014),
];

const MOVIE_TEMPLATES: &[&str] = &[
    "{T} ({Y})/{T} ({Y}).mkv",
    "{T} ({Y})/{D}.{Y}.German.DL.1080p.BluRay.x264-GRP.mkv",
    "{D}.{Y}.1080p.WEB-DL.DDP5.1.H.264-FLX.mkv",
    "{T} ({Y}) [imdbid-tt1234567]/{T} ({Y}).mp4",
    "{T} ({Y})/{D}.{Y}.2160p.UHD.BluRay.REMUX.HDR.HEVC.Atmos-GRP.mkv",
    "{T} [{Y}]/{T} [{Y}] 720p.avi",
    "{D}.{Y}.GERMAN.DVDRip.XviD-GRP.avi",
    "{U}_{Y}_1080p.mkv",
];

fn fill(template: &str, title: &str, year: u16) -> String {
    template
        .replace("{T}", title)
        .replace("{D}", &title.replace(' ', "."))
        .replace("{U}", &title.replace(' ', "_"))
        .replace("{Y}", &year.to_string())
}

#[test]
fn generated_movie_matrix() {
    let mut checked = 0;
    for (title, year) in MOVIES {
        for template in MOVIE_TEMPLATES {
            let path = fill(template, title, *year);
            assert_movie(&path, title, Some(*year));
            checked += 1;
        }
    }
    assert_eq!(checked, MOVIES.len() * MOVIE_TEMPLATES.len());
}

const SERIES: &[(&str, u16)] = &[
    ("Testserie", 2022),
    ("Dark", 2017),
    ("Babylon Berlin", 2017),
    ("How to Sell Drugs Online Fast", 2019),
    ("Der Tatortreiniger", 2011),
    ("Stromberg", 2004),
    ("The Expanse", 2015),
    ("Severance", 2022),
    ("Breaking Bad", 2008),
    ("Better Call Saul", 2015),
    ("Stranger Things", 2016),
    ("The Bear", 2022),
];

#[test]
fn generated_episode_matrix() {
    let mut checked = 0;
    for (i, (title, year)) in SERIES.iter().enumerate() {
        let season = (i % 4) as u16 + 1;
        let ep = (i % 9) as u16 + 1;
        let dotted = title.replace(' ', ".");
        let cases: Vec<(String, Vec<u16>)> = vec![
            (
                format!(
                    "{title} ({year})/Season {season:02}/{dotted}.S{season:02}E{ep:02}.German.DL.1080p.WEB.x264-FLX.mkv"
                ),
                vec![ep],
            ),
            (
                format!(
                    "{title}/Staffel {season}/{title} - S{season:02}E{ep:02} - Episodentitel.mkv"
                ),
                vec![ep],
            ),
            (
                format!("{title} ({year})/Season {season}/{title} - {season}x{ep:02} - Titel.mkv"),
                vec![ep],
            ),
            (
                format!("{dotted}.S{season:02}E{ep:02}.720p.HDTV.x264.mkv"),
                vec![ep],
            ),
            (
                format!(
                    "{title} ({year}) [tvdbid-123]/S{season:02}/{dotted}.S{season:02}E{ep:02}E{:02}.mkv",
                    ep + 1
                ),
                vec![ep, ep + 1],
            ),
            (format!("{title}/Season {season:02}/E{ep:02}.mkv"), vec![ep]),
        ];
        for (path, episodes) in cases {
            assert_episode(&path, title, Some(season), &episodes);
            checked += 1;
        }
    }
    assert_eq!(checked, SERIES.len() * 6);
}

#[test]
fn total_case_count_meets_the_milestone_gate() {
    // 35 titles/years + 8 tag groups + 10 id/part + 25 episodes + 6 details + 5 daily/absolute
    // + 20 roles + 7 sidecars + generated matrices.
    let generated = MOVIES.len() * MOVIE_TEMPLATES.len() + SERIES.len() * 6;
    let hand_written = 35 + 8 + 10 + 25 + 6 + 5 + 20 + 7;
    assert!(
        generated + hand_written >= 300,
        "{}",
        generated + hand_written
    );
}

#[test]
fn soft_tags_between_title_and_year() {
    assert_movie(
        "Alpha Remastered (2020)/Alpha.Remastered.2020.2160p.mkv",
        "Alpha",
        Some(2020),
    );
    assert_movie(
        "Aliens Extended Edition (1986)/Aliens.mkv",
        "Aliens",
        Some(1986),
    );
    assert_movie("Das Boot German Uncut 1981.mkv", "Das Boot", Some(1981));
    assert_eq!(
        movie("Alpha Remastered (2020)/Alpha.mkv")
            .edition
            .as_deref(),
        Some("Remastered")
    );
    // A year after technical tags is not the release year.
    assert_movie("Movie.Title.1080p.2160p.mkv", "Movie Title", None);
}
