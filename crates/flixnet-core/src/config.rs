//! Server configuration.
//!
//! Layered in this order (later wins): built-in defaults → TOML file → `FLIXNET_*`
//! environment variables. Nested keys use a double underscore, e.g.
//! `FLIXNET_SERVER__BIND=0.0.0.0:8097` or `FLIXNET_PATHS__CONFIG_DIR=/config`.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::{Deserialize, Serialize};

/// Default HTTP port. Chosen next to Jellyfin's 8096 without clashing with it.
pub const DEFAULT_PORT: u16 = 8097;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub paths: PathsConfig,
    pub media: MediaConfig,
    pub log: LogConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaConfig {
    /// ffmpeg binary (name on PATH or full path). Docker ships jellyfin-ffmpeg.
    pub ffmpeg: PathBuf,
    /// ffprobe binary (name on PATH or full path).
    pub ffprobe: PathBuf,
    /// Watch library folders for changes (periodic rescans run either way).
    pub watch_folders: bool,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            ffmpeg: PathBuf::from("ffmpeg"),
            ffprobe: PathBuf::from("ffprobe"),
            watch_folders: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Address the HTTP server listens on.
    pub bind: SocketAddr,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, DEFAULT_PORT)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PathsConfig {
    /// Persistent data: database, image cache, models. `/config` in Docker.
    pub config_dir: PathBuf,
}

impl Default for PathsConfig {
    fn default() -> Self {
        Self {
            config_dir: PathBuf::from("dev-config"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    /// `tracing` filter directive; `RUST_LOG` overrides it.
    pub level: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".to_owned(),
        }
    }
}

impl Config {
    /// Layered configuration sources. A missing TOML file is not an error.
    pub fn figment(file: &Path) -> Figment {
        Figment::from(Serialized::defaults(Config::default()))
            .merge(Toml::file(file))
            .merge(Env::prefixed("FLIXNET_").split("__"))
    }

    pub fn load(file: &Path) -> Result<Self, Box<figment::Error>> {
        Self::figment(file).extract().map_err(Box::new)
    }
}

#[cfg(test)]
#[allow(clippy::result_large_err)] // figment::Jail fixes the closure's error type
mod tests {
    use super::*;
    use figment::Jail;

    #[test]
    fn defaults_without_file_or_env() {
        Jail::expect_with(|_jail| {
            let config = Config::load(Path::new("missing.toml")).expect("load");
            assert_eq!(config, Config::default());
            assert_eq!(config.server.bind.port(), DEFAULT_PORT);
            Ok(())
        });
    }

    #[test]
    fn env_overrides_file() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "flixnet.toml",
                r#"
                [server]
                bind = "127.0.0.1:9000"

                [log]
                level = "debug"
                "#,
            )?;
            jail.set_env("FLIXNET_SERVER__BIND", "127.0.0.1:9100");
            jail.set_env("FLIXNET_PATHS__CONFIG_DIR", "/config");

            let config = Config::load(Path::new("flixnet.toml")).expect("load");
            assert_eq!(config.server.bind, "127.0.0.1:9100".parse().unwrap());
            assert_eq!(config.paths.config_dir, PathBuf::from("/config"));
            assert_eq!(config.log.level, "debug");
            Ok(())
        });
    }

    #[test]
    fn invalid_value_is_an_error() {
        Jail::expect_with(|jail| {
            jail.set_env("FLIXNET_SERVER__BIND", "not-an-address");
            assert!(Config::load(Path::new("missing.toml")).is_err());
            Ok(())
        });
    }
}
