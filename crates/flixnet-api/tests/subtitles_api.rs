//! Subtitles and track switching with real ffmpeg: text subtitles (embedded SubRip
//! and a sidecar file) as signed WebVTT, picture subtitles (PGS) burned into HLS.

mod common;

use std::path::Path;
use std::process::Command;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use common::{TestApp, app};
use flixnet_api::signing::UrlSigner;
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::{libraries, media};
use flixnet_library::scan::scan_library;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn raw(app: &TestApp, uri: &str, bearer: Option<&str>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().uri(uri);
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = app
        .router
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes.to_vec())
}

const W: u16 = 320;
const H: u16 = 240;
/// Where the PGS subtitles draw a white box: x, y, width, height.
const BOX: (u16, u16, u16, u16) = (110, 180, 100, 30);

/// One PGS segment: magic, PTS (90 kHz), DTS, type, size, payload.
fn pgs_segment(pts_ms: u32, kind: u8, data: &[u8]) -> Vec<u8> {
    let mut out = b"PG".to_vec();
    out.extend((pts_ms * 90).to_be_bytes());
    out.extend(0u32.to_be_bytes());
    out.push(kind);
    out.extend(u16::try_from(data.len()).unwrap().to_be_bytes());
    out.extend(data);
    out
}

/// Presentation composition: an epoch start showing object 0, or an empty one.
fn pgs_composition(number: u16, show: bool) -> Vec<u8> {
    let mut pcs = Vec::new();
    for v in [W, H] {
        pcs.extend(v.to_be_bytes());
    }
    pcs.push(0x10); // frame rate
    pcs.extend(number.to_be_bytes());
    pcs.extend([if show { 0x80 } else { 0x00 }, 0, 0, u8::from(show)]);
    if show {
        pcs.extend(0u16.to_be_bytes()); // object id
        pcs.extend([0, 0]); // window id, not cropped
        pcs.extend(BOX.0.to_be_bytes());
        pcs.extend(BOX.1.to_be_bytes());
    }
    pcs
}

fn pgs_window() -> Vec<u8> {
    let mut wds = vec![1, 0];
    for v in [BOX.0, BOX.1, BOX.2, BOX.3] {
        wds.extend(v.to_be_bytes());
    }
    wds
}

/// A white box from `start` to `end` (ms).
fn pgs_box(start: u32, end: u32, number: u16) -> Vec<u8> {
    let (_, _, w, h) = BOX;
    // Palette entry 1: white (Y, Cr, Cb, alpha).
    let pds = [0, 0, 1, 235, 128, 128, 255];
    // Run-length lines: `00 C(w) w color`, then end of line.
    let mut rle = Vec::new();
    for _ in 0..h {
        rle.extend([0, 0xC0 | (w >> 8) as u8, (w & 0xFF) as u8, 1, 0, 0]);
    }
    let mut ods = vec![0, 0, 0, 0xC0];
    ods.extend(&u32::try_from(rle.len() + 4).unwrap().to_be_bytes()[1..]);
    ods.extend(w.to_be_bytes());
    ods.extend(h.to_be_bytes());
    ods.extend(&rle);
    let mut out = Vec::new();
    out.extend(pgs_segment(start, 0x16, &pgs_composition(number, true)));
    out.extend(pgs_segment(start, 0x17, &pgs_window()));
    out.extend(pgs_segment(start, 0x14, &pds));
    out.extend(pgs_segment(start, 0x15, &ods));
    out.extend(pgs_segment(start, 0x80, &[]));
    out.extend(pgs_clear(end, number + 1));
    out
}

fn pgs_clear(at: u32, number: u16) -> Vec<u8> {
    let mut out = pgs_segment(at, 0x16, &pgs_composition(number, false));
    out.extend(pgs_segment(at, 0x17, &pgs_window()));
    out.extend(pgs_segment(at, 0x80, &[]));
    out
}

fn ffmpeg(args: &[&str]) {
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .status()
        .expect("ffmpeg on PATH");
    assert!(status.success(), "ffmpeg {args:?}");
}

