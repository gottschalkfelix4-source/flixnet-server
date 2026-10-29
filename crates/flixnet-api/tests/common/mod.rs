//! Shared helpers for API integration tests.
#![allow(dead_code)] // each test binary uses a different subset

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use flixnet_api::{AppState, Services, router};
use flixnet_core::Config;
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_core::scheduler::TaskRegistry;
use flixnet_db::Db;
use flixnet_library::images::ImageCache;
use flixnet_media::transcode::{TranscodeManager, TranscodeSettings};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

pub const PASSWORD: &str = "sicheres-passwort";

pub struct TestApp {
    /// Config dir of the test server (database, caches).
    pub dir: TempDir,
    pub router: Router,
    pub db: Db,
}

/// Router backed by a fresh, empty database.
pub async fn app() -> TestApp {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    // Jobs are queued but no workers run in API tests.
    let events = EventBus::new();
    let runtime = JobRuntime::new(db.clone(), events.clone());
    let services = Services {
        events,
        jobs: runtime.handle(),
        tasks: TaskRegistry::new(flixnet_core::maintenance::tasks()),
        images: ImageCache::new(dir.path().join("cache/images")),
        url_secret: b"test-secret".to_vec(),
        watcher: None,
        transcoder: TranscodeManager::new(
            "ffmpeg",
            dir.path().join("transcode"),
            TranscodeSettings::default(),
        ),
        ffprobe: flixnet_media::Ffprobe::new("ffprobe"),
        subtitles: flixnet_media::subtitles::SubtitleCache::new(
            "ffmpeg",
            dir.path().join("cache/subtitles"),
            std::time::Duration::from_secs(60),
            2,
        ),
    };
    // Caches the API reads directly (trickplay, chapter images) live in the temp dir.
    let mut config = Config::default();
    config.paths.config_dir = dir.path().to_owned();
    let router = router(AppState::new(config, db.clone(), services));
    TestApp { dir, router, db }
}

pub struct Response {
    pub status: StatusCode,
    pub json: Value,
}

impl TestApp {
    pub async fn request(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        token: Option<&str>,
    ) -> Response {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let request = match body {
            Some(body) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string())),
            None => builder.body(Body::empty()),
        }
        .unwrap();

        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Response { status, json }
    }

    pub async fn get(&self, uri: &str, token: Option<&str>) -> Response {
        self.request(Method::GET, uri, None, token).await
    }

    pub async fn post(&self, uri: &str, body: Value, token: Option<&str>) -> Response {
        self.request(Method::POST, uri, Some(body), token).await
    }

    /// Completes setup with an admin "Felix" and returns its token.
    pub async fn setup_admin(&self) -> String {
        let response = self
            .post(
                "/api/v1/setup",
                credentials("Felix", PASSWORD, "device-admin"),
                None,
            )
            .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.json);
        response.json["token"].as_str().unwrap().to_owned()
    }
}

pub fn credentials(name: &str, password: &str, device_id: &str) -> Value {
    json!({
        "name": name,
        "password": password,
        "device": { "id": device_id, "name": "Testbrowser", "client": "Flixnet Tests" }
    })
}
