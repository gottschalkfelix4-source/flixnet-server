//! Synthetic, deterministic test media generated with ffmpeg's lavfi sources.
//!
//! The library mimics a real one: German release names, Plex/Jellyfin folder layouts,
//! provider-id hints, sidecar subtitles, many codecs/containers, and a season whose
//! episodes share an identical intro (for intro detection). Nothing here is copyrighted.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::Context;

use crate::run;

/// Shared encoder settings that keep generation fast.
const X264: &[&str] = &["-c:v", "libx264", "-preset", "veryfast", "-crf", "30"];

/// Writes one fixture: `(output file, scratch dir)`.
type Builder = Box<dyn Fn(&Path, &Path) -> anyhow::Result<()>>;

pub fn generate(out: &Path, force: bool) -> anyhow::Result<()> {
    fs::create_dir_all(out)?;
    let tmp = out.join(".tmp");
    fs::create_dir_all(&tmp)?;

    let jobs: Vec<(&str, Builder)> = vec![
        (
            "movies/Testfilm Alpha (2021)/Testfilm.Alpha.2021.German.DL.1080p.WEB.H264-FLX.mp4",
            Box::new(movie_h264_chapters),
        ),
        (
            "movies/Testfilm Beta (2019) {tmdb-990001}/Testfilm Beta (2019) {tmdb-990001}.mkv",
            Box::new(movie_hevc_hdr10_multi_audio),
        ),
        (
            "movies/Testfilm Gamma (2023)/Testfilm.Gamma.2023.2160p.UHD.AV1.Opus-FLX.mkv",
            Box::new(movie_av1),
        ),
        (
            "movies/Testfilm Delta (2018)/Testfilm.Delta.2018.720p.VP9.webm",
            Box::new(movie_vp9),
        ),
        (
            "movies/Testfilm Epsilon (1999)/Testfilm.Epsilon.1999.GERMAN.DVDRip.MPEG2.ts",
            Box::new(movie_mpeg2_interlaced),
        ),
        (
            "shows/Testserie (2022) [tvdbid-990100]/Staffel 01/Testserie.S01E01.German.DL.1080p.WEB.x264-FLX.mkv",
            Box::new(|o, t| episode(o, t, 1, 0.0)),
        ),
        (
            "shows/Testserie (2022) [tvdbid-990100]/Staffel 01/Testserie.S01E02.German.DL.1080p.WEB.x264-FLX.mkv",
            Box::new(|o, t| episode(o, t, 2, 8.0)),
        ),
        (
            "shows/Testserie (2022) [tvdbid-990100]/Staffel 01/Testserie.S01E03.German.DL.1080p.WEB.x264-FLX.mp4",
            Box::new(|o, t| episode(o, t, 3, 15.0)),
        ),
    ];

    for (relative, build) in jobs {
        let target = out.join(relative);
        if target.exists() && !force {
            println!("skip     {relative}");
            continue;
        }
        fs::create_dir_all(target.parent().expect("fixture path has a parent"))?;
        println!("generate {relative}");
        build(&target, &tmp).with_context(|| format!("generating {relative}"))?;
    }

    write_sidecars(out)?;
    write_metadata(out, force)?;
    fs::remove_dir_all(&tmp).ok();
    println!("fixtures ready in {}", out.display());
    Ok(())
}

fn ffmpeg() -> Command {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    cmd
}

fn lavfi(cmd: &mut Command, source: &str) {
    cmd.args(["-f", "lavfi", "-i", source]);
}

fn p(path: &Path) -> &str {
    path.to_str().expect("fixture paths are UTF-8")
}

/// H.264/AAC in MP4 with three chapters.
fn movie_h264_chapters(out: &Path, tmp: &Path) -> anyhow::Result<()> {
    let meta = tmp.join("alpha-chapters.txt");
    fs::write(
        &meta,
        ";FFMETADATA1\ntitle=Testfilm Alpha\n\
         [CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=10000\ntitle=Anfang\n\
         [CHAPTER]\nTIMEBASE=1/1000\nSTART=10000\nEND=20000\ntitle=Mitte\n\
         [CHAPTER]\nTIMEBASE=1/1000\nSTART=20000\nEND=30000\ntitle=Ende\n",
    )?;
    let mut cmd = ffmpeg();
    lavfi(&mut cmd, "testsrc2=s=1280x720:r=25:d=30");
    lavfi(&mut cmd, "sine=f=440:d=30");
    cmd.args([
        "-i",
        p(&meta),
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-map_chapters",
        "2",
    ]);
    cmd.args(X264).args([
        "-c:a",
        "aac",
        "-b:a",
        "128k",
        "-movflags",
        "+faststart",
        p(out),
    ]);
    run(&mut cmd)
}

