//! Admin settings of the AI features (stored in the `settings` table).

use flixnet_db::{Db, Result, settings};
use serde::{Deserialize, Serialize};

pub const SETTINGS_KEY: &str = "ai";

/// Where item and query vectors come from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbedderKind {
    /// multilingual-e5-base via ONNX Runtime, on this server.
    #[default]
    Local,
    /// An OpenAI-compatible `/embeddings` endpoint (OpenAI, Ollama, …).
    Openai,
    /// No embeddings: search is full-text only, no recommendations.
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSettings {
    pub embedder: EmbedderKind,
    /// Download the local model automatically on first use.
    pub auto_download: bool,
    /// e.g. `https://api.openai.com/v1` or `http://192.168.1.10:11434/v1`.
    pub openai_base_url: Option<String>,
    pub openai_api_key: Option<String>,
    /// e.g. `text-embedding-3-small`, `nomic-embed-text`.
    pub openai_model: Option<String>,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            embedder: EmbedderKind::Local,
            auto_download: true,
            openai_base_url: None,
            openai_api_key: None,
            openai_model: None,
        }
    }
}

pub async fn load(db: &Db) -> Result<AiSettings> {
    Ok(settings::get(db, SETTINGS_KEY).await?.unwrap_or_default())
}

pub async fn save(db: &Db, value: &AiSettings) -> Result<()> {
    settings::set(db, SETTINGS_KEY, value).await
}
