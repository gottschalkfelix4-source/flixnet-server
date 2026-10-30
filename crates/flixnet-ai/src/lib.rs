//! Local AI for Flixnet: item embeddings, hybrid search, recommendations and group
//! suggestions.
//!
//! * Embeddings come from multilingual-e5-base (int8, ONNX Runtime, on this server),
//!   an OpenAI-compatible endpoint, or — in tests only — a deterministic stub.
//! * The local model and ONNX Runtime are downloaded on first use into
//!   `<config_dir>/models` with SHA-256 verification ([`models`]).
//! * Vectors live in `item_embeddings` and are searched in memory ([`index`]).
//! * AI jobs run in the `ai` pool (one worker, low priority) and wait while anyone is
//!   transcoding, so they never compete with playback ([`Ai::wait_until_idle`]).

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use flixnet_core::Config;
use flixnet_core::config::{AiConfig, EmbedderOverride};
use flixnet_db::Db;

pub mod embed;
pub mod http;
pub mod index;
pub mod jobs;
pub mod math;
pub mod models;
pub mod recommend;
pub mod search;
pub mod settings;
pub mod text;

use embed::Embedder;
use index::BruteForceIndex;
use models::ModelManager;
use settings::{AiSettings, EmbedderKind};

/// Answers "is someone transcoding right now?" (wired to the transcode manager).
pub type BusyProbe = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

/// The in-memory vector index of the active model.
#[derive(Debug)]
pub struct LoadedIndex {
    pub model_id: i64,
    pub model_key: String,
    pub index: BruteForceIndex,
}

/// The AI service. Cheap to clone; shared by jobs and the API.
#[derive(Clone)]
pub struct Ai {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    config: AiConfig,
    models: ModelManager,
    http: reqwest::Client,
    downloads: reqwest::Client,
    /// The loaded embedder and the settings fingerprint it was built from.
    embedder: tokio::sync::Mutex<Option<(String, Embedder)>>,
    index: tokio::sync::RwLock<Option<LoadedIndex>>,
    busy: RwLock<Option<BusyProbe>>,
    last_error: Mutex<Option<String>>,
}

/// What the admin page shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiStatus {
    /// local | openai | off | stub
    pub embedder: &'static str,
    /// Model whose vectors are in use (`None` when off).
    pub model_key: Option<String>,
    /// Vectors can be computed right now.
    pub ready: bool,
    /// Files the local model still needs: (label, bytes; 0 = unknown).
    pub missing: Vec<(&'static str, u64)>,
    /// ONNX Runtime exists for this platform (or is configured).
    pub platform_supported: bool,
    pub embedded: i64,
    pub total: i64,
    pub last_error: Option<String>,
}

impl Ai {
    /// The service for a server configuration (nothing is loaded yet).
    pub fn new(db: Db, config: &Config) -> Self {
        Self::with_models_dir(
            db,
            config.ai.clone(),
            config.paths.config_dir.join("models"),
        )
    }

    pub fn with_models_dir(db: Db, config: AiConfig, models_dir: PathBuf) -> Self {
        let models = ModelManager::new(models_dir, config.onnxruntime_path.clone());
        Self {
            inner: Arc::new(Inner {
                db,
                config,
                models,
                http: http::client(),
                downloads: models::download_client(),
                embedder: tokio::sync::Mutex::new(None),
                index: tokio::sync::RwLock::new(None),
                busy: RwLock::new(None),
                last_error: Mutex::new(None),
            }),
        }
    }

    /// Lets AI jobs wait while `probe` reports playback transcodes.
    pub fn set_busy_probe(&self, probe: BusyProbe) {
        *self.inner.busy.write().expect("busy lock") = Some(probe);
    }

    pub async fn is_busy(&self) -> bool {
        let probe = self.inner.busy.read().expect("busy lock").clone();
        match probe {
            Some(probe) => probe().await,
            None => false,
        }
    }