/// HEVC Main10 with HDR10 metadata, German + English audio and an embedded English ASS track.
fn movie_hevc_hdr10_multi_audio(out: &Path, tmp: &Path) -> anyhow::Result<()> {
    let ass = tmp.join("beta.en.ass");
    fs::write(
        &ass,
        "[Script Info]\nScriptType: v4.00+\nPlayResX: 1280\nPlayResY: 720\n\n\
         [V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, Bold, Italic, Alignment, MarginV\n\
         Style: Default,Arial,40,&H00FFFFFF,0,0,2,30\n\n\
         [Events]\nFormat: Layer, Start, End, Style, Text\n\
         Dialogue: 0,0:00:01.00,0:00:04.00,Default,This is an English subtitle.\n\
         Dialogue: 0,0:00:10.00,0:00:14.00,Default,{\\i1}Styled{\\i0} text in the middle.\n",
    )?;
    let mut cmd = ffmpeg();
    lavfi(&mut cmd, "testsrc2=s=1280x720:r=24:d=20");
    lavfi(&mut cmd, "sine=f=330:d=20");
    lavfi(&mut cmd, "sine=f=550:d=20");
    cmd.args(["-i", p(&ass)]);
    cmd.args(["-map", "0:v", "-map", "1:a", "-map", "2:a", "-map", "3:s"]);
    // ffmpeg 8 takes colour properties from the frames, so tag them there.
    cmd.args([
        "-vf",
        "setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc:range=tv",
    ]);
    cmd.args([
        "-c:v", "libx265", "-preset", "ultrafast", "-crf", "30", "-pix_fmt", "yuv420p10le",
        "-x265-params",
        "log-level=error:hdr10=1:repeat-headers=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:\
         master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400",
        "-color_primaries", "bt2020", "-color_trc", "smpte2084", "-colorspace", "bt2020nc",
    ]);
    cmd.args(["-c:a", "aac", "-b:a", "128k", "-ac:a:1", "6"]);
    cmd.args(["-c:s", "ass"]);
    cmd.args([
        "-metadata:s:a:0",
        "language=deu",
        "-metadata:s:a:0",
        "title=Deutsch",
        "-metadata:s:a:1",
        "language=eng",
        "-metadata:s:a:1",
        "title=English 5.1",
        "-metadata:s:s:0",
        "language=eng",
        "-metadata:s:s:0",
        "title=English (ASS)",
        "-disposition:a:0",
        "default",
    ]);
    cmd.arg(p(out));
    run(&mut cmd)
}

/// AV1 + Opus in MKV.
fn movie_av1(out: &Path, _tmp: &Path) -> anyhow::Result<()> {
    let mut cmd = ffmpeg();
    cmd.env("SVT_LOG", "1");
    lavfi(&mut cmd, "testsrc2=s=1280x720:r=24:d=15");
    lavfi(&mut cmd, "sine=f=660:d=15");
    cmd.args(["-c:v", "libsvtav1", "-preset", "12", "-crf", "45"]);
    cmd.args([
        "-c:a",
        "libopus",
        "-b:a",
        "96k",
        "-metadata:s:a:0",
        "language=eng",
        p(out),
    ]);
    run(&mut cmd)
}

/// VP9 + Opus in WebM.
fn movie_vp9(out: &Path, _tmp: &Path) -> anyhow::Result<()> {
    let mut cmd = ffmpeg();
    lavfi(&mut cmd, "testsrc2=s=1280x720:r=30:d=15");
    lavfi(&mut cmd, "sine=f=770:d=15");
    cmd.args([
        "-c:v",
        "libvpx-vp9",
        "-deadline",
        "realtime",
        "-cpu-used",
        "8",
        "-b:v",
        "1M",
    ]);
    cmd.args(["-c:a", "libopus", "-b:a", "96k", p(out)]);
    run(&mut cmd)
}