/// 30 s of black video (2 s GOPs) with German (default) and English audio, embedded
/// English SubRip, German PGS (white box 1–4 s and 20–28 s) and a German forced
/// sidecar `.srt` in Windows-1252.
fn make_movie(folder: &Path) {
    std::fs::create_dir_all(folder).unwrap();
    let srt = folder.join("embedded.tmp.srt");
    std::fs::write(
        &srt,
        "1\n00:00:01,000 --> 00:00:04,000\nHello world\n\n2\n00:00:10,000 --> 00:00:14,000\nSecond line\n",
    )
    .unwrap();
    // Starts with an empty composition at 0 so ffmpeg keeps the stream's timing.
    let mut sup = pgs_clear(0, 0);
    sup.extend(pgs_box(1_000, 4_000, 1));
    sup.extend(pgs_box(20_000, 28_000, 3));
    let sup_path = folder.join("embedded.tmp.sup");
    std::fs::write(&sup_path, sup).unwrap();
    let movie = folder.join("Spuren.2020.mkv");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=320x240:r=25:d=30",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=30",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=880:duration=30",
        "-i",
        srt.to_str().unwrap(),
        "-i",
        sup_path.to_str().unwrap(),
        "-map",
        "0",
        "-map",
        "1",
        "-map",
        "2",
        "-map",
        "3",
        "-map",
        "4",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-g",
        "50",
        "-c:a",
        "aac",
        "-c:s:0",
        "srt",
        "-c:s:1",
        "copy",
        "-metadata:s:a:0",
        "language=ger",
        "-metadata:s:a:1",
        "language=eng",
        "-metadata:s:s:0",
        "language=eng",
        "-metadata:s:s:1",
        "language=ger",
        "-disposition:a:0",
        "default",
        "-disposition:a:1",
        "0",
        movie.to_str().unwrap(),
    ]);
    std::fs::remove_file(srt).unwrap();
    std::fs::remove_file(sup_path).unwrap();
    // "Grüße" and „…“ in Windows-1252.
    std::fs::write(
        folder.join("Spuren.2020.de.forced.srt"),
        b"1\r\n00:00:02,500 --> 00:00:05,000\r\nGr\xfc\xdfe \x84aus Berlin\x93\r\n",
    )
    .unwrap();
}

struct Fixture {
    token: String,
    movie: i64,
    file_id: i64,
}

