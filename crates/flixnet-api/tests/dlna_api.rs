//! DLNA: enabled through the network settings, browsed with SOAP over real HTTP as
//! a TV would, limited to what the chosen DLNA user may see.

mod common;

use std::collections::HashMap;

use axum::http::{Method, StatusCode};
use common::{PASSWORD, TestApp, app};
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::{items, libraries};
use flixnet_library::refresh::refresh_local;
use flixnet_library::scan::scan_library;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const MARKER: &str = "Geheim";

fn write(path: &std::path::Path, content: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// Films (one FSK 0, one FSK 18 with the marker), a photo library with an album and
/// an audiobook with two parts.
async fn libraries(app: &TestApp, root: &std::path::Path) {
    let films = root.join("Filme");
    for (dir, title, mpaa) in [
        ("Kinderfilm (2020)", "Kinderfilm", "FSK 0"),
        ("Geheimfilm (2019)", "Geheimfilm", "FSK 18"),
    ] {
        write(
            &films.join(dir).join(format!("{title}.mkv")),
            b"video-bytes",
        );
        write(
            &films.join(dir).join("movie.nfo"),
            format!(
                "<movie><title>{title}</title><mpaa>{mpaa}</mpaa><plot>Über {title}.</plot></movie>"
            )
            .as_bytes(),
        );
        image::RgbImage::new(10, 15)
            .save(films.join(dir).join("poster.png"))
            .unwrap();
    }
    let photos = root.join("Fotos");
    std::fs::create_dir_all(photos.join("Urlaub")).unwrap();
    for name in ["Urlaub/Strand.png", "Urlaub/Berg.png", "Wohnung.png"] {
        image::RgbImage::new(40, 30)
            .save(photos.join(name))
            .unwrap();
    }
    let books = root.join("Hörbücher");
    write(&books.join("Anna Autorin/Das Buch/01.mp3"), b"part-one");
    write(&books.join("Anna Autorin/Das Buch/02.mp3"), b"part-two");

    let queue = JobRuntime::new(app.db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    for (name, kind, path) in [
        ("Filme", "movies", films),
        ("Fotos", "photos", photos),
        ("Hörbücher", "audiobooks", books),
    ] {
        let library =
            libraries::create(&app.db, name, kind, &[path.to_string_lossy().into_owned()])
                .await
                .unwrap()
                .id;
        scan_library(&app.db, &queue, library).await.unwrap();
        for item in items::list_by_kind(&app.db, library, "movie")
            .await
            .unwrap()
        {
            refresh_local(&app.db, item.id).await.unwrap();
        }
    }
}

struct HttpResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// A plain HTTP/1.1 request, as a TV would send it.
async fn http(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> HttpResponse {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("header end");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let mut body = raw[split + 4..].to_vec();
    if headers
        .get("transfer-encoding")
        .is_some_and(|t| t == "chunked")
    {
        let mut decoded = Vec::new();
        let mut rest = &body[..];
        loop {
            let line_end = rest.windows(2).position(|w| w == b"\r\n").unwrap();
            let size =
                usize::from_str_radix(std::str::from_utf8(&rest[..line_end]).unwrap().trim(), 16)
                    .unwrap();
            if size == 0 {
                break;
            }
            decoded.extend_from_slice(&rest[line_end + 2..line_end + 2 + size]);
            rest = &rest[line_end + 2 + size + 2..];
        }
        body = decoded;
    }
    HttpResponse {
        status,
        headers,
        body,
    }
}

fn browse_body(object_id: &str, flag: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:Browse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1"><ObjectID>{object_id}</ObjectID><BrowseFlag>{flag}</BrowseFlag><Filter>*</Filter><StartingIndex>0</StartingIndex><RequestedCount>0</RequestedCount><SortCriteria></SortCriteria></u:Browse></s:Body></s:Envelope>"#
    )
}

/// A Browse result: status, DIDL-Lite (unescaped) and TotalMatches.
async fn browse(port: u16, object_id: &str, flag: &str) -> (u16, String, i64) {
    let response = http(
        port,
        "POST",
        "/ContentDirectory/control",
        &[
            ("Content-Type", "text/xml; charset=\"utf-8\""),
            (
                "SOAPACTION",
                "\"urn:schemas-upnp-org:service:ContentDirectory:1#Browse\"",
            ),
        ],
        &browse_body(object_id, flag),
    )
    .await;
    let text = response.text();
    if response.status != 200 {
        return (response.status, text, 0);
    }
    let tag = |name: &str| {
        let start = text.find(&format!("<{name}>")).unwrap() + name.len() + 2;
        let end = text.find(&format!("</{name}>")).unwrap();
        text[start..end].to_owned()
    };
    let didl = tag("Result")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    (200, didl, tag("TotalMatches").parse().unwrap())
}

/// `id="…"` values of the containers/items in a DIDL-Lite document.
fn ids(didl: &str, element: &str) -> Vec<String> {
    didl.match_indices(&format!("<{element} id=\""))
        .map(|(at, m)| {
            let rest = &didl[at + m.len()..];
            rest[..rest.find('"').unwrap()].to_owned()
        })
        .collect()
}

/// `<res …>URL</res>` values (unescaped).
fn resources(didl: &str) -> Vec<String> {
    didl.match_indices("<res ")
        .map(|(at, _)| {
            let rest = &didl[at..];
            let start = rest.find('>').unwrap() + 1;
            let end = rest.find("</res>").unwrap();
            rest[start..end].replace("&amp;", "&")
        })
        .collect()
}

#[tokio::test]
async fn dlna_serves_the_chosen_users_view_over_soap() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let media = tempfile::tempdir().unwrap();
    libraries(&app, media.path()).await;
    // The DLNA user: age limit 12, unrated content (pictures, books) allowed.
    let kid = app
        .post(
            "/api/v1/users",
            json!({ "name": "Kind", "password": PASSWORD, "max_age_rating": 12, "allow_unrated": true }),
            Some(&admin),
        )
        .await;
    assert_eq!(kid.status, StatusCode::CREATED, "{}", kid.json);
    let kid_id = kid.json["id"].as_i64().unwrap();

    // Off by default.
    let settings = app.get("/api/v1/settings/network", Some(&admin)).await;
    assert_eq!(settings.status, StatusCode::OK, "{}", settings.json);
    assert_eq!(settings.json["dlna_user_id"], json!(null));
    assert_eq!(settings.json["dlna_status"]["running"], false);
    assert_eq!(settings.json["dlna_port"], 8099);
    let invalid = app
        .request(
            Method::PUT,
            "/api/v1/settings/network",
            Some(json!({ "dlna_user_id": 999 })),
            Some(&admin),
        )
        .await;
    assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);
    let low_port = app
        .request(
            Method::PUT,
            "/api/v1/settings/network",
            Some(json!({ "dlna_port": 80 })),
            Some(&admin),
        )
        .await;
    assert_eq!(low_port.status, StatusCode::UNPROCESSABLE_ENTITY);

    let enabled = app
        .request(
            Method::PUT,
            "/api/v1/settings/network",
            Some(json!({ "dlna_user_id": kid_id, "dlna_port": 0, "dlna_name": "Wohnzimmer & Co" })),
            Some(&admin),
        )
        .await;
    assert_eq!(enabled.status, StatusCode::OK, "{}", enabled.json);
    assert_eq!(
        enabled.json["dlna_status"]["running"], true,
        "{}",
        enabled.json
    );
    let port = enabled.json["dlna_status"]["port"].as_u64().unwrap() as u16;

    // Device and service descriptions.
    let description = http(port, "GET", "/description.xml", &[], "").await;
    assert_eq!(description.status, 200);
    let text = description.text();
    assert!(text.contains("<deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType>"));
    assert!(
        text.contains("<friendlyName>Wohnzimmer &amp; Co</friendlyName>"),
        "{text}"
    );
    assert!(text.contains("<UDN>uuid:"));
    assert!(text.contains("<controlURL>/ContentDirectory/control</controlURL>"));
    let scpd = http(port, "GET", "/ContentDirectory/scpd.xml", &[], "").await;
    assert!(scpd.text().contains("<name>Browse</name>"));
    let subscribed = http(
        port,
        "SUBSCRIBE",
        "/ContentDirectory/event",
        &[("NT", "upnp:event"), ("CALLBACK", "<http://127.0.0.1:1/>")],
        "",
    )
    .await;
    assert_eq!(subscribed.status, 200);
    assert!(subscribed.headers["sid"].starts_with("uuid:"));

    // Root: one container per library.
    let (status, root, total) = browse(port, "0", "BrowseDirectChildren").await;
    assert_eq!(status, 200, "{root}");
    assert_eq!(total, 3);
    let library_ids = ids(&root, "container");
    assert_eq!(library_ids.len(), 3);
    assert!(root.contains("<dc:title>Fotos</dc:title>"));
    let (_, root_meta, total) = browse(port, "0", "BrowseMetadata").await;
    assert_eq!(total, 1);
    assert!(
        root_meta.contains(r#"<container id="0" parentID="-1""#),
        "{root_meta}"
    );

    // Walk the whole tree like a TV: never the marker, every object browsable.
    let mut pending: Vec<String> = library_ids.clone();
    let mut seen_items = Vec::new();
    let mut all = root.clone();
    while let Some(id) = pending.pop() {
        let (status, didl, total) = browse(port, &id, "BrowseDirectChildren").await;
        assert_eq!(status, 200, "{id}: {didl}");
        assert_eq!(
            total as usize,
            ids(&didl, "container").len() + ids(&didl, "item").len()
        );
        assert!(!didl.contains(MARKER), "{id} leaked: {didl}");
        all.push_str(&didl);
        pending.extend(ids(&didl, "container"));
        seen_items.extend(ids(&didl, "item"));
    }
    assert!(all.contains("<dc:title>Kinderfilm</dc:title>"), "{all}");
    assert!(all.contains("<upnp:class>object.item.videoItem.movie</upnp:class>"));
    assert!(all.contains("<upnp:class>object.container.album.photoAlbum</upnp:class>"));
    assert!(all.contains("<dc:title>Urlaub</dc:title>"));
    assert_eq!(
        all.matches("<upnp:class>object.item.imageItem.photo</upnp:class>")
            .count(),
        3
    );
    assert!(all.contains("<dc:title>Das Buch</dc:title>"));
    assert!(all.contains("<dc:creator>Anna Autorin</dc:creator>"));
    assert_eq!(
        all.matches("<upnp:class>object.item.audioItem.audioBook</upnp:class>")
            .count(),
        2
    );
    assert_eq!(seen_items.len(), 1 + 3 + 2);

    // Every item can be looked at alone; restricted or unknown ones do not exist.
    for id in &seen_items {
        let (status, didl, total) = browse(port, id, "BrowseMetadata").await;
        assert_eq!((status, total), (200, 1), "{id}: {didl}");
        assert!(didl.contains(&format!("<item id=\"{id}\"")));
    }
    let geheim = items::list_by_kind(&app.db, 1, "movie")
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.title == "Geheimfilm")
        .unwrap()
        .id;
    for id in [
        format!("item:{geheim}"),
        "item:9999".to_owned(),
        "lib:99".to_owned(),
        "quatsch".to_owned(),
    ] {
        let (status, body, _) = browse(port, &id, "BrowseMetadata").await;
        assert_eq!(status, 500, "{id}");
        assert!(body.contains("<errorCode>701</errorCode>"), "{body}");
        assert!(!body.contains(MARKER));
    }
    let bad_flag = http(
        port,
        "POST",
        "/ContentDirectory/control",
        &[],
        &browse_body("0", "BrowseEverything"),
    )
    .await;
    assert!(bad_flag.text().contains("<errorCode>402</errorCode>"));
    let unknown = http(
        port,
        "POST",
        "/ContentDirectory/control",
        &[],
        "<s:Envelope xmlns:s=\"x\"><s:Body><u:Explode xmlns:u=\"y\"/></s:Body></s:Envelope>",
    )
    .await;
    assert!(unknown.text().contains("<errorCode>401</errorCode>"));

    // Streaming: absolute URLs on the DLNA port, with ranges; pictures as JPEG.
    let urls = resources(&all);
    let kinderfilm = items::list_by_kind(&app.db, 1, "movie")
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.title == "Kinderfilm")
        .unwrap()
        .id;
    let film_file = flixnet_db::media::files_for_item(&app.db, kinderfilm)
        .await
        .unwrap()[0]
        .id;
    let film = urls
        .iter()
        .find(|u| u.contains(&format!("/media/{film_file}?")))
        .unwrap()
        .clone();
    let base = format!("http://127.0.0.1:{port}");
    assert!(film.starts_with(&format!("{base}/media/")), "{film}");
    let path = film.strip_prefix(&base).unwrap();
    let full = http(port, "GET", path, &[], "").await;
    assert_eq!(full.status, 200);
    assert_eq!(full.headers["transfermode.dlna.org"], "Streaming");
    assert!(full.headers["contentfeatures.dlna.org"].contains("DLNA.ORG_OP=01"));
    let ranged = http(port, "GET", path, &[("Range", "bytes=0-4")], "").await;
    assert_eq!(ranged.status, 206);
    assert_eq!(ranged.body.len(), 5);
    let tampered = path.replace("sig=", "sig=00");
    assert_eq!(http(port, "GET", &tampered, &[], "").await.status, 404);
    let picture = urls.iter().find(|u| u.contains("/art/")).unwrap();
    let picture = http(port, "GET", picture.strip_prefix(&base).unwrap(), &[], "").await;
    assert_eq!(picture.status, 200);
    assert_eq!(picture.headers["content-type"], "image/jpeg");

    // ConnectionManager.
    let protocols = http(
        port,
        "POST",
        "/ConnectionManager/control",
        &[],
        "<s:Envelope xmlns:s=\"x\"><s:Body><u:GetProtocolInfo xmlns:u=\"urn:schemas-upnp-org:service:ConnectionManager:1\"/></s:Body></s:Envelope>",
    )
    .await;
    assert!(protocols.text().contains("http-get:*:video/x-matroska:"));

    // Lowering the DLNA user's limit applies to issued URLs at once.
    let kinderfilm_path = path.to_owned();
    app.request(
        Method::PATCH,
        &format!("/api/v1/users/{kid_id}"),
        Some(json!({ "max_age_rating": 0, "allow_unrated": false })),
        Some(&admin),
    )
    .await;
    assert_eq!(
        http(port, "GET", &kinderfilm_path, &[], "").await.status,
        200,
        "FSK 0 stays"
    );
    for library in &library_ids {
        let (_, after, _) = browse(port, library, "BrowseDirectChildren").await;
        assert!(
            !after.contains("photoAlbum") && !after.contains("Das Buch"),
            "unrated pictures and books are hidden now: {after}"
        );
    }

    // Switching DLNA off stops the server.
    let off = app
        .request(
            Method::PUT,
            "/api/v1/settings/network",
            Some(json!({ "dlna_user_id": null })),
            Some(&admin),
        )
        .await;
    assert_eq!(off.json["dlna_status"]["running"], false);
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn network_settings_and_cast_config() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let user = app
        .post(
            "/api/v1/users",
            json!({ "name": "Mia", "password": PASSWORD }),
            Some(&admin),
        )
        .await;
    assert_eq!(user.status, StatusCode::CREATED);
    let mia = app
        .post(
            "/api/v1/auth/login",
            common::credentials("Mia", PASSWORD, "mia"),
            None,
        )
        .await
        .json["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        app.get("/api/v1/settings/network", Some(&mia)).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.get("/api/v1/cast/config", Some(&mia)).await.json["base_url"],
        json!(null)
    );

    let bad = app
        .request(
            Method::PUT,
            "/api/v1/settings/network",
            Some(json!({ "public_url": "192.168.1.2" })),
            Some(&admin),
        )
        .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    let saved = app
        .request(
            Method::PUT,
            "/api/v1/settings/network",
            Some(json!({ "public_url": "http://192.168.188.129:8097/" })),
            Some(&admin),
        )
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.json);
    assert_eq!(saved.json["public_url"], "http://192.168.188.129:8097");
    assert_eq!(
        saved.json["dlna_status"]["running"], false,
        "DLNA stays off"
    );
    assert_eq!(
        app.get("/api/v1/cast/config", Some(&mia)).await.json["base_url"],
        "http://192.168.188.129:8097"
    );
    let cleared = app
        .request(
            Method::PUT,
            "/api/v1/settings/network",
            Some(json!({ "public_url": null })),
            Some(&admin),
        )
        .await;
    assert_eq!(cleared.json["public_url"], json!(null));
}
