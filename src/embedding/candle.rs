use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use hf_hub::{api::sync::Api, Repo, RepoType};
use tokenizers::Tokenizer;

use super::EmbeddingBackend;
use crate::error::MemoryError;

/// Default HuggingFace model ID for BGE-small-en-v1.5.
const DEFAULT_MODEL_ID: &str = "BAAI/bge-small-en-v1.5";

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
    pub fn new(_model_name: &str) -> Result<Self, MemoryError> {
        let device = Device::Cpu;

        let (config, tokenizer, weights_path) =
            load_model_files().map_err(|e| MemoryError::Embedding(e.to_string()))?;

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
            let guard = arc
                .lock()
                .expect("lock poisoned — prior panic corrupted state");
            embed_batch(&guard, &texts)
        })
        .await
        .map_err(|e| MemoryError::Join(e.to_string()))?
    }

    async fn embed_one(&self, text: &str) -> Result<Vec<f32>, MemoryError> {
        let arc = Arc::clone(&self.inner);
        let text = text.to_string();
        let mut results = tokio::task::spawn_blocking(move || {
            let guard = arc
                .lock()
                .expect("lock poisoned — prior panic corrupted state");
            embed_batch(&guard, &[text])
        })
        .await
        .map_err(|e| MemoryError::Join(e.to_string()))??;

        results
            .pop()
            .ok_or_else(|| MemoryError::Embedding("embedding returned no vectors".to_string()))
    }

    fn dimensions(&self) -> usize {
        self.dim
    }
}

// ---------------------------------------------------------------------------
// Model loading
// ---------------------------------------------------------------------------

/// Download (or retrieve from cache) the model files from HuggingFace Hub.
fn load_model_files() -> anyhow::Result<(BertConfig, Tokenizer, PathBuf)> {
    let api = Api::new()?;
    let repo = api.repo(Repo::new(DEFAULT_MODEL_ID.to_string(), RepoType::Model));

    let config_path = repo.get("config.json")?;
    let tokenizer_path = repo.get("tokenizer.json")?;
    let weights_path = repo.get("model.safetensors")?;

    let config: BertConfig = serde_json::from_str(&std::fs::read_to_string(&config_path)?)?;
    let tokenizer = Tokenizer::from_file(&tokenizer_path)
        .map_err(|e| anyhow::anyhow!("failed to load tokenizer: {e}"))?;

    Ok((config, tokenizer, weights_path))
}

// ---------------------------------------------------------------------------
// Inference
// ---------------------------------------------------------------------------

/// Embed a batch of texts one-at-a-time through the BERT model.
///
/// We process individually rather than as a padded batch because candle's
/// multi-head attention can hit `MatMulUnexpectedStriding` with non-contiguous
/// tensors from padded batches. For our workload (short texts, CPU inference)
/// the overhead is negligible.
fn embed_batch(inner: &CandleInner, texts: &[String]) -> Result<Vec<Vec<f32>>, MemoryError> {
    let mut results = Vec::with_capacity(texts.len());

    for text in texts {
        let encoding = inner
            .tokenizer
            .encode(text.as_str(), true)
            .map_err(|e| MemoryError::Embedding(format!("tokenization failed: {e}")))?;

        let ids = encoding.get_ids();
        let type_ids = encoding.get_type_ids();
        let len = ids.len();

        let input_ids = Tensor::new(ids, &inner.device)
            .and_then(|t| t.reshape((1, len)))
            .map_err(|e| MemoryError::Embedding(format!("tensor creation failed: {e}")))?;

        let token_type_ids = Tensor::new(type_ids, &inner.device)
            .and_then(|t| t.reshape((1, len)))
            .map_err(|e| MemoryError::Embedding(format!("tensor creation failed: {e}")))?;

        let embeddings = inner
            .model
            .forward(&input_ids, &token_type_ids, None)
            .map_err(|e| MemoryError::Embedding(format!("BERT forward pass failed: {e}")))?;

        // CLS pooling: take the first token's hidden state.
        let cls = embeddings
            .get(0)
            .and_then(|seq| seq.get(0))
            .map_err(|e| MemoryError::Embedding(format!("CLS extraction failed: {e}")))?;

        // L2 normalise.
        let norm = cls
            .sqr()
            .and_then(|s| s.sum_all())
            .and_then(|s| s.sqrt())
            .and_then(|n| cls.broadcast_div(&n))
            .map_err(|e| MemoryError::Embedding(format!("L2 normalisation failed: {e}")))?;

        let vector: Vec<f32> = norm
            .to_vec1()
            .map_err(|e| MemoryError::Embedding(format!("tensor to vec failed: {e}")))?;

        results.push(vector);
    }

    Ok(results)
}
