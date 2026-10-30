//! Admin settings of the LLM endpoint (JSON setting `llm`).

use std::time::Duration;

use flixnet_db::{Db, DbError, settings};
use serde::{Deserialize, Serialize};

use crate::client::{LlmClient, LlmConfig};

pub const SETTINGS_KEY: &str = "llm";
/// Local models on a CPU are slow; give them time.
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmSettings {
    pub enabled: bool,
    /// API root, e.g. `http://localhost:11434/v1` (Ollama), `http://localhost:1234/v1`
    /// (LM Studio) or `https://api.openai.com/v1`.
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub timeout_secs: Option<u64>,
}

impl LlmSettings {
    /// Whether everything needed for requests is there.
    pub fn is_configured(&self) -> bool {
        self.enabled
            && self.base_url.as_deref().is_some_and(is_valid_base_url)
            && self.model.as_deref().is_some_and(|m| !m.trim().is_empty())
    }

    /// A client when the LLM is switched on and configured.
    pub fn client(&self) -> Option<LlmClient> {
        if !self.is_configured() {
            return None;
        }
        Some(LlmClient::new(LlmConfig {
            base_url: self.base_url.clone()?.trim().to_owned(),
            model: self.model.clone()?.trim().to_owned(),
            api_key: self.api_key.clone().filter(|k| !k.trim().is_empty()),
            timeout: Duration::from_secs(
                self.timeout_secs
                    .unwrap_or(DEFAULT_TIMEOUT_SECS)
                    .clamp(5, 900),
            ),
        }))
    }
}

/// `http(s)://host…` without whitespace.
pub fn is_valid_base_url(url: &str) -> bool {
    let url = url.trim();
    (url.starts_with("http://") || url.starts_with("https://"))
        && url.len() > "https://".len()
        && !url.chars().any(char::is_whitespace)
}

pub async fn load(db: &Db) -> Result<LlmSettings, DbError> {
    Ok(settings::get(db, SETTINGS_KEY).await?.unwrap_or_default())
}

pub async fn save(db: &Db, value: &LlmSettings) -> Result<(), DbError> {
    settings::set(db, SETTINGS_KEY, value).await
}

/// The configured client, if any.
pub async fn client(db: &Db) -> Result<Option<LlmClient>, DbError> {
    Ok(load(db).await?.client())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_complete_settings_give_a_client() {
        let mut s = LlmSettings {
            enabled: true,
            base_url: Some("http://localhost:11434/v1".into()),
            model: Some("llama3.2".into()),
            ..Default::default()
        };
        assert!(s.client().is_some());
        s.enabled = false;
        assert!(s.client().is_none());
        s.enabled = true;
        s.base_url = Some("ftp://x".into());
        assert!(s.client().is_none());
        s.base_url = Some("https://api.openai.com/v1".into());
        s.model = Some("  ".into());
        assert!(s.client().is_none());
        assert!(!is_valid_base_url("http://a b"));
        assert!(!is_valid_base_url("https://"));
    }
}
