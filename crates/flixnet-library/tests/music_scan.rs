//! Music libraries: tags of generated audio files (ffmpeg lavfi sine + `-metadata` +
//! attached covers) become artists, albums and tracks; folder and embedded artwork,
//! lyrics, re-tags and rescans. Needs ffmpeg/ffprobe.

use std::path::{Path, PathBuf};
use std::process::Command;

use flixnet_core::events::EventBus;
use flixnet_core::jobs::{JobQueue, JobRuntime};
use flixnet_db::{Db, items, jobs, libraries, media, metadata, music};
use flixnet_library::images::ImageCache;
use flixnet_library::jobs::{ProbePayload, probe_file};
use flixnet_library::music::lyrics;
use flixnet_library::scan::scan_library;
use flixnet_media::Ffprobe;
use tempfile::TempDir;

/// Encodes `seconds` of a sine tone with tags (and optionally an attached cover).
fn encode(path: &Path, seconds: u32, tags: &[(&str, &str)], cover: Option<&Path>, codec: &[&str]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:duration={seconds}"));
    if let Some(cover) = cover {
        cmd.arg("-i").arg(cover);
        cmd.args([
            "-map",
            "0:a",
            "-map",
            "1:v",
            "-c:v",
            "png",
            "-disposition:v",
            "attached_pic",
        ]);
    }
    for (key, value) in tags {
        cmd.arg("-metadata").arg(format!("{key}={value}"));
    }
    cmd.args(codec).arg(path);
    let status = cmd.status().expect("ffmpeg must be installed");
    assert!(status.success(), "ffmpeg failed for {}", path.display());
}

fn png(path: &Path, rgb: [u8; 3]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::RgbImage::from_pixel(32, 32, image::Rgb(rgb))
        .save(path)
        .unwrap();
}

struct Fixture {
    dir: TempDir,
    root: PathBuf,
    db: Db,
    queue: JobQueue,
    library: i64,
}

