//! Embeddings from an OpenAI-compatible endpoint (`POST {base}/embeddings`): OpenAI
//! itself, Ollama (`http://host:11434/v1`), LM Studio, vLLM, LocalAI …

use anyhow::{Context, anyhow, bail};
use serde::Deserialize;
use serde_json::json;

use crate::math::normalize;

#[derive(Debug, Clone)]
pub struct OpenAiEmbedder {
    http: reqwest::Client,
    /// Without trailing slash, e.g. `https://api.openai.com/v1`.
    base_url: String,
    api_key: Option<String>,
    model: String,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
    #[serde(default)]
    index: usize,
}

impl OpenAiEmbedder {
    pub fn new(
        http: reqwest::Client,
        base_url: &str,
        api_key: Option<String>,
        model: &str,
    ) -> Self {
        Self {
            http,
            base_url: base_url.trim().trim_end_matches('/').to_owned(),
            api_key: api_key.filter(|k| !k.trim().is_empty()),
            model: model.trim().to_owned(),
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// e5 models served by Ollama & co. still want their `query: `/`passage: ` prefixes.
    pub fn wants_e5_prefixes(&self) -> bool {
        self.model.to_ascii_lowercase().contains("e5")
    }

    pub async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut request = self
            .http
            .post(format!("{}/embeddings", self.base_url))
            .json(&json!({ "model": self.model, "input": texts }));
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("embedding endpoint {} not reachable", self.base_url))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let snippet: String = body.chars().take(200).collect();
            bail!("embedding endpoint answered {status}: {snippet}");
        }
        let mut body: EmbeddingResponse = response
            .json()
            .await
            .context("embedding endpoint sent an unexpected answer")?;
        if body.data.len() != texts.len() {
            return Err(anyhow!(
                "embedding endpoint returned {} vectors for {} texts",
                body.data.len(),
                texts.len()
            ));
        }
        body.data.sort_by_key(|d| d.index);
        let dims = body.data[0].embedding.len();
        if dims == 0 || body.data.iter().any(|d| d.embedding.len() != dims) {
            bail!("embedding endpoint returned vectors of different sizes");
        }
        Ok(body
            .data
            .into_iter()
            .map(|d| {
                let mut v = d.embedding;
                normalize(&mut v);
                v
            })
            .collect())
    }
}