    /// Waits while someone is transcoding (checking every few seconds). Returns
    /// `false` if `cancelled` became true meanwhile.
    pub async fn wait_until_idle(&self, cancelled: impl Fn() -> bool) -> bool {
        let mut logged = false;
        while self.is_busy().await {
            if cancelled() {
                return false;
            }
            if !logged {
                tracing::debug!("AI work paused while a transcode is running");
                logged = true;
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        !cancelled()
    }

    pub fn db(&self) -> &Db {
        &self.inner.db
    }

    pub fn models(&self) -> &ModelManager {
        &self.inner.models
    }

    pub(crate) fn download_client(&self) -> &reqwest::Client {
        &self.inner.downloads
    }

    /// The test-only stub embedder is forced by the configuration.
    pub fn is_stub(&self) -> bool {
        self.inner.config.embedder == EmbedderOverride::Stub
    }

    pub async fn settings(&self) -> anyhow::Result<AiSettings> {
        Ok(settings::load(&self.inner.db).await?)
    }

    /// Stores new settings; the next use loads the embedder they describe.
    pub async fn save_settings(&self, value: &AiSettings) -> anyhow::Result<()> {
        settings::save(&self.inner.db, value).await?;
        *self.inner.embedder.lock().await = None;
        *self.inner.index.write().await = None;
        self.set_error(None);
        Ok(())
    }

    /// Model whose vectors the current settings use (without loading anything).
    pub async fn model_key(&self) -> anyhow::Result<Option<String>> {
        if self.is_stub() {
            return Ok(Some(embed::stub::MODEL_KEY.to_owned()));
        }
        let s = self.settings().await?;
        Ok(match s.embedder {
            EmbedderKind::Off => None,
            EmbedderKind::Local => Some(embed::LOCAL_MODEL_KEY.to_owned()),
            EmbedderKind::Openai => s
                .openai_model
                .filter(|m| !m.trim().is_empty())
                .map(|m| format!("openai:{}", m.trim())),
        })
    }

    pub(crate) fn set_error(&self, error: Option<String>) {
        *self.inner.last_error.lock().expect("error lock") = error;
    }

    /// The embedder for the current settings, loaded on first use. `Ok(None)` when AI is
    /// off, unconfigured, or the local model is not downloaded yet.
    pub async fn embedder(&self) -> anyhow::Result<Option<Embedder>> {
        if self.is_stub() {
            return Ok(Some(Embedder::Stub));
        }
        let s = self.settings().await?;
        let fingerprint = match s.embedder {
            EmbedderKind::Off => return Ok(None),
            EmbedderKind::Local => "local".to_owned(),
            EmbedderKind::Openai => format!(
                "openai|{}|{}|{}",
                s.openai_base_url.as_deref().unwrap_or_default(),
                s.openai_model.as_deref().unwrap_or_default(),
                text::text_hash("", s.openai_api_key.as_deref().unwrap_or_default()),
            ),
        };
        let mut slot = self.inner.embedder.lock().await;
        if let Some((loaded, embedder)) = slot.as_ref()
            && *loaded == fingerprint
        {
            return Ok(Some(embedder.clone()));
        }
        let embedder = match s.embedder {
            EmbedderKind::Off => return Ok(None),
            EmbedderKind::Openai => {
                let (Some(base), Some(model)) = (
                    s.openai_base_url.filter(|u| !u.trim().is_empty()),
                    s.openai_model.filter(|m| !m.trim().is_empty()),
                ) else {
                    return Ok(None);
                };
                Embedder::OpenAi(Arc::new(embed::openai::OpenAiEmbedder::new(
                    self.inner.http.clone(),
                    &base,
                    s.openai_api_key,
                    &model,
                )))
            }
            EmbedderKind::Local => match self.load_local().await {
                Ok(Some(embedder)) => embedder,
                Ok(None) => return Ok(None),
                Err(err) => {
                    self.set_error(Some(format!("{err:#}")));
                    return Err(err);
                }
            },
        };
        *slot = Some((fingerprint, embedder.clone()));
        Ok(Some(embedder))
    }

    #[cfg(feature = "onnx")]
    async fn load_local(&self) -> anyhow::Result<Option<Embedder>> {
        let models = &self.inner.models;
        if !models.is_installed(&models::E5_MODEL) || !models.is_installed(&models::E5_TOKENIZER) {
            return Ok(None);
        }
        let Some(runtime) = models.runtime_path() else {
            return Ok(None);
        };
        let model = models.path(&models::E5_MODEL);
        let tokenizer = models.path(&models::E5_TOKENIZER);
        let threads = std::thread::available_parallelism()
            .map_or(2, |n| n.get() / 2)
            .clamp(1, 4);
        let started = std::time::Instant::now();
        let local = tokio::task::spawn_blocking(move || {
            embed::local::LocalEmbedder::load(&runtime, &model, &tokenizer, threads)
        })
        .await??;
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "local embedding model loaded"
        );
        Ok(Some(Embedder::Local(Arc::new(local))))
    }

    #[cfg(not(feature = "onnx"))]
    async fn load_local(&self) -> anyhow::Result<Option<Embedder>> {
        Ok(None)
    }

    /// Loads the vector index of the current model if it is not in memory yet.
    /// Returns whether an index with at least one vector is available.
    pub async fn ensure_index(&self) -> anyhow::Result<bool> {
        let Some(key) = self.model_key().await? else {
            return Ok(false);
        };
        {
            let current = self.inner.index.read().await;
            if let Some(loaded) = current.as_ref()
                && loaded.model_key == key
            {
                return Ok(!loaded.index.ids().is_empty());
            }
        }
        let Some((model_id, dims)) = flixnet_db::ai::find_model(&self.inner.db, &key).await? else {
            return Ok(false);
        };
        let mut index = BruteForceIndex::new(dims as usize);
        for (id, bytes) in flixnet_db::ai::vectors(&self.inner.db, model_id).await? {
            index.insert(id, &math::from_bytes(&bytes));
        }
        let available = !index.ids().is_empty();
        *self.inner.index.write().await = Some(LoadedIndex {
            model_id,
            model_key: key,
            index,
        });
        Ok(available)
    }

    /// Read access to the loaded index (call [`Ai::ensure_index`] first).
    pub async fn index(&self) -> tokio::sync::RwLockReadGuard<'_, Option<LoadedIndex>> {
        self.inner.index.read().await
    }

