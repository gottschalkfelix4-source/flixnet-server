//! OpenSubtitles client against a mocked API (no real requests).

use flixnet_library::subtitles::{
    OpenSubtitles, OpenSubtitlesConfig, OpenSubtitlesError, SearchQuery,
};
use serde_json::json;
use wiremock::matchers::{body_json, header, header_regex, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer, login: bool) -> OpenSubtitles {
    let mut config = OpenSubtitlesConfig::new("test-key");
    config.base_url = format!("{}/api/v1", server.uri());
    if login {
        config.username = Some("felix".into());
        config.password = Some("geheim".into());
    }
    OpenSubtitles::new(config)
}

fn search_body() -> serde_json::Value {
    json!({
        "total_count": 2, "total_pages": 1, "page": 1, "per_page": 60,
        "data": [
            { "id": "9001", "type": "subtitle", "attributes": {
                "subtitle_id": "9001", "language": "de", "download_count": 1200,
                "hearing_impaired": false, "hd": true, "fps": 23.976, "votes": 3, "ratings": 8.5,
                "from_trusted": true, "foreign_parts_only": false, "ai_translated": false,
                "machine_translated": false, "release": "Der.Pate.1972.German.1080p.BluRay.x264-GRP",
                "moviehash_match": true, "upload_date": "2020-01-01T00:00:00Z",
                "uploader": { "name": "subber" },
                "files": [{ "file_id": 555, "cd_number": 1, "file_name": "Der.Pate.de.srt" }]
            }},
            { "id": "9002", "type": "subtitle", "attributes": {
                "language": "de", "download_count": 50, "hearing_impaired": true,
                "files": []
            }}
        ]
    })
}

#[tokio::test]
async fn search_sends_key_agent_and_sorted_parameters() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/subtitles"))
        .and(header("Api-Key", "test-key"))
        .and(header_regex("User-Agent", r"^Flixnet v\d+\.\d+"))
        .and(query_param("imdb_id", "68646"))
        .and(query_param("languages", "de"))
        .and(query_param("moviehash", "00000000000a0000"))
        .and(query_param("tmdb_id", "238"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_body()))
        .expect(1)
        .mount(&server)
        .await;
    let results = client(&server, false)
        .search(&SearchQuery {
            languages: vec!["de".into()],
            moviehash: Some("00000000000A0000".into()),
            imdb_id: Some("tt0068646".into()),
            tmdb_id: Some(238),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(results.len(), 1, "entries without files are skipped");
    let r = &results[0];
    assert_eq!((r.file_id, r.subtitle_id.as_str()), (555, "9001"));
    assert_eq!(r.language.as_deref(), Some("de"));
    assert_eq!(r.download_count, 1200);
    assert!(r.moviehash_match && r.hd && r.from_trusted && !r.hearing_impaired);
    assert_eq!(r.fps, Some(23.976));
    assert_eq!(r.uploader.as_deref(), Some("subber"));
    // The order of the query string is alphabetical.
    let request = &server.received_requests().await.unwrap()[0];
    let names: Vec<String> = request
        .url
        .query_pairs()
        .map(|(k, _)| k.into_owned())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
}

#[tokio::test]
async fn login_token_is_reused_for_downloads() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/login"))
        .and(body_json(
            json!({ "username": "felix", "password": "geheim" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token": "jwt-123", "base_url": "evil.example.com", "status": 200,
            "user": { "allowed_downloads": 20 }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/download"))
        .and(header("Authorization", "Bearer jwt-123"))
        .and(body_json(json!({ "file_id": 555, "sub_format": "srt" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "link": format!("{}/files/555.srt", server.uri()),
            "file_name": "Der.Pate.de.srt", "requests": 1, "remaining": 19
        })))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/files/555.srt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("1\n00:00:01,000 --> 00:00:02,000\nHallo\n"),
        )
        .mount(&server)
        .await;

    let client = client(&server, true);
    let link = client.download_link(555).await.unwrap();
    assert_eq!(link.remaining, Some(19));
    assert!(link.link.ends_with("/files/555.srt"), "{}", link.link);
    // The returned base_url of a stub/foreign host is not followed.
    client.download_link(555).await.unwrap();
    let bytes = client.fetch(&link.link).await.unwrap();
    assert!(String::from_utf8(bytes).unwrap().contains("Hallo"));
}

#[tokio::test]
async fn errors_are_classified() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/subtitles"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "message": "invalid key" })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/download"))
        .respond_with(ResponseTemplate::new(406).set_body_json(json!({
            "requests": 6, "remaining": 0, "message": "You have downloaded your allowed 5 subtitles for 24h"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/login"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "message": "wrong" })))
        .mount(&server)
        .await;

    let anonymous = client(&server, false);
    assert!(matches!(
        anonymous.search(&SearchQuery::default()).await,
        Err(OpenSubtitlesError::InvalidKey)
    ));
    match anonymous.download_link(1).await {
        Err(OpenSubtitlesError::QuotaExhausted(message)) => assert!(message.contains("allowed 5")),
        other => panic!("expected quota error, got {other:?}"),
    }
    assert!(matches!(
        client(&server, true).verify().await,
        Err(OpenSubtitlesError::InvalidLogin)
    ));
}

#[tokio::test]
async fn rate_limited_requests_are_retried_once() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/subtitles"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/subtitles"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_body()))
        .mount(&server)
        .await;
    let results = client(&server, false)
        .search(&SearchQuery::default())
        .await
        .unwrap();
    assert_eq!(results.len(), 1);

    let limited = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .expect(2)
        .mount(&limited)
        .await;
    assert!(matches!(
        client(&limited, false)
            .search(&SearchQuery::default())
            .await,
        Err(OpenSubtitlesError::RateLimited)
    ));
}
