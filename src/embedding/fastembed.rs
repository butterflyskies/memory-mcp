use std::sync::{Arc, Mutex};

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

use super::EmbeddingBackend;
use crate::error::MemoryError;

/// Wraps `fastembed::TextEmbedding` behind an `Arc<Mutex<...>>` and exposes a
/// minimal embedding API.
///
/// `TextEmbedding::embed` takes `&mut self`, so interior mutability is
/// required. We use `std::sync::Mutex` combined with `tokio::task::spawn_blocking`
/// so blocking embed work doesn't occupy executor threads.
pub struct FastEmbedEngine {
    inner: Arc<Mutex<TextEmbedding>>,
    dim: usize,
}

impl FastEmbedEngine {
    /// Initialise the embedding engine for the named model.
    ///
    /// `model_name` is matched case-insensitively against known
    /// `EmbeddingModel` variants. Falls back to `BGESmallENV15` (the
    /// fastembed default) if the name is unrecognised.
    pub fn new(model_name: &str) -> Result<Self, MemoryError> {
        let model = parse_model(model_name);
        let dim = model_dimensions(&model);

        let inner =
            TextEmbedding::try_new(InitOptions::new(model).with_show_download_progress(true))
                .map_err(|e| MemoryError::Embedding(e.to_string()))?;

        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
            dim,
        })
    }
}

#[async_trait::async_trait]
impl EmbeddingBackend for FastEmbedEngine {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, MemoryError> {
        let arc = Arc::clone(&self.inner);
        let texts = texts.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut guard = arc
                .lock()
                .expect("lock poisoned — prior panic corrupted state");
            guard
                .embed(texts, None)
                .map_err(|e| MemoryError::Embedding(e.to_string()))
        })
        .await
        .map_err(|e| MemoryError::Join(e.to_string()))?
    }

    async fn embed_one(&self, text: &str) -> Result<Vec<f32>, MemoryError> {
        let arc = Arc::clone(&self.inner);
        let text = text.to_string();
        let mut results = tokio::task::spawn_blocking(move || {
            let mut guard = arc
                .lock()
                .expect("lock poisoned — prior panic corrupted state");
            guard
                .embed(vec![text], None)
                .map_err(|e| MemoryError::Embedding(e.to_string()))
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
// Helpers
// ---------------------------------------------------------------------------

/// Parse a model name string into an `EmbeddingModel` variant.
///
/// Matching is case-insensitive and hyphen/underscore-agnostic.
/// Returns `EmbeddingModel::BGESmallENV15` for unknown names.
fn parse_model(name: &str) -> EmbeddingModel {
    let normalised: String = name
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();

    match normalised.as_str() {
        "allminilml6v2" => EmbeddingModel::AllMiniLML6V2,
        "allminilml6v2q" => EmbeddingModel::AllMiniLML6V2Q,
        "allminilml12v2" => EmbeddingModel::AllMiniLML12V2,
        "allminilml12v2q" => EmbeddingModel::AllMiniLML12V2Q,
        "bgesmallenv15" | "bgesmallenglishv15" => EmbeddingModel::BGESmallENV15,
        "bgesmallenv15q" => EmbeddingModel::BGESmallENV15Q,
        "bgebaseenv15" | "bgebaseenglishv15" => EmbeddingModel::BGEBaseENV15,
        "bgebaseenv15q" => EmbeddingModel::BGEBaseENV15Q,
        "bgelargeenv15" => EmbeddingModel::BGELargeENV15,
        "bgelargeenv15q" => EmbeddingModel::BGELargeENV15Q,
        "bgem3" => EmbeddingModel::BGEM3,
        "nomicembedtextv1" => EmbeddingModel::NomicEmbedTextV1,
        "nomicembedtextv15" => EmbeddingModel::NomicEmbedTextV15,
        "nomicembedtextv15q" => EmbeddingModel::NomicEmbedTextV15Q,
        "multilinguale5small" => EmbeddingModel::MultilingualE5Small,
        "multilinguale5base" => EmbeddingModel::MultilingualE5Base,
        "multilinguale5large" => EmbeddingModel::MultilingualE5Large,
        "mxbaiembedlargev1" => EmbeddingModel::MxbaiEmbedLargeV1,
        "mxbaiembedlargev1q" => EmbeddingModel::MxbaiEmbedLargeV1Q,
        "snowflakearcticeembedxs" | "snowflakearcticembedxs" => {
            EmbeddingModel::SnowflakeArcticEmbedXS
        }
        "snowflakearcticembeds" => EmbeddingModel::SnowflakeArcticEmbedS,
        "snowflakearcticembedm" => EmbeddingModel::SnowflakeArcticEmbedM,
        "snowflakearcticembedl" => EmbeddingModel::SnowflakeArcticEmbedL,
        _ => {
            tracing::warn!(
                "Unknown embedding model '{}', falling back to BGESmallENV15",
                name
            );
            EmbeddingModel::BGESmallENV15
        }
    }
}

/// Known output dimensions for common models.
fn model_dimensions(model: &EmbeddingModel) -> usize {
    match model {
        EmbeddingModel::AllMiniLML6V2 | EmbeddingModel::AllMiniLML6V2Q => 384,
        EmbeddingModel::AllMiniLML12V2 | EmbeddingModel::AllMiniLML12V2Q => 384,
        EmbeddingModel::BGESmallENV15 | EmbeddingModel::BGESmallENV15Q => 384,
        EmbeddingModel::BGEBaseENV15 | EmbeddingModel::BGEBaseENV15Q => 768,
        EmbeddingModel::BGELargeENV15 | EmbeddingModel::BGELargeENV15Q => 1024,
        EmbeddingModel::BGEM3 => 1024,
        EmbeddingModel::NomicEmbedTextV1
        | EmbeddingModel::NomicEmbedTextV15
        | EmbeddingModel::NomicEmbedTextV15Q => 768,
        EmbeddingModel::MultilingualE5Small => 384,
        EmbeddingModel::MultilingualE5Base => 768,
        EmbeddingModel::MultilingualE5Large => 1024,
        EmbeddingModel::MxbaiEmbedLargeV1 | EmbeddingModel::MxbaiEmbedLargeV1Q => 1024,
        EmbeddingModel::SnowflakeArcticEmbedXS | EmbeddingModel::SnowflakeArcticEmbedXSQ => 384,
        EmbeddingModel::SnowflakeArcticEmbedS | EmbeddingModel::SnowflakeArcticEmbedSQ => 384,
        EmbeddingModel::SnowflakeArcticEmbedM | EmbeddingModel::SnowflakeArcticEmbedMQ => 768,
        EmbeddingModel::SnowflakeArcticEmbedL | EmbeddingModel::SnowflakeArcticEmbedLQ => 1024,
        _ => {
            // EmbeddingModel is non-exhaustive; log so operators know they
            // may be getting the wrong dimension for a newly-added model.
            tracing::warn!(
                "model_dimensions: unrecognised EmbeddingModel variant — \
                 defaulting to 384 dimensions; verify this is correct for your model"
            );
            384
        }
    }
}