    /// Adds freshly stored vectors to the in-memory index, if it holds that model.
    pub(crate) async fn index_insert(
        &self,
        model_key: &str,
        model_id: i64,
        vectors: &[(i64, Vec<f32>)],
    ) {
        let mut slot = self.inner.index.write().await;
        match slot.as_mut() {
            Some(loaded) if loaded.model_key == model_key && loaded.model_id == model_id => {
                for (id, v) in vectors {
                    loaded.index.insert(*id, v);
                }
            }
            // Not loaded (or another model): the next search loads it from the database.
            _ => *slot = None,
        }
    }

    /// Unloads the embedder and the index (before deleting model files).
    pub async fn forget_embedder(&self) {
        *self.inner.embedder.lock().await = None;
        *self.inner.index.write().await = None;
    }

    /// Forgets the in-memory index (after deleting vectors).
    pub async fn drop_index(&self) {
        *self.inner.index.write().await = None;
    }

    /// Embeds a search query with the model of the loaded index; `None` if semantic
    /// search is unavailable (no vectors, or the embedder cannot run).
    pub async fn query_vector(&self, text: &str) -> Option<(Vec<f32>, f32)> {
        if !self.ensure_index().await.ok()? {
            return None;
        }
        let embedder = match self.embedder().await {
            Ok(Some(e)) => e,
            Ok(None) => return None,
            Err(err) => {
                tracing::warn!(%err, "embedder unavailable; search falls back to full text");
                return None;
            }
        };
        let key = embedder.model_key();
        if self
            .index()
            .await
            .as_ref()
            .is_none_or(|l| l.model_key != key)
        {
            return None;
        }
        match embedder.embed_query(text).await {
            Ok(v) => Some((v, embedder.search_margin())),
            Err(err) => {
                tracing::warn!(%err, "embedding the search query failed");
                None
            }
        }
    }

    pub async fn status(&self) -> anyhow::Result<AiStatus> {
        let settings = self.settings().await?;
        let embedder = if self.is_stub() {
            "stub"
        } else {
            match settings.embedder {
                EmbedderKind::Local => "local",
                EmbedderKind::Openai => "openai",
                EmbedderKind::Off => "off",
            }
        };
        let model_key = self.model_key().await?;
        let model_id = match &model_key {
            Some(key) => flixnet_db::ai::find_model(&self.inner.db, key)
                .await?
                .map(|(id, _)| id),
            None => None,
        };
        let (embedded, total) = flixnet_db::ai::embedding_counts(&self.inner.db, model_id).await?;
        let missing = if embedder == "local" {
            self.inner
                .models
                .missing()
                .into_iter()
                .map(|a| (a.label, a.size))
                .collect()
        } else {
            Vec::new()
        };
        let ready = match embedder {
            "stub" => true,
            "local" => {
                cfg!(feature = "onnx")
                    && missing.is_empty()
                    && self.inner.models.platform_supported()
            }
            "openai" => model_key.is_some() && settings.openai_base_url.is_some(),
            _ => false,
        };
        Ok(AiStatus {
            embedder,
            model_key,
            ready,
            missing,
            platform_supported: self.inner.models.platform_supported(),
            embedded,
            total,
            last_error: self.inner.last_error.lock().expect("error lock").clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[tokio::test]
    async fn ai_work_waits_while_someone_transcodes() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
        let ai = Ai::with_models_dir(db, AiConfig::default(), dir.path().join("models"));
        assert!(!ai.is_busy().await, "no probe: never busy");

        // Busy for the first check, then idle.
        let checks = Arc::new(AtomicUsize::new(0));
        let counter = checks.clone();
        ai.set_busy_probe(Arc::new(move || {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { n == 0 })
        }));
        let started = std::time::Instant::now();
        assert!(ai.wait_until_idle(|| false).await);
        assert!(checks.load(Ordering::SeqCst) >= 2);
        assert!(
            started.elapsed() >= Duration::from_secs(2),
            "it really waited"
        );

        // Always busy, but cancelled: gives up instead of waiting forever.
        ai.set_busy_probe(Arc::new(|| Box::pin(async { true })));
        assert!(!ai.wait_until_idle(|| true).await);
    }
}
