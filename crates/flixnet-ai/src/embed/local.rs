//! Local embeddings with ONNX Runtime: multilingual-e5-base (int8) for German and
//! English. ONNX Runtime itself is loaded at runtime (`ort` with `load-dynamic`), from
//! the library the model manager downloads or from `ai.onnxruntime_path`.

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, anyhow};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

use crate::math::normalize;

/// Longest input in tokens (the model's limit).
const MAX_TOKENS: usize = 512;

static ENVIRONMENT: OnceLock<Result<ort::environment::Environment, String>> = OnceLock::new();

/// Loads ONNX Runtime once per process (later calls reuse the first result).
fn environment(runtime: &Path) -> anyhow::Result<&'static ort::environment::Environment> {
    let env = ENVIRONMENT.get_or_init(|| {
        ort::init_from(runtime)
            .map_err(|e| e.to_string())?
            .with_name("flixnet")
            .build()
            .map_err(|e| e.to_string())
    });
    env.as_ref()
        .map_err(|e| anyhow!("ONNX Runtime could not be loaded: {e}"))
}

pub struct LocalEmbedder {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    /// Some exports also want `token_type_ids` (all zero for XLM-R).
    token_type_ids: bool,
}

impl LocalEmbedder {
    /// Loads model and tokenizer (seconds; call from a blocking thread).
    pub fn load(
        runtime: &Path,
        model: &Path,
        tokenizer: &Path,
        threads: usize,
    ) -> anyhow::Result<Self> {
        let env = environment(runtime)?;
        let mut tokenizer = Tokenizer::from_file(tokenizer)
            .map_err(|e| anyhow!("tokenizer {}: {e}", tokenizer.display()))?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_TOKENS,
                ..Default::default()
            }))
            .map_err(|e| anyhow!("tokenizer truncation: {e}"))?;
        tokenizer.with_padding(Some(PaddingParams::default()));

        let session = Session::builder(env)
            .and_then(|b| {
                b.with_optimization_level(GraphOptimizationLevel::Level3)
                    .map_err(Into::into)
            })
            .and_then(|b| b.with_intra_threads(threads.max(1)).map_err(Into::into))
            .and_then(|b| b.with_inter_threads(1).map_err(Into::into))
            .and_then(|mut b| b.commit_from_file(model))
            .with_context(|| format!("loading {}", model.display()))?;
        let token_type_ids = session
            .inputs()
            .iter()
            .any(|i| i.name() == "token_type_ids");
        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            token_type_ids,
        })
    }

    /// Mean-pooled, L2-normalized sentence vectors (blocking).
    pub fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| anyhow!("tokenizing: {e}"))?;
        let batch = encodings.len();
        let seq = encodings
            .iter()
            .map(|e| e.get_ids().len())
            .max()
            .unwrap_or(0);
        let mut ids = Vec::with_capacity(batch * seq);
        let mut mask = Vec::with_capacity(batch * seq);
        for e in &encodings {
            ids.extend(e.get_ids().iter().map(|x| *x as i64));
            mask.extend(e.get_attention_mask().iter().map(|x| *x as i64));
        }
        let shape = [batch as i64, seq as i64];
        let mut inputs = vec![
            ("input_ids", Tensor::from_array((shape, ids))?.into_dyn()),
            (
                "attention_mask",
                Tensor::from_array((shape, mask.clone()))?.into_dyn(),
            ),
        ];
        if self.token_type_ids {
            inputs.push((
                "token_type_ids",
                Tensor::from_array((shape, vec![0_i64; batch * seq]))?.into_dyn(),
            ));
        }

        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow!("model lock poisoned"))?;
        let first_output = session
            .outputs()
            .first()
            .map(|o| o.name().to_owned())
            .ok_or_else(|| anyhow!("model has no output"))?;
        let outputs = session.run(inputs)?;
        let hidden = outputs
            .get("last_hidden_state")
            .or_else(|| outputs.get(&first_output))
            .ok_or_else(|| anyhow!("model has no output"))?;
        let (out_shape, data) = hidden.try_extract_tensor::<f32>()?;
        let dims = out_shape.iter().copied().collect::<Vec<i64>>();
        let [b, s, h] = dims[..] else {
            return Err(anyhow!("unexpected output shape {dims:?}"));
        };
        let (b, s, h) = (b as usize, s as usize, h as usize);
        if b != batch || s != seq {
            return Err(anyhow!("output shape {dims:?} does not match the input"));
        }

        let mut vectors = Vec::with_capacity(batch);
        for row in 0..batch {
            let mut pooled = vec![0.0_f32; h];
            let mut count = 0.0_f32;
            for t in 0..seq {
                if mask[row * seq + t] == 0 {
                    continue;
                }
                count += 1.0;
                let offset = (row * seq + t) * h;
                for (p, x) in pooled.iter_mut().zip(&data[offset..offset + h]) {
                    *p += x;
                }
            }
            if count > 0.0 {
                pooled.iter_mut().for_each(|p| *p /= count);
            }
            normalize(&mut pooled);
            vectors.push(pooled);
        }
        Ok(vectors)
    }
}
