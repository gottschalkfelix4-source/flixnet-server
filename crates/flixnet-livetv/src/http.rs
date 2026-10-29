//! HTTP client for playlists, provider APIs, HDHomeRun devices and guide downloads.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("{0}")]
    Http(#[from] reqwest::Error),
    #[error("HTTP status {0}")]
    Status(u16),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// rustls with the `ring` provider (no cmake/NASM toolchain needed to build).
fn install_crypto_provider() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        // Err only means another provider was installed first, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Client without an overall timeout (guide files can take minutes); stalled
/// connections still fail through the read timeout.
pub fn client() -> reqwest::Client {
    install_crypto_provider();
    reqwest::Client::builder()
        .user_agent(concat!("Flixnet/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(60))
        .build()
        .expect("HTTP client configuration is valid")
}

/// `http://` or `https://` (else the location is a local file).
pub fn is_http(location: &str) -> bool {
    let lower = location.trim_start().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn request(
    client: &reqwest::Client,
    url: &str,
    user_agent: Option<&str>,
) -> reqwest::RequestBuilder {
    let mut request = client.get(url);
    if let Some(agent) = user_agent {
        request = request.header(reqwest::header::USER_AGENT, agent);
    }
    request
}

/// Text of a URL or local file (playlists).
pub async fn fetch_text(
    client: &reqwest::Client,
    location: &str,
    user_agent: Option<&str>,
) -> Result<String, FetchError> {
    if !is_http(location) {
        let bytes = tokio::fs::read(location.trim()).await?;
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }
    let response = request(client, location, user_agent)
        .timeout(Duration::from_secs(120))
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(FetchError::Status(response.status().as_u16()));
    }
    let bytes = response.bytes().await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// JSON from a URL (provider APIs, HDHomeRun).
pub async fn fetch_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T, FetchError> {
    let response = client
        .get(url)
        .timeout(Duration::from_secs(30))
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(FetchError::Status(response.status().as_u16()));
    }
    Ok(response.json().await?)
}

/// Streams a URL to `target` (via a temporary file, so a failed download never
/// replaces a good one). Never holds the whole body in memory.
pub async fn download(
    client: &reqwest::Client,
    url: &str,
    target: &Path,
) -> Result<u64, FetchError> {
    let mut response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Err(FetchError::Status(response.status().as_u16()));
    }
    if let Some(dir) = target.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let partial = target.with_extension("part");
    let mut file = tokio::fs::File::create(&partial).await?;
    let mut written = 0u64;
    while let Some(chunk) = response.chunk().await? {
        file.write_all(&chunk).await?;
        written += chunk.len() as u64;
    }
    file.flush().await?;
    drop(file);
    tokio::fs::rename(&partial, target).await?;
    Ok(written)
}
