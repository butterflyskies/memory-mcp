mod candle;

#[cfg(feature = "legacy-fastembed")]
mod fastembed;

use crate::error::MemoryError;

pub use self::candle::CandleEmbeddingEngine;

#[cfg(feature = "legacy-fastembed")]
pub use self::fastembed::FastEmbedEngine;

/// Trait abstracting embedding backends so we can swap implementations
/// (candle, fastembed, remote APIs) without changing calling code.
#[async_trait::async_trait]
pub trait EmbeddingBackend: Send + Sync {
    /// Embed a batch of texts, returning one vector per input.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, MemoryError>;

    /// Convenience: embed a single text.
    async fn embed_one(&self, text: &str) -> Result<Vec<f32>, MemoryError>;

    /// Number of dimensions produced by the model.
    fn dimensions(&self) -> usize;
}
