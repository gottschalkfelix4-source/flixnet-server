//! Schedules Direct and tvheadend against wiremock: sign-in, lineups, schedules and
//! programs in batches; tvheadend channel list, stream URLs and EPG paging.

use flixnet_livetv::guide::GuideSource;
use flixnet_livetv::schedules_direct::{self, SchedulesDirect, SdCredentials};
use flixnet_livetv::tuner::Tuner;
use flixnet_livetv::tvheadend::Tvheadend;
use serde_json::json;
use wiremock::matchers::{basic_auth, body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// 2026-10-30 00:00 UTC.
const DAY: i64 = 1_793_318_400_000;

fn credentials(server: &MockServer) -> SdCredentials {
    SdCredentials {
        username: "felix".into(),
        password_sha1: schedules_direct::password_hash("geheim"),
        base_url: Some(server.uri()),
    }
}

async fn sd_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_partial_json(json!({
            "username": "felix",
            "password": "906072001efddf3e11e6d2b5782f4777fe038739"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0, "message": "OK", "token": "tok-123"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "code": 4003, "message": "Invalid username or token."
        })))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn schedules_direct_guide_in_batches() {
    let server = sd_server().await;
    Mock::given(method("GET"))
        .and(path("/lineups"))
        .and(header("token", "tok-123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "lineups": [{ "lineup": "DEU-1000097-DEFAULT", "name": "Kabel", "location": "Berlin" }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/lineups/DEU-1000097-DEFAULT"))
        .and(header("token", "tok-123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "map": [
                { "stationID": "10001", "channel": "1" },
                { "stationID": "10002", "channel": "2" }
            ],
            "stations": [
                { "stationID": "10001", "name": "Das Erste HD", "callsign": "ARDHD",
                  "stationLogo": [{ "URL": "https://logos.example/ard.png" }] },
                { "stationID": "10002", "name": "ZDF HD", "callsign": "ZDFHD" }
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/schedules"))
        .and(header("token", "tok-123"))
        .and(body_partial_json(json!([{ "stationID": "10001", "date": ["2026-10-30"] }])))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "stationID": "10001", "programs": [
                { "programID": "EP000000060003", "airDateTime": "2026-10-30T19:15:00Z",
                  "duration": 5400, "new": true,
                  "ratings": [{ "body": "Freiwillige Selbstkontrolle", "code": "12" }] },
                { "programID": "MV000000010000", "airDateTime": "2026-10-30T21:00:00Z", "duration": 7200 }
            ]},
            { "stationID": "10002", "programs": [
                { "programID": "EP000000060004", "airDateTime": "2026-10-30T19:15:00Z", "duration": 5400 }
            ]}
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/programs"))
        .and(header("token", "tok-123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "programID": "EP000000060003",
              "titles": [{ "title120": "Tatort" }],
              "episodeTitle150": "Der Fall",
              "descriptions": { "description1000": [
                  { "descriptionLanguage": "en", "description": "A case." },
                  { "descriptionLanguage": "de", "description": "Ein Fall." }
              ]},
              "genres": ["Krimi"],
              "metadata": [{ "Gracenote": { "season": 54, "episode": 3 } }] },
            { "programID": "EP000000060004", "titles": [{ "title120": "Tatort" }] },
            { "programID": "MV000000010000", "titles": [{ "title120": "Matrix" }],
              "movie": { "year": "1999" }, "contentRating": [{ "body": "FSK", "code": "FSK 16" }] }
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let client = flixnet_livetv::http::client();
    let lineups = schedules_direct::lineups(&client, &credentials(&server))
        .await
        .unwrap();
    assert_eq!(lineups[0].lineup, "DEU-1000097-DEFAULT");
    assert_eq!(lineups[0].location.as_deref(), Some("Berlin"));

    let guide = SchedulesDirect {
        credentials: credentials(&server),
        lineup: "DEU-1000097-DEFAULT".into(),
        from_ms: DAY + 3_600_000,
    }
    .fetch(&client)
    .await
    .unwrap();
    assert_eq!(guide.channels.len(), 2);
    let (id, names, logo) = &guide.channels[0];
    assert_eq!(id, "10001");
    assert_eq!(names, &["Das Erste HD", "ARDHD", "1"]);
    assert_eq!(logo.as_deref(), Some("https://logos.example/ard.png"));

    assert_eq!(guide.programs.len(), 3);
    let tatort = guide
        .programs
        .iter()
        .find(|p| p.channel_id == "10001" && p.title == "Tatort")
        .unwrap();
    assert_eq!(tatort.start_ms, DAY + 19 * 3_600_000 + 15 * 60_000);
    assert_eq!(tatort.end_ms - tatort.start_ms, 5_400_000);
    assert_eq!(tatort.subtitle.as_deref(), Some("Der Fall"));
    assert_eq!(
        tatort.description.as_deref(),
        Some("Ein Fall."),
        "German first"
    );
    assert_eq!((tatort.season, tatort.episode), (Some(54), Some(3)));
    assert_eq!(tatort.episode_label.as_deref(), Some("S54E03"));
    assert_eq!(tatort.rating.as_deref(), Some("FSK 12"));
    assert_eq!(tatort.series_id.as_deref(), Some("SH00000006"));
    assert_eq!(tatort.categories, ["Krimi"]);
    assert!(tatort.is_new);
    let matrix = guide.programs.iter().find(|p| p.title == "Matrix").unwrap();
    assert_eq!(matrix.year, Some(1999));
    assert_eq!(matrix.rating.as_deref(), Some("FSK 16"));
    assert_eq!(matrix.series_id, None);

    // Wrong password: a clear error.
    let wrong = SdCredentials {
        password_sha1: schedules_direct::password_hash("falsch"),
        ..credentials(&server)
    };
    let err = schedules_direct::token(&client, &wrong).await.unwrap_err();
    assert!(err.to_string().contains("Invalid username"), "{err}");
}

#[tokio::test]
async fn tvheadend_channels_streams_and_epg_pages() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/channel/grid"))
        .and(basic_auth("felix", "geheim"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "entries": [
                { "uuid": "a1b2c3", "name": "Das Erste HD", "number": 1, "enabled": true,
                  "icon_public_url": "imagecache/12" },
                { "uuid": "d4e5f6", "name": "arte", "number": 7.1, "enabled": true },
                { "uuid": "ffff00", "name": "Aus", "number": 99, "enabled": false }
            ],
            "total": 3
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/channel/grid"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let event = |id: i64, channel: &str, start: i64| {
        json!({ "eventId": id, "channelUuid": channel, "start": start, "stop": start + 1800,
                "title": format!("Sendung {id}"), "subtitle": "Folge", "description": "Text",
                "seasonNumber": 2, "episodeNumber": id, "ageRating": 16,
                "serieslinkUri": "crid://serie/1", "category": ["Serie"] })
    };
    let page_one: Vec<_> = (0..2000)
        .map(|i| event(i, "a1b2c3", DAY / 1000 + i * 1800))
        .collect();
    Mock::given(method("GET"))
        .and(path("/api/epg/events/grid"))
        .and(query_param("start", "0"))
        .and(basic_auth("felix", "geheim"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "entries": page_one, "totalCount": 2001
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/epg/events/grid"))
        .and(query_param("start", "2000"))
        .and(basic_auth("felix", "geheim"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "entries": [event(5000, "d4e5f6", DAY / 1000)], "totalCount": 2001
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = flixnet_livetv::http::client();
    let tvh = Tvheadend {
        url: server.uri(),
        username: Some("felix".into()),
        password: Some("geheim".into()),
    };
    let lineup = tvh.lineup(&client).await.unwrap();
    assert_eq!(lineup.channels.len(), 2, "disabled channels are left out");
    let ard = &lineup.channels[0];
    assert_eq!(ard.remote_id, "a1b2c3");
    assert_eq!(
        ard.tvg_id.as_deref(),
        Some("a1b2c3"),
        "matched to the EPG by uuid"
    );
    assert_eq!(ard.number.as_deref(), Some("1"));
    assert!(ard.is_hd);
    let port = server.address().port();
    assert_eq!(
        ard.stream_url,
        format!("http://felix:geheim@127.0.0.1:{port}/stream/channel/a1b2c3?profile=pass")
    );
    assert!(flixnet_livetv::tuner::is_stream_url(&ard.stream_url));
    assert_eq!(
        ard.logo_url.as_deref(),
        Some(format!("{}/imagecache/12", server.uri()).as_str())
    );
    assert_eq!(lineup.channels[1].number.as_deref(), Some("7.1"));

    let guide = tvh.fetch(&client).await.unwrap();
    assert_eq!(guide.channels.len(), 2);
    assert_eq!(guide.channels[0].1, ["Das Erste HD", "1"]);
    assert_eq!(guide.programs.len(), 2001, "both pages");
    let first = &guide.programs[1];
    assert_eq!(first.start_ms, DAY + 1_800_000);
    assert_eq!(first.end_ms - first.start_ms, 1_800_000);
    assert_eq!(first.rating.as_deref(), Some("FSK 16"));
    assert_eq!((first.season, first.episode), (Some(2), Some(1)));
    assert_eq!(first.series_id.as_deref(), Some("crid://serie/1"));
    assert_eq!(first.categories, ["Serie"]);

    // Without the right account: refused.
    let anonymous = Tvheadend {
        url: server.uri(),
        username: None,
        password: None,
    };
    let err = anonymous.lineup(&client).await.unwrap_err();
    assert!(err.to_string().contains("Anmeldung"), "{err}");
}
