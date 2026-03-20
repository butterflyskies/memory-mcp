use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use hf_hub::{api::sync::ApiBuilder, Cache, Repo, RepoType};
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

use super::EmbeddingBackend;
use crate::error::MemoryError;

/// HuggingFace model ID. Only BGE-small-en-v1.5 is supported currently.
pub const MODEL_ID: &str = "BAAI/bge-small-en-v1.5";

/// Pure-Rust embedding engine using candle for BERT inference.
///
/// Uses candle-transformers' BERT implementation with tokenizers for
/// tokenisation. No C/C++ FFI dependencies — compiles on all platforms.
pub struct CandleEmbeddingEngine {
    inner: Arc<Mutex<CandleInner>>,
    dim: usize,
}

struct CandleInner {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl CandleEmbeddingEngine {
    /// Initialise the candle embedding engine.
    ///
    /// Downloads model weights from HuggingFace Hub on first use (cached
    /// in the standard HF cache directory, respects `HF_HOME`).
    pub fn new() -> Result<Self, MemoryError> {
        let device = Device::Cpu;

        let (config, mut tokenizer, weights_path) =
            load_model_files().map_err(|e| MemoryError::Embedding(e.to_string()))?;

        // Enable padding so encode_batch produces equal-length sequences.
        tokenizer.with_padding(Some(PaddingParams {
            strategy: tokenizers::PaddingStrategy::BatchLongest,
            ..Default::default()
        }));
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: 512,
                ..Default::default()
            }))
            .map_err(|e| MemoryError::Embedding(format!("failed to set truncation: {e}")))?;

        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path], candle_core::DType::F32, &device)
                .map_err(|e| MemoryError::Embedding(format!("failed to load weights: {e}")))?
        };

        let model = BertModel::load(vb, &config)
            .map_err(|e| MemoryError::Embedding(format!("failed to build BERT model: {e}")))?;

        let dim = config.hidden_size;

        Ok(Self {
            inner: Arc::new(Mutex::new(CandleInner {
                model,
                tokenizer,
                device,
            })),
            dim,
        })
    }
}

#[async_trait::async_trait]
impl EmbeddingBackend for CandleEmbeddingEngine {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, MemoryError> {
        let arc = Arc::clone(&self.inner);
        let texts = texts.to_vec();
        tokio::task::spawn_blocking(move || {
            let guard = arc.lock().unwrap_or_else(|poisoned| {
                tracing::warn!("embedding mutex was poisoned — clearing poison and continuing");
                poisoned.into_inner()
            });
            catch_unwind(AssertUnwindSafe(|| embed_batch(&guard, &texts))).unwrap_or_else(
                |panic_payload| {
                    let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                        (*s).to_string()
                    } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "unknown panic in embedding engine".to_string()
                    };
                    Err(MemoryError::Embedding(format!(
                        "embedding engine panicked: {msg}"
                    )))
                },
            )
        })
        .await
        .map_err(|e| MemoryError::Join(e.to_string()))?
    }

    fn dimensions(&self) -> usize {
        self.dim
    }
}

// ---------------------------------------------------------------------------
// Model loading
// ---------------------------------------------------------------------------

/// Download (or retrieve from cache) the model files from HuggingFace Hub.
///
/// On first run (cold start), this downloads ~130 MB of model files from
/// HuggingFace Hub. Subsequent starts use the local cache (`HF_HOME`).
/// Use the `warmup` subcommand or a k8s init container to pre-populate the
/// cache and avoid blocking the first server startup.
fn load_model_files() -> anyhow::Result<(BertConfig, Tokenizer, PathBuf)> {
    let cache = Cache::from_env();
    let hf_repo = Repo::new(MODEL_ID.to_string(), RepoType::Model);

    // Check whether the heaviest file (model weights) is already cached.
    let cached = cache.repo(hf_repo.clone()).get("model.safetensors");
    if cached.is_none() {
        tracing::warn!(
            model = MODEL_ID,
            "embedding model not found in cache — downloading from HuggingFace Hub \
             (this may take a minute on first run; use `memory-mcp warmup` to pre-populate)"
        );
    } else {
        tracing::info!(model = MODEL_ID, "loading embedding model from cache");
    }

    // Respect HF_HOME and HF_ENDPOINT env vars; disable indicatif progress
    // bars since we are a headless server.
    let api = ApiBuilder::from_env().with_progress(false).build()?;
    let repo = api.repo(hf_repo);

    let start = std::time::Instant::now();
    let config_path = repo.get("config.json")?;
    let tokenizer_path = repo.get("tokenizer.json")?;
    let weights_path = repo.get("model.safetensors")?;
    tracing::info!(
        elapsed_ms = start.elapsed().as_millis(),
        "model files ready"
    );

    let config: BertConfig = serde_json::from_str(&std::fs::read_to_string(&config_path)?)?;
    let tokenizer = Tokenizer::from_file(&tokenizer_path)
        .map_err(|e| anyhow::anyhow!("failed to load tokenizer: {e}"))?;

    Ok((config, tokenizer, weights_path))
}