/// Interlaced MPEG-2 with MP2 audio in MPEG-TS, like a DVB recording.
fn movie_mpeg2_interlaced(out: &Path, _tmp: &Path) -> anyhow::Result<()> {
    let mut cmd = ffmpeg();
    lavfi(&mut cmd, "testsrc2=s=720x576:r=25:d=15");
    lavfi(&mut cmd, "sine=f=220:d=15");
    cmd.args([
        "-vf",
        "interlace",
        "-c:v",
        "mpeg2video",
        "-b:v",
        "4M",
        "-flags",
        "+ilme+ildct",
        "-top",
        "1",
        "-c:a",
        "mp2",
        "-b:a",
        "192k",
        "-metadata:s:a:0",
        "language=deu",
    ]);
    cmd.args(["-f", "mpegts", p(out)]);
    run(&mut cmd)
}

/// One episode: optional cold open, then an identical 20 s intro, then unique content.
/// The intro audio uses a fixed noise seed so every episode carries the same fingerprint.
fn episode(out: &Path, _tmp: &Path, number: u32, cold_open: f64) -> anyhow::Result<()> {
    const INTRO: f64 = 20.0;
    const TOTAL: f64 = 75.0;
    let body = TOTAL - INTRO - cold_open;
    let seed = 1000 + number;

    let mut cmd = ffmpeg();
    let mut parts = Vec::new();
    let mut index = 0;
    let mut add = |cmd: &mut Command, video: String, audio: String| {
        lavfi(cmd, &video);
        lavfi(cmd, &audio);
        parts.push(format!("[{index}:v][{}:a]", index + 1));
        index += 2;
    };
    if cold_open > 0.0 {
        add(
            &mut cmd,
            format!("testsrc2=s=1280x720:r=25:d={cold_open}"),
            format!("anoisesrc=d={cold_open}:c=brown:seed={seed}:a=0.4"),
        );
    }
    add(
        &mut cmd,
        format!("smptehdbars=s=1280x720:r=25:d={INTRO}"),
        format!("anoisesrc=d={INTRO}:c=pink:seed=4242:a=0.5"),
    );
    add(
        &mut cmd,
        format!("testsrc=s=1280x720:r=25:d={body}"),
        format!("anoisesrc=d={body}:c=white:seed={}:a=0.3", seed + 100),
    );

    let filter = format!("{}concat=n={}:v=1:a=1[v][a]", parts.concat(), parts.len());
    cmd.args(["-filter_complex", &filter, "-map", "[v]", "-map", "[a]"]);
    cmd.args(X264).args(["-c:a", "aac", "-b:a", "128k"]);
    cmd.args(["-metadata", &format!("title=Folge {number}")]);
    if out.extension().is_some_and(|e| e == "mp4") {
        cmd.args(["-movflags", "+faststart"]);
    }
    cmd.arg(p(out));
    run(&mut cmd)
}

/// A gradient image (poster/backdrop stand-in) rendered by ffmpeg.
fn art(
    path: &Path,
    width: u32,
    height: u32,
    from: &str,
    to: &str,
    seed: u32,
    force: bool,
) -> anyhow::Result<()> {
    if path.exists() && !force {
        return Ok(());
    }
    let mut cmd = ffmpeg();
    lavfi(
        &mut cmd,
        &format!("gradients=s={width}x{height}:c0={from}:c1={to}:seed={seed}:speed=0:d=1"),
    );
    cmd.args(["-frames:v", "1", "-q:v", "3", p(path)]);
    run(&mut cmd)
}

