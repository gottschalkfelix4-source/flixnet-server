//! Tuner imports, guide refresh, matching and HDHomeRun discovery against the fake
//! source (no real IPTV, no real device).

mod fake;

use std::time::Duration;

use flixnet_db::Db;
use flixnet_db::livetv::{self, NewTunerHost};
use flixnet_livetv::{guide, hdhomerun, import, matching};

async fn db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    (dir, db)
}

async fn host(db: &Db, kind: &str, url: &str, credentials: Option<(&str, &str)>) -> i64 {
    livetv::create_host(
        db,
        &NewTunerHost {
            kind,
            name: kind,
            url,
            username: credentials.map(|c| c.0),
            password: credentials.map(|c| c.1),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn m3u_import_keeps_matches_and_drops_removed_channels() {
    let fake = fake::start().await;
    let (_dir, db) = db().await;
    let client = flixnet_livetv::http::client();
    let id = host(&db, "m3u", &fake.url("/playlist.m3u"), None).await;

    let report = import::import_host(&db, &client, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.channels, 4);
    let channels = livetv::list_channels(&db, false).await.unwrap();
    let names: Vec<_> = channels.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Das Erste HD", "ZDF HD", "DE: arte", "Kaputt TV"]);
    assert_eq!(channels[0].tvg_id.as_deref(), Some("daserste.de"));
    assert_eq!(channels[0].group_title.as_deref(), Some("Vollprogramm"));
    assert!(
        channels[0]
            .logo_url
            .as_deref()
            .unwrap()
            .ends_with("/logo/ard.png")
    );
    assert!(channels[0].is_hd);
    let host = livetv::get_host(&db, id).await.unwrap().unwrap();
    assert_eq!(host.channel_count, 4);
    assert!(host.last_import_at.is_some() && host.last_error.is_none());

    // A manual guide choice survives re-imports; a vanished channel goes.
    livetv::set_epg_matches(
        &db,
        &[livetv::EpgMatch {
            channel_id: channels[1].id,
            epg_source_id: None,
            epg_channel_id: None,
            kind: Some("manual"),
            score: None,
        }],
    )
    .await
    .unwrap();
    livetv::set_channel_enabled(&db, channels[3].id, false)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    import::import_host(&db, &client, id).await.unwrap();
    let again = livetv::list_channels(&db, false).await.unwrap();
    assert_eq!(again.len(), 4);
    assert_eq!(again[1].id, channels[1].id, "same row, not a new one");
    assert_eq!(again[1].epg_match.as_deref(), Some("manual"));
    assert!(!again[3].enabled, "disabled stays disabled");
    assert_eq!(livetv::list_channels(&db, true).await.unwrap().len(), 3);

    // An unreachable playlist records the error on the host.
    livetv::update_host(
        &db,
        id,
        &livetv::HostUpdate {
            url: Some(&fake.url("/gibt-es-nicht.m3u")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(import::import_host(&db, &client, id).await.is_err());
    let host = livetv::get_host(&db, id).await.unwrap().unwrap();
    assert!(
        host.last_error.as_deref().unwrap().contains("404"),
        "{:?}",
        host.last_error
    );
    assert_eq!(host.channel_count, 4, "a failed import removes nothing");
}

#[tokio::test]
async fn xtream_and_hdhomerun_lineups() {
    let fake = fake::start().await;
    let (_dir, db) = db().await;
    let client = flixnet_livetv::http::client();

    let xtream = host(
        &db,
        "xtream",
        &fake.base,
        Some((fake::USER, fake::PASSWORD)),
    )
    .await;
    import::import_host(&db, &client, xtream).await.unwrap();
    let host_row = livetv::get_host(&db, xtream).await.unwrap().unwrap();
    assert_eq!(host_row.tuner_count, 1, "max_connections of the account");
    let channels = livetv::list_channels(&db, false).await.unwrap();
    assert_eq!(channels.len(), 2);
    assert_eq!(channels[0].remote_id, "101");
    assert_eq!(
        channels[0].group_title.as_deref(),
        Some("DE | Vollprogramm")
    );
    assert!(channels[0].stream_url.ends_with(&format!(
        "/live/{}/{}/101.ts",
        fake::USER,
        fake::PASSWORD
    )));

    let wrong = host(&db, "xtream", &fake.base, Some((fake::USER, "falsch"))).await;
    assert!(import::import_host(&db, &client, wrong).await.is_err());
    let wrong_row = livetv::get_host(&db, wrong).await.unwrap().unwrap();
    assert!(wrong_row.last_error.is_some());

    let address = fake.base.trim_start_matches("http://");
    let hdhr = host(&db, "hdhomerun", address, None).await;
    import::import_host(&db, &client, hdhr).await.unwrap();
    let device = livetv::get_host(&db, hdhr).await.unwrap().unwrap();
    assert_eq!(device.device_id.as_deref(), Some("1013ABCD"));
    assert_eq!(device.tuner_count, 2);
    let lineup: Vec<_> = livetv::list_channels(&db, false)
        .await
        .unwrap()
        .into_iter()
        .filter(|c| c.tuner_host_id == hdhr)
        .collect();
    assert_eq!(lineup.len(), 2, "DRM channels are left out");
    assert_eq!(lineup[0].number.as_deref(), Some("1"));
    assert_eq!(lineup[1].number.as_deref(), Some("2.1"));
    assert_eq!(lineup[1].sort_order, 2.1);
    assert_eq!(lineup[1].video_codec.as_deref(), Some("mpeg2video"));
    assert_eq!(lineup[1].audio_codec.as_deref(), Some("mp2"));

    // A tuner count set by the admin wins over the device's.
    livetv::update_host(
        &db,
        hdhr,
        &livetv::HostUpdate {
            tuner_count: Some(Some(1)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    import::import_host(&db, &client, hdhr).await.unwrap();
    assert_eq!(
        livetv::get_host(&db, hdhr)
            .await
            .unwrap()
            .unwrap()
            .tuner_count,
        1
    );
}

#[tokio::test]
async fn udp_discovery_finds_the_fake_device() {
    let fake = fake::start().await;
    let found = hdhomerun::discover(&[fake.discovery], Duration::from_millis(500))
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].device_id, "1013ABCD");
    assert_eq!(found[0].tuner_count, Some(2));
    assert_eq!(found[0].base_url, fake.base);
    // The base URL leads to the device's description.
    let info = hdhomerun::device_info(&flixnet_livetv::http::client(), &found[0].base_url)
        .await
        .unwrap();
    assert_eq!(info.model_number.as_deref(), Some("HDTC-2US"));
}

#[tokio::test]
async fn guide_refresh_matching_and_search() {
    let fake = fake::start().await;
    let (dir, db) = db().await;
    let client = flixnet_livetv::http::client();
    let tuner = host(&db, "m3u", &fake.url("/playlist.m3u"), None).await;
    import::import_host(&db, &client, tuner).await.unwrap();

    let source = livetv::create_source(&db, "Test-EPG", &fake.url("/guide.xml.gz"))
        .await
        .unwrap();
    let cache = dir.path().join("cache/epg");
    let report = guide::refresh_source(&db, &client, &cache, source)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.channels, 3);
    // 16 slots per channel, minus those that ended more than 12 h ago (none).
    assert_eq!(report.programs, 48);
    let row = livetv::get_source(&db, source).await.unwrap().unwrap();
    assert_eq!((row.channel_count, row.program_count), (3, 48));

    let matched = matching::auto_match(&db).await.unwrap();
    assert_eq!(matched, 3, "ARD by tvg-id, ZDF and arte by name");
    let channels = livetv::list_channels(&db, false).await.unwrap();
    let guide_of = |name: &str| {
        let c = channels.iter().find(|c| c.name == name).unwrap();
        (c.epg_channel_id.clone(), c.epg_match.clone())
    };
    assert_eq!(
        guide_of("Das Erste HD"),
        (Some("daserste.de".into()), Some("auto".into()))
    );
    assert_eq!(
        guide_of("ZDF HD"),
        (Some("zdf.de".into()), Some("auto".into()))
    );
    assert_eq!(
        guide_of("DE: arte"),
        (Some("arte.de".into()), Some("auto".into()))
    );
    assert_eq!(guide_of("Kaputt TV"), (None, None));

    let now = flixnet_db::now_ms();
    let ard = channels
        .iter()
        .find(|c| c.name == "Das Erste HD")
        .unwrap()
        .id;
    let current = livetv::programs_between(&db, &[ard], now, now + 1)
        .await
        .unwrap();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].title, "Tagesschau");
    assert_eq!(current[0].channel, ard);
    assert!(current[0].start_ms <= now && now < current[0].end_ms);
    assert_eq!(
        current[0].description.as_deref(),
        Some("Beschreibung von Tagesschau & mehr.")
    );
    assert_eq!(current[0].category_list(), ["Unterhaltung"]);

    let fts = flixnet_db::browse::fts_query("tagessch").unwrap();
    let found = livetv::search_programs(&db, &fts, now, 10).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].channel, ard);

    // A second refresh replaces the programs instead of adding to them.
    guide::refresh_source(&db, &client, &cache, source)
        .await
        .unwrap();
    let all = livetv::programs_between(&db, &[ard], 0, i64::MAX)
        .await
        .unwrap();
    assert_eq!(all.len(), 16);
    let rows: i64 = visible_programs(&db).await;
    assert_eq!(rows, 48, "only the new generation is visible");

    // A broken source keeps the last good guide and records the error.
    livetv::update_source(&db, source, None, Some(&fake.url("/kaputt.xml")), None)
        .await
        .unwrap();
    assert!(
        guide::refresh_source(&db, &client, &cache, source)
            .await
            .is_err()
    );
    let row = livetv::get_source(&db, source).await.unwrap().unwrap();
    assert!(row.last_error.is_some());
    assert_eq!(
        livetv::programs_between(&db, &[ard], 0, i64::MAX)
            .await
            .unwrap()
            .len(),
        16
    );

    // Deleting the source unmatches its channels.
    livetv::delete_source(&db, source).await.unwrap();
    let channels = livetv::list_channels(&db, false).await.unwrap();
    assert!(channels.iter().all(|c| c.epg_channel_id.is_none()));
}

async fn visible_programs(db: &Db) -> i64 {
    // Every visible program of every channel, all time.
    let channels = livetv::list_channels(db, false).await.unwrap();
    let ids: Vec<i64> = channels.iter().map(|c| c.id).collect();
    livetv::programs_between(db, &ids, i64::MIN, i64::MAX)
        .await
        .unwrap()
        .len() as i64
}
