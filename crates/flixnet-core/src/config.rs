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
/// Default port of the Jellyfin-compatible API (off unless `jellyfin.enabled`).
pub const DEFAULT_JELLYFIN_PORT: u16 = 8098;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub paths: PathsConfig,
    pub media: MediaConfig,
    pub log: LogConfig,
    pub jellyfin: JellyfinConfig,
}

/// The Jellyfin-compatible API for existing Jellyfin clients (Infuse, Swiftfin, …),
/// served on its own listener so a parallel Jellyfin keeps its port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JellyfinConfig {
    /// Off by default.
    pub enabled: bool,
    /// Address of the second listener.
    pub bind: SocketAddr,
    /// Server name shown in the clients (`Flixnet` when unset).
    pub server_name: Option<String>,
}

impl Default for JellyfinConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, DEFAULT_JELLYFIN_PORT)),
            server_name: None,
        }
    }
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
    /// Hardware transcoding: `auto` (best that works), `none`, `qsv`, `vaapi`, `nvenc`.
    /// Whatever is chosen is tested at startup; software is the fallback.
    pub hardware_acceleration: HardwareAcceleration,
    /// Render node for VA-API and Quick Sync (Linux).
    pub vaapi_device: PathBuf,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            ffmpeg: PathBuf::from("ffmpeg"),
            ffprobe: PathBuf::from("ffprobe"),
            watch_folders: true,
            hardware_acceleration: HardwareAcceleration::Auto,
            vaapi_device: PathBuf::from("/dev/dri/renderD128"),
        }
    }
}

/// `media.hardware_acceleration`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HardwareAcceleration {
    #[default]
    Auto,
    None,
    Qsv,
    Vaapi,
    Nvenc,
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
    /// Temporary transcode segments (ideally in RAM). Defaults to `<config_dir>/transcode`.
    pub transcode_dir: Option<PathBuf>,
}

impl Default for PathsConfig {
    fn default() -> Self {
        Self {
            config_dir: PathBuf::from("dev-config"),
            transcode_dir: None,
        }
    }
}

impl PathsConfig {
    pub fn transcode_dir(&self) -> PathBuf {
        self.transcode_dir
            .clone()
            .unwrap_or_else(|| self.config_dir.join("transcode"))
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
            assert!(!config.jellyfin.enabled, "Jellyfin API is opt-in");
            assert_eq!(config.jellyfin.bind.port(), DEFAULT_JELLYFIN_PORT);
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
            jail.set_env("FLIXNET_JELLYFIN__ENABLED", "true");

            let config = Config::load(Path::new("flixnet.toml")).expect("load");
            assert_eq!(config.server.bind, "127.0.0.1:9100".parse().unwrap());
            assert_eq!(config.paths.config_dir, PathBuf::from("/config"));
            assert_eq!(config.log.level, "debug");
            assert!(config.jellyfin.enabled);
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

    #[test]
    fn hardware_acceleration_from_file_and_env() {
        Jail::expect_with(|jail| {
            assert_eq!(
                Config::default().media.hardware_acceleration,
                HardwareAcceleration::Auto
            );
            jail.create_file(
                "flixnet.toml",
                "[media]\nhardware_acceleration = \"vaapi\"\nvaapi_device = \"/dev/dri/renderD129\"\n",
            )?;
            let config = Config::load(Path::new("flixnet.toml")).expect("load");
            assert_eq!(
                config.media.hardware_acceleration,
                HardwareAcceleration::Vaapi
            );
            assert_eq!(
                config.media.vaapi_device,
                PathBuf::from("/dev/dri/renderD129")
            );

            jail.set_env("FLIXNET_MEDIA__HARDWARE_ACCELERATION", "nvenc");
            let config = Config::load(Path::new("flixnet.toml")).expect("load");
            assert_eq!(
                config.media.hardware_acceleration,
                HardwareAcceleration::Nvenc
            );

            jail.set_env("FLIXNET_MEDIA__HARDWARE_ACCELERATION", "cuda");
            assert!(
                Config::load(Path::new("flixnet.toml")).is_err(),
                "unknown method"
            );
            Ok(())
        });
    }
}
