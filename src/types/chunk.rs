//! Chunk-addressable retrieval contract (#262 slice 1, ADR-0042).
//!
//! Identity, addressing, and provenance types for fact-level retrieval
//! units ("chunks"). This module is contract only — the deterministic
//! chunker, derived catalog, and index wiring land in later slices.
//! Markdown memories remain the sole source of truth; everything these
//! types describe is derived, rebuildable state.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::error::MemoryError;

use super::memory::MemoryRef;

/// Number of hex characters of the SHA-256 content digest embedded in a
/// rendered [`FactId`] (16 hex chars = 64 bits).
const DIGEST_HEX_LEN: usize = 16;

// ---------------------------------------------------------------------------
// ChunkerVersion
// ---------------------------------------------------------------------------

/// Version of the deterministic chunking algorithm that produced a chunk.
///
/// Participates in [`FactId`] derivation: bumping the version invalidates
/// every derived id, forcing a full catalog rebuild from git truth. There
/// are no chunk migrations — only rebuilds (ADR-0042).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChunkerVersion(u32);

impl ChunkerVersion {
    /// Wrap a chunker algorithm revision number.
    pub const fn new(version: u32) -> Self {
        Self(version)
    }

    /// The raw revision number.
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for ChunkerVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// SourceSpan
// ---------------------------------------------------------------------------

/// A half-open byte range `[start, end)` into a parent memory's markdown
/// body (the `content` field — frontmatter is not addressable).
///
/// Invariant: `start < end` — chunks are never empty. Enforced at
/// construction and on deserialization (strict serde per ADR-0019).
/// Whether the offsets land on UTF-8 character boundaries can only be
/// checked against the parent text; [`SourceSpan::slice_in`] performs that
/// final validation when resolving the span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawSourceSpan")]
pub struct SourceSpan {
    start: usize,
    end: usize,
}

/// Unvalidated wire shape for [`SourceSpan`]; conversion enforces the
/// `start < end` invariant.
#[derive(Deserialize)]
struct RawSourceSpan {
    start: usize,
    end: usize,
}

impl TryFrom<RawSourceSpan> for SourceSpan {
    type Error = MemoryError;

    fn try_from(raw: RawSourceSpan) -> Result<Self, Self::Error> {
        Self::new(raw.start, raw.end)
    }
}

impl SourceSpan {
    /// Create a span, enforcing `start < end`.
    pub fn new(start: usize, end: usize) -> Result<Self, MemoryError> {
        if start >= end {
            return Err(MemoryError::InvalidInput {
                reason: format!("source span {start}..{end} is empty or inverted"),
            });
        }
        Ok(Self { start, end })
    }

    /// Inclusive start byte offset.
    pub const fn start(self) -> usize {
        self.start
    }

    /// Exclusive end byte offset.
    pub const fn end(self) -> usize {
        self.end
    }

    /// Length of the span in bytes (always non-zero — spans are non-empty
    /// by construction, so there is deliberately no `is_empty`).
    #[allow(clippy::len_without_is_empty)]
    pub const fn len(self) -> usize {
        self.end - self.start
    }

    /// Resolve this span against the parent memory's content.
    ///
    /// Errors when the span is out of bounds or either offset falls inside
    /// a multi-byte UTF-8 sequence — a span that does not resolve to valid
    /// UTF-8 is corrupt derived state, never sliced blindly.
    pub fn slice_in<'a>(&self, content: &'a str) -> Result<&'a str, MemoryError> {
        content
            .get(self.start..self.end)
            .ok_or_else(|| MemoryError::InvalidInput {
                reason: format!(
                    "source span {}..{} does not resolve within content of {} bytes \
                     (out of bounds or not on a UTF-8 character boundary)",
                    self.start,
                    self.end,
                    content.len()
                ),
            })
    }
}

impl fmt::Display for SourceSpan {
    /// Renders as `<start>-<end>` — the same form [`FactId`] embeds in its
    /// canonical string.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.start, self.end)
    }
}

// ---------------------------------------------------------------------------
// FactId
// ---------------------------------------------------------------------------