async fn library(app: &TestApp, root: &Path) -> Fixture {
    make_movie(&root.join("Filme/Spuren (2020)"));
    let library = libraries::create(
        &app.db,
        "Filme",
        "movies",
        &[root.join("Filme").to_string_lossy().into_owned()],
    )
    .await
    .unwrap()
    .id;
    let queue = JobRuntime::new(app.db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    scan_library(&app.db, &queue, library).await.unwrap();
    let ffprobe = flixnet_media::Ffprobe::new("ffprobe");
    let file_id = media::file_states(&app.db, library).await.unwrap()[0].id;
    flixnet_library::jobs::probe_file(&app.db, &ffprobe, file_id)
        .await
        .unwrap();
    let token = app.setup_admin().await;
    let movie = app.get("/api/v1/items?kind=movie", Some(&token)).await.json["items"][0]["id"]
        .as_i64()
        .unwrap();
    Fixture {
        token,
        movie,
        file_id,
    }
}

async fn playback_info(app: &TestApp, f: &Fixture, body: Value) -> Value {
    let info = app
        .post(
            &format!("/api/v1/items/{}/playback-info", f.movie),
            body,
            Some(&f.token),
        )
        .await;
    assert_eq!(info.status, StatusCode::OK, "{}", info.json);
    info.json["sources"][0].clone()
}

#[tokio::test]
async fn playback_info_lists_tracks_and_serves_webvtt() {
    let app = app().await;
    let root = tempfile::tempdir().unwrap();
    let f = library(&app, root.path()).await;
    let source = playback_info(&app, &f, json!({})).await;

    let audio = &source["audio_tracks"];
    assert_eq!(audio.as_array().unwrap().len(), 2, "{source}");
    assert_eq!(
        (
            &audio[0]["index"],
            &audio[0]["language"],
            &audio[0]["is_default"]
        ),
        (&json!(1), &json!("ger"), &json!(true))
    );
    assert_eq!(
        (
            &audio[1]["index"],
            &audio[1]["language"],
            &audio[1]["is_default"]
        ),
        (&json!(2), &json!("eng"), &json!(false))
    );

    let subs = source["subtitle_tracks"].as_array().unwrap();
    assert_eq!(subs.len(), 3, "{source}");
    let (srt, pgs, sidecar) = (&subs[0], &subs[1], &subs[2]);
    assert_eq!(
        (&srt["index"], &srt["codec"]),
        (&json!(3), &json!("subrip"))
    );
    assert_eq!(srt["burn_in"], false);
    assert_eq!(
        (&pgs["index"], &pgs["codec"]),
        (&json!(4), &json!("hdmv_pgs_subtitle"))
    );
    assert_eq!(pgs["burn_in"], true);
    assert_eq!(pgs["vtt_url"], Value::Null, "pictures cannot become text");
    assert_eq!(sidecar["index"], 1000);
    assert_eq!(
        (
            &sidecar["language"],
            &sidecar["is_forced"],
            &sidecar["is_external"]
        ),
        (&json!("deu"), &json!(true), &json!(true))
    );

    // Embedded SubRip, fetched without a token: the signature is enough.
    let url = srt["vtt_url"].as_str().unwrap().to_owned();
    assert!(url.starts_with(&format!("/api/v1/subtitles/{}/3.vtt?u=1&", f.file_id)));
    let (status, headers, body) = raw(&app, &url, None).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(headers[header::CONTENT_TYPE], "text/vtt; charset=utf-8");
    let vtt = String::from_utf8(body).unwrap();
    assert!(vtt.starts_with("WEBVTT"), "{vtt}");
    assert!(
        vtt.contains("00:01.000 --> 00:04.000\nHello world"),
        "{vtt}"
    );
    assert!(vtt.contains("Second line"), "{vtt}");
    // Cached on disk under the config dir; served again from there.
    let cached: Vec<_> = std::fs::read_dir(app.dir.path().join("cache/subtitles"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(cached.len(), 1, "{cached:?}");
    assert!(cached[0].starts_with(&format!("{}-3-", f.file_id)) && cached[0].ends_with(".vtt"));
    assert_eq!(
        String::from_utf8(raw(&app, &url, None).await.2).unwrap(),
        vtt
    );

    // The sidecar file, converted from Windows-1252.
    let (status, _, body) = raw(&app, sidecar["vtt_url"].as_str().unwrap(), None).await;
    assert_eq!(status, StatusCode::OK);
    let vtt = String::from_utf8(body).unwrap();
    assert!(
        vtt.contains("00:02.500 --> 00:05.000\nGrüße „aus Berlin“"),
        "{vtt}"
    );

    // Bound to user and file; without a signature a token is needed.
    let base = format!("/api/v1/subtitles/{}/3.vtt", f.file_id);
    assert_eq!(raw(&app, &base, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(raw(&app, &base, Some(&f.token)).await.0, StatusCode::OK);
    let other_user = url.replace("u=1&", "u=2&");
    assert_eq!(
        raw(&app, &other_user, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let other_file = url.replace(&format!("/subtitles/{}/", f.file_id), "/subtitles/9999/");
    assert_eq!(
        raw(&app, &other_file, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let stream_sig = UrlSigner::new(b"test-secret".to_vec()).stream_url(f.file_id, 1);
    let query = stream_sig.split_once('?').unwrap().1;
    assert_eq!(
        raw(&app, &format!("{base}?{query}"), None).await.0,
        StatusCode::UNAUTHORIZED,
        "a stream signature is not a subtitle signature"
    );

    // Picture subtitles, unknown tracks and odd names have no WebVTT.
    for track in ["4.vtt", "0.vtt", "77.vtt", "3", "x.vtt", "..%2F3.vtt"] {
        let uri = format!("/api/v1/subtitles/{}/{track}", f.file_id);
        assert_eq!(
            raw(&app, &uri, Some(&f.token)).await.0,
            StatusCode::NOT_FOUND,
            "{track}"
        );
    }
}

#[tokio::test]
async fn choosing_tracks_changes_the_plan() {
    let app = app().await;
    let root = tempfile::tempdir().unwrap();
    let f = library(&app, root.path()).await;
    let browser = json!({
        "containers": ["mp4", "mkv"],
        "video_codecs": [{ "codec": "h264", "max_height": 2160 }],
        "audio_codecs": [{ "codec": "aac", "max_channels": 2 }],
    });

    let default = playback_info(&app, &f, json!({ "device_profile": browser })).await;
    assert_eq!(default["plan"]["method"], "direct_play");
    assert_eq!(default["hls_url"], Value::Null);

    // The English track: browsers cannot switch, so the server remuxes with it.
    let english = playback_info(
        &app,
        &f,
        json!({ "device_profile": browser, "audio_stream_index": 2 }),
    )
    .await;
    assert_eq!(english["plan"]["method"], "remux");
    assert_eq!(english["plan"]["audio"]["stream_index"], 2);
    assert!(english["hls_url"].is_string());

    // Text subtitles keep Direct Play; the plan names them as external.
    let text = playback_info(
        &app,
        &f,
        json!({ "device_profile": browser, "subtitle_stream_index": 1000 }),
    )
    .await;
    assert_eq!(text["plan"]["method"], "direct_play");
    assert_eq!(
        text["plan"]["subtitle"],
        json!({ "stream_index": 1000, "delivery": "external" })
    );

    // PGS: burned in by a transcode.
    let burn = playback_info(
        &app,
        &f,
        json!({ "device_profile": browser, "subtitle_stream_index": 4 }),
    )
    .await;
    assert_eq!(burn["plan"]["method"], "transcode");
    assert_eq!(
        burn["plan"]["subtitle"],
        json!({ "stream_index": 4, "delivery": "burn_in" })
    );
    assert_eq!(burn["plan"]["reasons"], json!(["subtitle_burn_in"]));
}

/// Brightness of the subtitle box in every frame of one HLS segment.
fn box_brightness(dir: &Path, init: &[u8], segment: &[u8]) -> Vec<u8> {
    let joined = dir.join("joined.mp4");
    std::fs::write(&joined, [init, segment].concat()).unwrap();
    let crop = format!("crop={}:{}:{}:{},scale=1:1", BOX.2, BOX.3, BOX.0, BOX.1);
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&joined)
        .args(["-vf", &crop, "-fps_mode", "passthrough"])
        .args(["-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .output()
        .expect("ffmpeg on PATH");
    assert!(output.status.success());
    output.stdout
}

/// End to end: the PGS track is drawn into the transcoded video at the right times,
/// also in a run that started at a later segment (a seek).
#[tokio::test]
async fn picture_subtitles_are_burned_in_and_stay_in_sync() {
    let app = app().await;
    let root = tempfile::tempdir().unwrap();
    let f = library(&app, root.path()).await;
    let source = playback_info(&app, &f, json!({ "subtitle_stream_index": 4 })).await;
    assert_eq!(source["plan"]["subtitle"]["delivery"], "burn_in");
    let url = source["hls_url"].as_str().unwrap();
    let base = url.trim_end_matches("main.m3u8");

    // Segment 3 (18–24 s) first: ffmpeg starts there. The box appears at 20 s.
    let (status, _, seg3) = raw(&app, &format!("{base}3.m4s"), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, init) = raw(&app, &format!("{base}init.mp4"), None).await;
    assert_eq!(status, StatusCode::OK);
    let frames = box_brightness(root.path(), &init, &seg3);
    assert_eq!(frames.len(), 150, "6 s at 25 fps");
    assert!(frames[..48].iter().all(|b| *b < 40), "18–20 s: {frames:?}");
    assert!(frames[52..].iter().all(|b| *b > 200), "20–24 s: {frames:?}");

    // Back to the start (another restart): the box is shown from 1 s to 4 s.
    let (status, _, seg0) = raw(&app, &format!("{base}0.m4s"), None).await;
    assert_eq!(status, StatusCode::OK);
    let frames = box_brightness(root.path(), &init, &seg0);
    assert_eq!(frames.len(), 150);
    assert!(frames[..23].iter().all(|b| *b < 40), "0–1 s: {frames:?}");
    assert!(frames[27..98].iter().all(|b| *b > 200), "1–4 s: {frames:?}");
    assert!(frames[102..].iter().all(|b| *b < 40), "4–6 s: {frames:?}");
}