/// NFO files and artwork so the web UI has titles, plots, genres and images to show.
fn write_metadata(out: &Path, force: bool) -> anyhow::Result<()> {
    let movies = [
        FixtureMovie {
            folder: "movies/Testfilm Alpha (2021)",
            title: "Testfilm Alpha",
            plot: "Ein Testbild macht sich auf die Reise durch die Farbbalken des Universums.",
            genre: "Abenteuer",
            mpaa: "FSK 6",
            colors: ("0x1e3a8a", "0xff6b3d"),
        },
        FixtureMovie {
            folder: "movies/Testfilm Beta (2019) {tmdb-990001}",
            title: "Testfilm Beta",
            plot: "In HDR10 leuchtet alles heller – auch die Geheimnisse einer stillen Kleinstadt.",
            genre: "Drama",
            mpaa: "FSK 12",
            colors: ("0x14532d", "0xfacc15"),
        },
        FixtureMovie {
            folder: "movies/Testfilm Gamma (2023)",
            title: "Testfilm Gamma",
            plot: "Ein AV1-kodierter Thriller über Kompression und Verlust.",
            genre: "Thriller",
            mpaa: "FSK 16",
            colors: ("0x450a0a", "0xf97316"),
        },
        FixtureMovie {
            folder: "movies/Testfilm Delta (2018)",
            title: "Testfilm Delta",
            plot: "Eine leichte Komödie in VP9 über ein Missverständnis im Büro.",
            genre: "Komödie",
            mpaa: "FSK 0",
            colors: ("0x3b0764", "0x22d3ee"),
        },
        FixtureMovie {
            folder: "movies/Testfilm Epsilon (1999)",
            title: "Testfilm Epsilon",
            plot: "Ein interlaced Klassiker aus der Zeit der DVB-Aufnahmen.",
            genre: "Klassiker",
            mpaa: "FSK 12",
            colors: ("0x1f2937", "0xa3e635"),
        },
    ];
    for (i, movie) in movies.iter().enumerate() {
        let FixtureMovie {
            folder,
            title,
            plot,
            genre,
            mpaa,
            colors: (from, to),
        } = movie;
        let dir = out.join(folder);
        if !dir.exists() {
            continue;
        }
        let year = folder
            .split('(')
            .nth(1)
            .and_then(|s| s.get(..4))
            .unwrap_or("2020");
        fs::write(
            dir.join("movie.nfo"),
            format!(
                "<movie><title>{title}</title><year>{year}</year><plot>{plot}</plot><genre>{genre}</genre>\
                 <mpaa>{mpaa}</mpaa><rating>{rating}</rating><runtime>1</runtime>\
                 <director>Regie Robot</director>\
                 <actor><name>Erika Testfrau</name><role>Hauptrolle</role><order>0</order></actor>\
                 <actor><name>Max Pixel</name><role>Nebenrolle</role><order>1</order></actor></movie>",
                rating = 6.5 + i as f64 * 0.4
            ),
        )?;
        art(
            &dir.join("poster.jpg"),
            500,
            750,
            from,
            to,
            i as u32 + 1,
            force,
        )?;
        art(
            &dir.join("fanart.jpg"),
            1280,
            720,
            to,
            from,
            i as u32 + 11,
            force,
        )?;
    }

    let series = out.join("shows/Testserie (2022) [tvdbid-990100]");
    if series.exists() {
        fs::write(
            series.join("tvshow.nfo"),
            "<tvshow><title>Testserie</title><year>2022</year>\
             <plot>Drei Folgen, ein Intro: Die Serie, an der Flixnet das Überspringen lernt.</plot>\
             <genre>Mystery</genre><mpaa>FSK 12</mpaa><rating>8.1</rating>\
             <actor><name>Tom Test</name><role>Kommissar</role></actor></tvshow>",
        )?;
        art(
            &series.join("poster.jpg"),
            500,
            750,
            "0x0f172a",
            "0xe11d48",
            21,
            force,
        )?;
        art(
            &series.join("fanart.jpg"),
            1280,
            720,
            "0xe11d48",
            "0x0f172a",
            22,
            force,
        )?;
        art(
            &series.join("season01-poster.jpg"),
            500,
            750,
            "0x172554",
            "0xe11d48",
            23,
            force,
        )?;
    }
    Ok(())
}

/// A fixture movie's metadata and the two colours of its generated artwork.
struct FixtureMovie {
    folder: &'static str,
    title: &'static str,
    plot: &'static str,
    genre: &'static str,
    mpaa: &'static str,
    colors: (&'static str, &'static str),
}

/// External subtitles next to the media files (SRT, forced SRT).
fn write_sidecars(out: &Path) -> anyhow::Result<()> {
    let beta = out.join("movies/Testfilm Beta (2019) {tmdb-990001}");
    fs::write(
        beta.join("Testfilm Beta (2019) {tmdb-990001}.de.srt"),
        "1\n00:00:01,000 --> 00:00:04,000\nDas ist ein deutscher Untertitel.\n\n\
         2\n00:00:10,000 --> 00:00:14,000\nÄrger über Größe – Umlaute müssen stimmen.\n",
    )?;
    fs::write(
        beta.join("Testfilm Beta (2019) {tmdb-990001}.de.forced.srt"),
        "1\n00:00:05,000 --> 00:00:08,000\n[Schild: Ausgang]\n",
    )?;
    let alpha = out.join("movies/Testfilm Alpha (2021)");
    fs::write(
        alpha.join("Testfilm.Alpha.2021.German.DL.1080p.WEB.H264-FLX.en.srt"),
        "1\n00:00:02,000 --> 00:00:05,000\nHello from the English sidecar.\n",
    )?;
    Ok(())
}