/// Deterministic identity of a single retrieval chunk ("fact").
///
/// Derived from the parent memory's id, the chunker version, the source
/// span, and a SHA-256 digest over all of those plus the chunk body. The
/// same parent content chunked by the same chunker version always yields
/// the same ids, so catalog rebuilds are idempotent and index entries are
/// stably addressable (ADR-0042).
///
/// Canonical string form (also the serde representation):
///
/// ```text
/// fact:v1:<parent-id>:<chunker-version>:<start>-<end>:<digest16>
/// ```
///
/// The parent id is embedded verbatim so parent-wide operations (delete or
/// update every chunk of one memory) can filter ids without a lookup
/// table. The span participates in identity, so two chunks with identical
/// bodies at different positions are distinct facts. The `fact:v1:` prefix
/// versions the id format itself, mirroring the `v1:scope=...` convention
/// used by [`MemoryRef::qualified_path`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FactId {
    parent_id: String,
    chunker_version: ChunkerVersion,
    span: SourceSpan,
    digest: String,
}

impl FactId {
    /// Derive the id for a chunk of `body` at `span` within the parent
    /// memory identified by `parent_id` (the memory's UUID string).
    ///
    /// `parent_id` must be non-empty and must not contain `:` — the
    /// rendered form is colon-delimited, and an id that cannot round-trip
    /// through its canonical string is not an id.
    pub fn derive(
        parent_id: impl Into<String>,
        chunker_version: ChunkerVersion,
        span: SourceSpan,
        body: &str,
    ) -> Result<Self, MemoryError> {
        let parent_id = parent_id.into();
        Self::validate_parent_id(&parent_id)?;

        // Length-prefixed canonical encoding: no field concatenation can
        // collide with a different field split producing the same bytes.
        let mut hasher = Sha256::new();
        hasher.update((parent_id.len() as u64).to_le_bytes());
        hasher.update(parent_id.as_bytes());
        hasher.update(chunker_version.get().to_le_bytes());
        hasher.update((span.start() as u64).to_le_bytes());
        hasher.update((span.end() as u64).to_le_bytes());
        hasher.update((body.len() as u64).to_le_bytes());
        hasher.update(body.as_bytes());
        let full = hasher.finalize();

        let mut digest = String::with_capacity(DIGEST_HEX_LEN);
        for byte in &full[..DIGEST_HEX_LEN / 2] {
            use fmt::Write;
            write!(digest, "{byte:02x}").expect("writing hex to a String cannot fail");
        }

        Ok(Self {
            parent_id,
            chunker_version,
            span,
            digest,
        })
    }

    /// The parent memory's id (UUID string).
    pub fn parent_id(&self) -> &str {
        &self.parent_id
    }

    /// The chunker version this id was derived under.
    pub const fn chunker_version(&self) -> ChunkerVersion {
        self.chunker_version
    }

    /// The chunk's source span within the parent's markdown body.
    pub const fn span(&self) -> SourceSpan {
        self.span
    }

    /// The truncated content digest (16 lowercase hex characters).
    pub fn digest(&self) -> &str {
        &self.digest
    }

    fn validate_parent_id(parent_id: &str) -> Result<(), MemoryError> {
        if parent_id.is_empty() {
            return Err(MemoryError::InvalidInput {
                reason: "fact id parent id must not be empty".to_string(),
            });
        }
        if parent_id.contains(':') {
            return Err(MemoryError::InvalidInput {
                reason: format!(
                    "fact id parent id '{parent_id}' contains ':' — \
                     it would corrupt the colon-delimited canonical form"
                ),
            });
        }
        Ok(())
    }
}

impl fmt::Display for FactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "fact:v1:{}:{}:{}:{}",
            self.parent_id, self.chunker_version, self.span, self.digest
        )
    }
}

impl FromStr for FactId {
    type Err = MemoryError;