/// - Anna Alpha / Sommer: two FLAC tracks with full tags, MusicBrainz ids, ReplayGain,
///   an embedded cover, a folder `cover.jpg` and an `.lrc` for track 1.
/// - Bert Beta / Doppelalbum: MP3 on two discs in `CD1`/`CD2` folders, embedded cover.
/// - Sampler: a compilation (no album artist) with two track artists.
/// - Carla / Ohne Tags: a WAV without tags (folder layout only).
async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Musik");
    let cover = dir.path().join("cover-red.png");
    png(&cover, [200, 20, 20]);
    let flac = ["-c:a", "flac"];
    let mp3 = ["-c:a", "libmp3lame", "-b:a", "128k", "-id3v2_version", "3"];

    for (n, title) in [(1, "Erster Song"), (2, "Zweiter Song")] {
        let track = n.to_string();
        encode(
            &root.join(format!("Anna Alpha/Sommer (2020)/0{n} - {title}.flac")),
            2,
            &[
                ("title", title),
                ("artist", "Anna Alpha"),
                ("album_artist", "Anna Alpha"),
                ("album", "Sommer"),
                ("track", &format!("{track}/2")),
                ("disc", "1/1"),
                ("date", "2020"),
                ("genre", "Pop;Elektro"),
                ("REPLAYGAIN_TRACK_GAIN", "-6.50 dB"),
                ("REPLAYGAIN_TRACK_PEAK", "0.950000"),
                ("REPLAYGAIN_ALBUM_GAIN", "-7.25 dB"),
                (
                    "MUSICBRAINZ_ALBUMID",
                    "11111111-1111-1111-1111-111111111111",
                ),
                (
                    "MUSICBRAINZ_ALBUMARTISTID",
                    "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                ),
                (
                    "MUSICBRAINZ_RELEASETRACKID",
                    &format!("22222222-2222-2222-2222-00000000000{n}"),
                ),
            ],
            Some(&cover),
            &flac,
        );
    }
    std::fs::write(
        root.join("Anna Alpha/Sommer (2020)/01 - Erster Song.lrc"),
        "[ti:Erster Song]\n[00:00.50]Hallo Sonne\n[00:01.20]Hallo Meer\n",
    )
    .unwrap();
    image::RgbImage::from_pixel(16, 16, image::Rgb([20, 200, 20]))
        .save(root.join("Anna Alpha/Sommer (2020)/cover.jpg"))
        .unwrap();

    for disc in [1, 2] {
        encode(
            &root.join(format!(
                "Bert Beta/Doppelalbum/CD{disc}/01 - Seite {disc}.mp3"
            )),
            2,
            &[
                ("title", &format!("Seite {disc}")),
                ("artist", "Bert Beta"),
                ("album", "Doppelalbum"),
                ("track", "1"),
                ("disc", &format!("{disc}/2")),
                ("date", "2018-05-01"),
                ("genre", "Rock"),
            ],
            Some(&cover),
            &mp3,
        );
    }

    for (n, artist) in [(1, "Anna Alpha"), (2, "Dora Delta")] {
        encode(
            &root.join(format!("Sampler/0{n}.mp3")),
            2,
            &[
                ("title", &format!("Sampler-Hit {n}")),
                ("artist", artist),
                ("album", "Sommerhits"),
                ("track", &n.to_string()),
                ("compilation", "1"),
                ("date", "2021"),
            ],
            None,
            &mp3,
        );
    }

    encode(
        &root.join("Carla Cello/Ohne Tags/03 - Nackt.wav"),
        1,
        &[],
        None,
        &[
            "-map_metadata",
            "-1",
            "-fflags",
            "+bitexact",
            "-c:a",
            "pcm_s16le",
        ],
    );

    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    let queue = JobRuntime::new(db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    let library = libraries::create(
        &db,
        "Musik",
        "music",
        &[root.to_string_lossy().into_owned()],
    )
    .await
    .unwrap()
    .id;
    Fixture {
        dir,
        root,
        db,
        queue,
        library,
    }
}

async fn run_probes(db: &Db) -> usize {
    let ffprobe = Ffprobe::new("ffprobe");
    let mut ran = 0;
    while let Some(job) = jobs::claim(db, "probe").await.unwrap() {
        let payload: ProbePayload = serde_json::from_str(&job.payload).unwrap();
        probe_file(db, &ffprobe, payload.file_id).await.unwrap();
        jobs::complete(db, job.id).await.unwrap();
        ran += 1;
    }
    ran
}

async fn titles(db: &Db, library: i64, kind: &str) -> Vec<String> {
    let mut out: Vec<String> = items::list_by_kind(db, library, kind)
        .await
        .unwrap()
        .into_iter()
        .map(|i| i.title)
        .collect();
    out.sort();
    out
}

async fn by_title(db: &Db, library: i64, kind: &str, title: &str) -> items::Item {
    items::list_by_kind(db, library, kind)
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.title == title)
        .unwrap_or_else(|| panic!("no {kind} {title}"))
}

