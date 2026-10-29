//! Shared HTTP client for metadata providers and image downloads.

use std::sync::OnceLock;
use std::time::Duration;

/// rustls with the `ring` provider (no cmake/NASM toolchain needed to build).
fn install_crypto_provider() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        // Err only means another provider was installed first, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

pub fn client() -> reqwest::Client {
    install_crypto_provider();
    reqwest::Client::builder()
        .user_agent(concat!(
            "Flixnet/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/gottschalkfelix4-source/flixnet-server)"
        ))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("HTTP client configuration is valid")
}
