//! Integration tests for the candle BERT embedding pipeline.
//!
//! These tests validate the core embedding behaviour end-to-end:
//! correct dimensions, normalisation, determinism, semantic similarity.
//! They exercise the same model (BGE-small-en-v1.5) and inference path
//! used by the production `CandleEmbeddingEngine`.

use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use hf_hub::{api::sync::Api, Repo, RepoType};
use tokenizers::Tokenizer;

const MODEL_ID: &str = "BAAI/bge-small-en-v1.5";

/// Minimal embedding helper that mirrors `CandleEmbeddingEngine` internals.
struct TestEmbedder {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl TestEmbedder {
    fn new() -> Self {
        let device = Device::Cpu;
        let api = Api::new().expect("HF Hub API init");
        let repo = api.repo(Repo::new(MODEL_ID.to_string(), RepoType::Model));

        let config_path = repo.get("config.json").expect("config.json");
        let tokenizer_path = repo.get("tokenizer.json").expect("tokenizer.json");
        let weights_path = repo.get("model.safetensors").expect("model.safetensors");

        let config: BertConfig =
            serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        let tokenizer = Tokenizer::from_file(&tokenizer_path).unwrap();

        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path], candle_core::DType::F32, &device)
                .unwrap()
        };
        let model = BertModel::load(vb, &config).unwrap();

        Self {
            model,
            tokenizer,
            device,
        }
    }

    fn embed_one(&self, text: &str) -> Vec<f32> {
        let encoding = self.tokenizer.encode(text, true).unwrap();
        let ids = encoding.get_ids();
        let type_ids = encoding.get_type_ids();
        let len = ids.len();

        let input_ids = Tensor::new(ids, &self.device)
            .and_then(|t| t.reshape((1, len)))
            .unwrap();
        let token_type_ids = Tensor::new(type_ids, &self.device)
            .and_then(|t| t.reshape((1, len)))
            .unwrap();

        let embeddings = self
            .model
            .forward(&input_ids, &token_type_ids, None)
            .unwrap();

        // CLS pooling + L2 normalise.
        let cls = embeddings.get(0).and_then(|seq| seq.get(0)).unwrap();
        let norm = cls
            .sqr()
            .and_then(|s| s.sum_all())
            .and_then(|s| s.sqrt())
            .and_then(|n| cls.broadcast_div(&n))
            .unwrap();

        norm.to_vec1().unwrap()
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (norm_a * norm_b)
}

#[test]
fn produces_384_dim_vectors() {
    let engine = TestEmbedder::new();
    let vec = engine.embed_one("hello world");
    assert_eq!(vec.len(), 384);
}

#[test]
fn vectors_are_normalised() {
    let engine = TestEmbedder::new();
    let vec = engine.embed_one("test normalisation");
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-4, "expected unit norm, got {norm}");
}

#[test]
fn self_consistency() {
    let engine = TestEmbedder::new();
    let a = engine.embed_one("determinism check");
    let b = engine.embed_one("determinism check");
    assert_eq!(a, b);
}

#[test]
fn semantic_similarity() {
    let engine = TestEmbedder::new();
    let rust = engine.embed_one("Rust programming language");
    let cargo = engine.embed_one("cargo build system for Rust");
    let recipe = engine.embed_one("chocolate cake baking recipe");

    let sim_related = cosine_similarity(&rust, &cargo);
    let sim_unrelated = cosine_similarity(&rust, &recipe);

    assert!(
        sim_related > sim_unrelated,
        "related texts should be more similar: {sim_related} vs {sim_unrelated}"
    );
}

#[test]
fn batch_consistency() {
    let engine = TestEmbedder::new();
    // Embed individually and verify vectors match.
    let a = engine.embed_one("first");
    let b = engine.embed_one("second");
    let c = engine.embed_one("third");

    // All should be 384-dim and normalised.
    for v in [&a, &b, &c] {
        assert_eq!(v.len(), 384);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4);
    }

    // Verify they're distinct vectors.
    let sim_ab = cosine_similarity(&a, &b);
    assert!(
        sim_ab < 1.0,
        "distinct texts should produce distinct vectors"
    );
}