// ---------------------------------------------------------------------------
// Inference
// ---------------------------------------------------------------------------

/// Embed a batch of texts through the BERT model in a single forward pass.
///
/// Texts are tokenised with padding (to the longest sequence in the batch)
/// and truncation (to 512 tokens), then passed through BERT together.
/// An attention mask ensures padding tokens do not affect the output.
/// CLS pooling extracts the first token's hidden state, which is then
/// L2-normalised to produce unit vectors.
fn embed_batch(inner: &CandleInner, texts: &[String]) -> Result<Vec<Vec<f32>>, MemoryError> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }

    let encodings = inner
        .tokenizer
        .encode_batch(texts.to_vec(), true)
        .map_err(|e| MemoryError::Embedding(format!("tokenization failed: {e}")))?;

    let batch_size = encodings.len();
    let seq_len = encodings[0].get_ids().len();

    let all_ids: Vec<u32> = encodings
        .iter()
        .flat_map(|e| e.get_ids().to_vec())
        .collect();
    let all_type_ids: Vec<u32> = encodings
        .iter()
        .flat_map(|e| e.get_type_ids().to_vec())
        .collect();
    let all_masks: Vec<u32> = encodings
        .iter()
        .flat_map(|e| e.get_attention_mask().to_vec())
        .collect();

    if all_ids.len() != batch_size * seq_len {
        return Err(MemoryError::Embedding(format!(
            "padding invariant violated: expected {} token ids, got {} \
             (batch_size={batch_size}, seq_len={seq_len})",
            batch_size * seq_len,
            all_ids.len(),
        )));
    }

    let input_ids = Tensor::new(all_ids.as_slice(), &inner.device)
        .and_then(|t| t.reshape((batch_size, seq_len)))
        .map_err(|e| MemoryError::Embedding(format!("tensor creation failed: {e}")))?;

    let token_type_ids = Tensor::new(all_type_ids.as_slice(), &inner.device)
        .and_then(|t| t.reshape((batch_size, seq_len)))
        .map_err(|e| MemoryError::Embedding(format!("tensor creation failed: {e}")))?;

    let attention_mask = Tensor::new(all_masks.as_slice(), &inner.device)
        .and_then(|t| t.reshape((batch_size, seq_len)))
        .map_err(|e| MemoryError::Embedding(format!("tensor creation failed: {e}")))?;

    let embeddings = inner
        .model
        .forward(&input_ids, &token_type_ids, Some(&attention_mask))
        .map_err(|e| MemoryError::Embedding(format!("BERT forward pass failed: {e}")))?;

    // CLS pooling + L2 normalise each vector in the batch.
    let mut results = Vec::with_capacity(batch_size);
    for i in 0..batch_size {
        let cls = embeddings
            .get(i)
            .and_then(|seq| seq.get(0))
            .map_err(|e| MemoryError::Embedding(format!("CLS extraction failed: {e}")))?;

        // L2 normalise with epsilon guard against division by zero
        // (e.g. malformed model weights producing an all-zero CLS vector).
        let norm = cls
            .sqr()
            .and_then(|s| s.sum_all())
            .and_then(|s| s.sqrt())
            .and_then(|n| n.maximum(1e-12))
            .and_then(|n| cls.broadcast_div(&n))
            .map_err(|e| MemoryError::Embedding(format!("L2 normalisation failed: {e}")))?;

        let vector: Vec<f32> = norm
            .to_vec1()
            .map_err(|e| MemoryError::Embedding(format!("tensor to vec failed: {e}")))?;

        results.push(vector);
    }

    Ok(results)
}
