//! HTTP client for the embedding endpoint.

use std::sync::OnceLock;
use std::time::Duration;

/// rustls with the `ring` provider (no cmake/NASM toolchain needed to build).
pub(crate) fn install_crypto_provider() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        // Err only means another provider was installed first, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Client for API calls (embedding endpoints can be slow on first load of a model).
pub fn client() -> reqwest::Client {
    install_crypto_provider();
    reqwest::Client::builder()
        .user_agent(concat!("Flixnet/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
        .expect("HTTP client configuration is valid")
}