    /// Strict parse of the canonical form. Anything that does not
    /// round-trip byte-for-byte through [`fmt::Display`] is rejected —
    /// there is no lenient mode (ADR-0019).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let malformed = |detail: &str| MemoryError::InvalidInput {
            reason: format!("malformed fact id '{s}': {detail}"),
        };

        let rest = s
            .strip_prefix("fact:v1:")
            .ok_or_else(|| malformed("missing 'fact:v1:' prefix"))?;

        // parent_id is validated to contain no ':', so the remainder splits
        // into exactly four colon-delimited fields.
        let parts: Vec<&str> = rest.split(':').collect();
        let [parent_id, version, span, digest] = parts.as_slice() else {
            return Err(malformed(
                "expected '<parent>:<version>:<start>-<end>:<digest>' after prefix",
            ));
        };

        Self::validate_parent_id(parent_id)?;

        let chunker_version = version
            .parse::<u32>()
            .map(ChunkerVersion::new)
            .map_err(|_| malformed("chunker version is not a u32"))?;

        let (start, end) = span
            .split_once('-')
            .ok_or_else(|| malformed("span is not '<start>-<end>'"))?;
        let start = start
            .parse::<usize>()
            .map_err(|_| malformed("span start is not an integer"))?;
        let end = end
            .parse::<usize>()
            .map_err(|_| malformed("span end is not an integer"))?;
        let span = SourceSpan::new(start, end)?;

        if digest.len() != DIGEST_HEX_LEN
            || !digest.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
        {
            return Err(malformed(&format!(
                "digest must be exactly {DIGEST_HEX_LEN} lowercase hex characters"
            )));
        }

        Ok(Self {
            parent_id: (*parent_id).to_string(),
            chunker_version,
            span,
            digest: (*digest).to_string(),
        })
    }
}

impl Serialize for FactId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for FactId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// FactRecord
// ---------------------------------------------------------------------------

/// One entry in the derived retrieval catalog (#262 slice 3).
///
/// Data-like wire DTO with public fields. The parent memory id, chunker
/// version, and source span live inside [`FactRecord::id`] and are
/// deliberately not duplicated as fields — a record and its identity
/// cannot diverge. Accessors delegate.
///
/// The catalog is derived, rebuildable state; markdown remains canonical.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactRecord {
    /// Deterministic identity (carries parent id, chunker version, span).
    pub id: FactId,
    /// Scope + name of the parent memory, for resolution and display.
    pub parent: MemoryRef,
    /// Markdown heading trail from the document root to this chunk
    /// (empty for preamble text before the first heading).
    pub heading_path: Vec<String>,
    /// The chunk's text — the parent content at the id's span, at the
    /// time the catalog was built.
    pub body: String,
    /// Tags inherited from the parent memory.
    pub tags: Vec<String>,
    /// Raw `[[reference]]` targets extracted from the chunk body
    /// (#262 slice 8). Unresolved targets are valid — dangling references
    /// are future-work markers, not errors. Empty until slice 8 lands.
    pub refs_out: Vec<String>,
}

impl FactRecord {
    /// The parent memory's id (UUID string), from [`FactRecord::id`].
    pub fn parent_id(&self) -> &str {
        self.id.parent_id()
    }

    /// The chunker version, from [`FactRecord::id`].
    pub const fn chunker_version(&self) -> ChunkerVersion {
        self.id.chunker_version()
    }

    /// The source span within the parent's markdown body, from
    /// [`FactRecord::id`].
    pub const fn span(&self) -> SourceSpan {
        self.id.span()
    }
}

// ---------------------------------------------------------------------------
// MatchedChunk
// ---------------------------------------------------------------------------

/// Matched-chunk provenance attached to a recall hit (#262 slice 7).
///
/// Additive wire compatibility (ADR-0042): existing recall result fields
/// are unchanged; once chunk retrieval is wired, hits additionally carry
/// the specific fact that matched so clients see the precise hit and know
/// where to drill in. `read` still returns the whole parent memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchedChunk {
    /// Identity of the matched fact (carries parent id and span).
    pub fact_id: FactId,
    /// Markdown heading trail locating the chunk within its parent.
    pub heading_path: Vec<String>,
    /// The matched chunk's text.
    pub text: String,
}

#[cfg(test)]
#[path = "chunk_tests.rs"]
mod tests;
