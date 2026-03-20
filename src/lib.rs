//! Semantic memory engine with pure-Rust embeddings and git-backed storage.
//!
//! This crate provides the library core for `memory-mcp`, an MCP server that
//! stores and retrieves memories using vector similarity search. Embeddings
//! are computed on-device via candle (BERT inference) with no C/C++ FFI.

#![warn(missing_docs)]

/// Embedding backends for computing vector representations of text.
pub mod embedding;
/// Error types used throughout the crate.
pub mod error;
