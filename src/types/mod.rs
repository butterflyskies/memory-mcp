//! Domain types: memories, scopes, metadata, validation, and application state.

mod args;
mod chunk;
mod memory;
mod scope;
mod validated;

// Re-export everything that was previously public from types.rs.
// Items that were `pub` remain `pub`; items that were `pub(crate)` are
// re-exported as `pub(crate)`.

pub(crate) use args::ResolvedChanges;
pub use args::{
    AppState, BatchMarkAppliedArgs, ChangedMemories, EditArgs, ForgetArgs, ListArgs, ListField,
    MarkAppliedArgs, MoveArgs, PullResult, ReadArgs, RecallArgs, RecallStatsArgs, ReindexStats,
    RememberArgs, SyncArgs, Verdict, VerdictEntry,
};

pub(crate) use args::{ListToolArgs, LIST_MAX_LIMIT};

pub use chunk::{ChunkerVersion, FactId, SourceSpan};

// The catalog and recall-wire shapes stay off the public surface until
// their owning slices (#262 slices 3 and 7) produce an external
// consumer (ADR-0042) — every `pub` item is a semver commitment.
// `FactRecord` gained a crate-internal consumer in slice 2 (the
// chunker), so it is re-exported `pub(crate)`; `MatchedChunk` still has
// none and stays internal to `chunk`.
pub(crate) use chunk::FactRecord;

pub use memory::{parse_qualified_name, Memory, MemoryMetadata, MemoryName, MemoryRef};

pub use scope::{validate_branch_name, Scope, ScopeFilter, ScopePath};