#[tokio::test]
async fn tags_become_artists_albums_and_tracks() {
    let f = fixture().await;
    let report = scan_library(&f.db, &f.queue, f.library).await.unwrap();
    assert_eq!(report.added, 7, "{report:?}");
    assert_eq!(report.probes_queued, 7);
    // Artists and albums get a refresh, tracks do not.
    assert_eq!(report.refreshes_queued, 4 + 4, "{report:?}");

    assert_eq!(
        titles(&f.db, f.library, "artist").await,
        [
            "Anna Alpha",
            "Bert Beta",
            "Carla Cello",
            "Verschiedene Interpreten"
        ]
    );
    assert_eq!(
        titles(&f.db, f.library, "album").await,
        ["Doppelalbum", "Ohne Tags", "Sommer", "Sommerhits"]
    );

    // Full tags: numbers, ids, ReplayGain, genres, embedded cover, sidecar lyrics.
    let sommer = by_title(&f.db, f.library, "album", "Sommer").await;
    let anna = by_title(&f.db, f.library, "artist", "Anna Alpha").await;
    assert_eq!(sommer.parent_id, Some(anna.id));
    assert_eq!(
        sommer.content_key,
        "mb:release:11111111-1111-1111-1111-111111111111"
    );
    assert_eq!(anna.content_key, "local:artist:anna alpha");
    assert_eq!(
        items::provider_ids(&f.db, anna.id).await.unwrap(),
        [(
            "musicbrainz_artist".to_owned(),
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_owned()
        )]
    );
    let tracks = items::children(&f.db, sommer.id).await.unwrap();
    let numbers: Vec<_> = tracks
        .iter()
        .map(|t| {
            (
                t.title.as_str(),
                t.parent_index_number,
                t.index_number,
                t.series_id,
            )
        })
        .collect();
    assert_eq!(
        numbers,
        [
            ("Erster Song", Some(1), Some(1), Some(anna.id)),
            ("Zweiter Song", Some(1), Some(2), Some(anna.id)),
        ]
    );
    assert_eq!(
        tracks[0].content_key,
        "mb:track:22222222-2222-2222-2222-000000000001"
    );
    let file = &media::files_for_item(&f.db, tracks[0].id).await.unwrap()[0];
    let tags = music::tags(&f.db, file.id).await.unwrap().unwrap();
    assert_eq!(tags.track_gain_db, Some(-6.5));
    assert_eq!(tags.track_peak, Some(0.95));
    assert_eq!(tags.album_gain_db, Some(-7.25));
    assert_eq!(tags.genres, ["Pop", "Elektro"]);
    assert_eq!(tags.year, Some(2020));
    assert!(tags.has_cover, "attached picture");
    assert!(tags.has_lyrics, "sidecar .lrc");
    assert!(
        tags.duration_ms.is_some_and(|d| (1900..=2200).contains(&d)),
        "{tags:?}"
    );
    assert_eq!(
        metadata::genres(&f.db, tracks[0].id).await.unwrap(),
        ["Elektro", "Pop"]
    );

    // Two discs in CD1/CD2 folders are one album.
    let doppel = by_title(&f.db, f.library, "album", "Doppelalbum").await;
    let discs: Vec<_> = items::children(&f.db, doppel.id)
        .await
        .unwrap()
        .iter()
        .map(|t| (t.parent_index_number, t.index_number))
        .collect();
    assert_eq!(discs, [(Some(1), Some(1)), (Some(2), Some(1))]);

    // The compilation files under "Verschiedene Interpreten", track artists kept.
    let hits = by_title(&f.db, f.library, "album", "Sommerhits").await;
    let various = by_title(&f.db, f.library, "artist", "Verschiedene Interpreten").await;
    assert_eq!(hits.parent_id, Some(various.id));
    let artists: Vec<Option<String>> = {
        let mut out = Vec::new();
        for track in items::children(&f.db, hits.id).await.unwrap() {
            let file = &media::files_for_item(&f.db, track.id).await.unwrap()[0];
            out.push(music::tags(&f.db, file.id).await.unwrap().unwrap().artist);
        }
        out
    };
    assert_eq!(
        artists,
        [Some("Anna Alpha".to_owned()), Some("Dora Delta".to_owned())]
    );

    // No tags at all: artist, album, number and title from the folders.
    let nackt = by_title(&f.db, f.library, "track", "Nackt").await;
    assert_eq!(nackt.index_number, Some(3));
    let ohne = by_title(&f.db, f.library, "album", "Ohne Tags").await;
    assert_eq!(nackt.parent_id, Some(ohne.id));

    // Probing works on audio files.
    assert_eq!(run_probes(&f.db).await, 7);
    let probed = media::get_file(&f.db, file.id).await.unwrap().unwrap();
    assert!(probed.duration_ms.is_some_and(|d| d >= 1900), "{probed:?}");

    // A rescan without changes writes nothing.
    let again = scan_library(&f.db, &f.queue, f.library).await.unwrap();
    assert_eq!((again.added, again.changed, again.unchanged), (0, 0, 7));
}

