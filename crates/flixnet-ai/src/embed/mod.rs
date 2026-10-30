//! Text → vector. Three implementations behind one type: the local ONNX model, an
//! OpenAI-compatible endpoint, and a deterministic stub for tests.

use std::sync::Arc;

#[cfg(feature = "onnx")]
pub mod local;
pub mod openai;
pub mod stub;

/// What a text is: e5 models embed documents and queries with different prefixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Passage,
    Query,
}

/// Key of the local model in `embedding_models`.
pub const LOCAL_MODEL_KEY: &str = "multilingual-e5-base-int8";

#[derive(Clone)]
pub enum Embedder {
    Stub,
    #[cfg(feature = "onnx")]
    Local(Arc<local::LocalEmbedder>),
    OpenAi(Arc<openai::OpenAiEmbedder>),
}

impl std::fmt::Debug for Embedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.model_key())
    }
}

/// Texts per model call.
pub const BATCH: usize = 16;

impl Embedder {
    /// Identity of the vector space (vectors of different keys never mix).
    pub fn model_key(&self) -> String {
        match self {
            Embedder::Stub => stub::MODEL_KEY.to_owned(),
            #[cfg(feature = "onnx")]
            Embedder::Local(_) => LOCAL_MODEL_KEY.to_owned(),
            Embedder::OpenAi(o) => format!("openai:{}", o.model()),
        }
    }

    /// How far below the best vector hit a search result may score and still count
    /// as relevant (cosine; models compress similarities differently).
    pub fn search_margin(&self) -> f32 {
        match self {
            Embedder::Stub => 0.25,
            #[cfg(feature = "onnx")]
            Embedder::Local(_) => 0.06,
            Embedder::OpenAi(_) => 0.1,
        }
    }

    fn e5_prefixes(&self) -> bool {
        match self {
            Embedder::Stub => false,
            #[cfg(feature = "onnx")]
            Embedder::Local(_) => true,
            Embedder::OpenAi(o) => o.wants_e5_prefixes(),
        }
    }

    /// Unit vectors, one per text, in order.
    pub async fn embed(&self, texts: &[String], purpose: Purpose) -> anyhow::Result<Vec<Vec<f32>>> {
        let texts: Vec<String> = if self.e5_prefixes() {
            let prefix = match purpose {
                Purpose::Passage => "passage: ",
                Purpose::Query => "query: ",
            };
            texts.iter().map(|t| format!("{prefix}{t}")).collect()
        } else {
            texts.to_vec()
        };
        match self {
            Embedder::Stub => Ok(texts.iter().map(|t| stub::embed(t)).collect()),
            #[cfg(feature = "onnx")]
            Embedder::Local(model) => {
                let model = model.clone();
                let mut out = Vec::with_capacity(texts.len());
                for chunk in texts.chunks(BATCH) {
                    let chunk = chunk.to_vec();
                    let model = model.clone();
                    out.extend(tokio::task::spawn_blocking(move || model.embed(&chunk)).await??);
                }
                Ok(out)
            }
            Embedder::OpenAi(client) => {
                let mut out = Vec::with_capacity(texts.len());
                for chunk in texts.chunks(32) {
                    out.extend(client.embed(chunk).await?);
                }
                Ok(out)
            }
        }
    }

    /// One query vector.
    pub async fn embed_query(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.embed(&[text.to_owned()], Purpose::Query)
            .await?
            .pop()
            .ok_or_else(|| anyhow::anyhow!("no vector returned"))
    }
}
