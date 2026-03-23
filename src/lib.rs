//! Semantic memory engine with pure-Rust embeddings and git-backed storage.
//!
//! This crate provides the library core for `memory-mcp`, an MCP server that
//! stores and retrieves memories using vector similarity search. Embeddings
//! are computed on-device via candle (BERT inference) with no C/C++ FFI.

#![warn(missing_docs)]

/// Token resolution, OAuth device flow, and credential storage backends.
pub mod auth;
/// Embedding backends for computing vector representations of text.
pub mod embedding;
/// Error types used throughout the crate.
pub mod error;
/// HNSW vector index for approximate nearest-neighbour search.
pub mod index;
/// Git-backed memory repository — read, write, sync, and diff operations.
pub mod repo;
/// MCP server implementation — tool handlers for the memory protocol.
pub mod server;
/// Bounded session manager with FIFO eviction for limiting concurrent sessions.
pub mod session;
/// Domain types: memories, scopes, metadata, validation, and application state.
pub mod types;