#[tokio::test]
async fn album_refresh_rolls_up_tags_and_finds_artwork() {
    let f = fixture().await;
    scan_library(&f.db, &f.queue, f.library).await.unwrap();
    run_probes(&f.db).await;
    // Offline: MusicBrainz switched off.
    flixnet_db::settings::set(
        &f.db,
        flixnet_library::providers::SETTINGS_KEY,
        &serde_json::json!({ "musicbrainz_enabled": false }),
    )
    .await
    .unwrap();
    for kind in ["album", "artist"] {
        for item in items::list_by_kind(&f.db, f.library, kind).await.unwrap() {
            flixnet_library::music::refresh(&f.db, item.id)
                .await
                .unwrap();
        }
    }

    let sommer = by_title(&f.db, f.library, "album", "Sommer").await;
    let details = items::details(&f.db, sommer.id).await.unwrap().unwrap();
    assert_eq!(details.year, Some(2020));
    assert!(
        details
            .runtime_ms
            .is_some_and(|r| (3800..=4400).contains(&r)),
        "two 2 s tracks: {details:?}"
    );
    assert_eq!(
        metadata::genres(&f.db, sommer.id).await.unwrap(),
        ["Elektro", "Pop"]
    );
    // Folder cover first, embedded cover second.
    let images = metadata::images(&f.db, sommer.id).await.unwrap();
    let posters: Vec<(&str, bool)> = images
        .iter()
        .filter(|i| i.kind == "poster")
        .map(|i| {
            (
                i.source.as_str(),
                i.path.as_deref().is_some_and(|p| p.ends_with("cover.jpg")),
            )
        })
        .collect();
    assert_eq!(posters, [("local", true), ("embedded", false)]);

    // The embedded cover is served by the image cache (decoded from the tags).
    let embedded = images.iter().find(|i| i.source == "embedded").unwrap();
    let cache = ImageCache::new(f.dir.path().join("cache"));
    let variant = cache.variant(embedded, Some(92)).await.unwrap();
    let decoded = image::open(&variant.path).unwrap();
    assert_eq!(decoded.width(), 32, "the 32 px PNG from the tags");
    assert!(variant.blurhash.is_some());

    // The double album only has embedded art; the artist borrows an album cover.
    let doppel = by_title(&f.db, f.library, "album", "Doppelalbum").await;
    let doppel_images = metadata::images(&f.db, doppel.id).await.unwrap();
    assert_eq!(doppel_images.len(), 1);
    assert_eq!(doppel_images[0].source, "embedded");
    let bert = by_title(&f.db, f.library, "artist", "Bert Beta").await;
    let bert_images = metadata::images(&f.db, bert.id).await.unwrap();
    assert_eq!(bert_images.len(), 1);
    assert_eq!(
        (bert_images[0].kind.as_str(), bert_images[0].source.as_str()),
        ("poster", "album")
    );
    let doppel_details = items::details(&f.db, doppel.id).await.unwrap().unwrap();
    assert_eq!(doppel_details.year, Some(2018));

    // Lyrics: the sidecar .lrc, synced.
    let first = f
        .root
        .join("Anna Alpha/Sommer (2020)/01 - Erster Song.flac");
    let found = lyrics::find(&first).unwrap();
    assert!(found.synced);
    assert_eq!(found.lines[0].time_ms, 500);
    assert_eq!(found.lines[1].text, "Hallo Meer");
    assert!(
        lyrics::find(
            &f.root
                .join("Anna Alpha/Sommer (2020)/02 - Zweiter Song.flac")
        )
        .is_none()
    );
}

#[tokio::test]
async fn retagged_files_move_and_empty_albums_disappear() {
    let f = fixture().await;
    scan_library(&f.db, &f.queue, f.library).await.unwrap();
    let ohne = by_title(&f.db, f.library, "album", "Ohne Tags").await;
    let nackt = by_title(&f.db, f.library, "track", "Nackt").await;
    let file_id = media::files_for_item(&f.db, nackt.id).await.unwrap()[0].id;

    // Re-tag the untagged WAV (rewritten as FLAC under the same name would change the
    // extension; rewrite the WAV with INFO tags instead).
    let path = f.root.join("Carla Cello/Ohne Tags/03 - Nackt.wav");
    let tmp = f.dir.path().join("retag.wav");
    encode(
        &tmp,
        1,
        &[
            ("title", "Angezogen"),
            ("artist", "Carla Cello"),
            ("album", "Mit Tags"),
        ],
        None,
        &["-c:a", "pcm_s16le"],
    );
    // A different size/mtime makes it a changed file.
    std::fs::copy(&tmp, &path).unwrap();
    let mtime = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(mtime)
        .unwrap();

    let report = scan_library(&f.db, &f.queue, f.library).await.unwrap();
    assert_eq!(report.changed, 1, "{report:?}");
    let moved = media::get_file(&f.db, file_id).await.unwrap().unwrap();
    let track = items::get(&f.db, moved.item_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(track.title, "Angezogen");
    let album = items::get(&f.db, track.parent_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(album.title, "Mit Tags");
    assert!(
        items::get(&f.db, ohne.id).await.unwrap().is_none(),
        "the emptied album is pruned"
    );
    assert!(items::get(&f.db, nackt.id).await.unwrap().is_none());
    assert_eq!(
        titles(&f.db, f.library, "artist").await,
        [
            "Anna Alpha",
            "Bert Beta",
            "Carla Cello",
            "Verschiedene Interpreten"
        ],
        "the artist stays (same name)"
    );
}
