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
    ///
    /// `body` must be exactly `span.len()` bytes: the body *is* the parent
    /// content at that span, so a length mismatch is false provenance and
    /// is rejected rather than silently blessed with an id.
    pub fn derive(
        parent_id: impl Into<String>,
        chunker_version: ChunkerVersion,
        span: SourceSpan,
        body: &str,
    ) -> Result<Self, MemoryError> {
        let parent_id = parent_id.into();
        Self::validate_parent_id(&parent_id)?;
        if body.len() != span.len() {
            return Err(MemoryError::InvalidInput {
                reason: format!(
                    "chunk body is {} bytes but source span {span} covers {} bytes — \
                     a fact id must describe the body it is derived from",
                    body.len(),
                    span.len()
                ),
            });
        }

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

    /// Strict parse of the canonical form. Acceptance implies
    /// byte-for-byte canonicality: `parse(s)` succeeds only when
    /// `render(parse(s)) == s`, so noncanonical numeric spellings
    /// (leading zeros, signs) are rejected along with everything else
    /// [`fmt::Display`] cannot produce — there is no lenient mode
    /// (ADR-0019, ADR-0042 P1.3).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        /// Parse a numeric field, accepting only the canonical spelling:
        /// the parsed value must re-render to exactly the input bytes.
        /// Rust's integer `FromStr` tolerates leading zeros and a leading
        /// `+`; laundering those through parse would break P1.3's
        /// render∘parse identity.
        fn parse_canonical<T: FromStr + ToString>(field: &str) -> Option<T> {
            let value = field.parse::<T>().ok()?;
            (value.to_string() == field).then_some(value)
        }

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

        let chunker_version = parse_canonical::<u32>(version)
            .map(ChunkerVersion::new)
            .ok_or_else(|| malformed("chunker version is not a canonically spelled u32"))?;

        let (start, end) = span
            .split_once('-')
            .ok_or_else(|| malformed("span is not '<start>-<end>'"))?;
        let start = parse_canonical::<usize>(start)
            .ok_or_else(|| malformed("span start is not a canonically spelled integer"))?;
        let end = parse_canonical::<usize>(end)
            .ok_or_else(|| malformed("span end is not a canonically spelled integer"))?;
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
/// The parent memory id, chunker version, and source span live inside
/// [`FactRecord::id`] and are deliberately not duplicated as fields — a
/// record and its identity cannot diverge. Accessors delegate.
///
/// Invariant: the id is *true provenance* for the body — re-deriving a
/// `FactId` from the record's own body at the id's coordinates reproduces
/// the id exactly (which implies `body.len() == span.len()`). Enforced at
/// construction ([`FactRecord::new`]) and on deserialization (strict serde
/// per ADR-0019): a persisted catalog entry whose body and id disagree is
/// corrupt derived state and fails closed instead of loading cleanly.
///
/// The catalog is derived, rebuildable state; markdown remains canonical.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawFactRecord")]
pub struct FactRecord {
    /// Deterministic identity (carries parent id, chunker version, span).
    id: FactId,
    /// Scope + name of the parent memory, for resolution and display.
    parent: MemoryRef,
    /// Markdown heading trail from the document root to this chunk
    /// (empty for preamble text before the first heading).
    heading_path: Vec<String>,
    /// The chunk's text — the parent content at the id's span, at the
    /// time the catalog was built.
    body: String,
    /// Tags inherited from the parent memory.
    tags: Vec<String>,
    /// Raw `[[reference]]` targets extracted from the chunk body
    /// (#262 slice 8). Unresolved targets are valid — dangling references
    /// are future-work markers, not errors. Empty until slice 8 lands.
    refs_out: Vec<String>,
}

/// Unvalidated wire shape for [`FactRecord`]; conversion enforces id/body
/// consistency so no inconsistent record can enter through deserialization.
#[derive(Deserialize)]
struct RawFactRecord {
    id: FactId,
    parent: MemoryRef,
    heading_path: Vec<String>,
    body: String,
    tags: Vec<String>,
    refs_out: Vec<String>,
}

impl TryFrom<RawFactRecord> for FactRecord {
    type Error = MemoryError;

    fn try_from(raw: RawFactRecord) -> Result<Self, Self::Error> {
        Self::new(
            raw.id,
            raw.parent,
            raw.heading_path,
            raw.body,
            raw.tags,
            raw.refs_out,
        )
    }
}

/// Check that `id` is true provenance for `body`: deriving a fresh id from
/// the body at the id's own coordinates must reproduce `id` exactly.
/// Rejects both a span whose length disagrees with the body (via
/// [`FactId::derive`]) and a digest that does not match the body's content.
fn validate_id_matches_body(id: &FactId, body: &str) -> Result<(), MemoryError> {
    let derived = FactId::derive(id.parent_id(), id.chunker_version(), id.span(), body)?;
    if derived != *id {
        return Err(MemoryError::InvalidInput {
            reason: format!(
                "fact id '{id}' does not match its paired body: deriving from the body \
                 yields '{derived}' — id and content have diverged"
            ),
        });
    }
    Ok(())
}

impl FactRecord {
    /// Create a catalog record, enforcing that `id` is true provenance for
    /// `body`: the id must re-derive exactly from the body at the id's own
    /// parent/version/span coordinates.
    pub fn new(
        id: FactId,
        parent: MemoryRef,
        heading_path: Vec<String>,
        body: String,
        tags: Vec<String>,
        refs_out: Vec<String>,
    ) -> Result<Self, MemoryError> {
        validate_id_matches_body(&id, &body)?;
        Ok(Self {
            id,
            parent,
            heading_path,
            body,
            tags,
            refs_out,
        })
    }

    /// Deterministic identity (carries parent id, chunker version, span).
    pub const fn id(&self) -> &FactId {
        &self.id
    }

    /// Scope + name of the parent memory.
    pub const fn parent(&self) -> &MemoryRef {
        &self.parent
    }

    /// Markdown heading trail from the document root to this chunk.
    pub fn heading_path(&self) -> &[String] {
        &self.heading_path
    }

    /// The chunk's text — the parent content at the id's span.
    pub fn body(&self) -> &str {
        &self.body
    }

    /// Tags inherited from the parent memory.
    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    /// Raw `[[reference]]` targets extracted from the chunk body.
    pub fn refs_out(&self) -> &[String] {
        &self.refs_out
    }

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
///
/// Invariant: same true-provenance contract as [`FactRecord`] — `text` is
/// the matched chunk's body, so the fact id must re-derive exactly from it.
/// Enforced at construction ([`MatchedChunk::new`]) and on deserialization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawMatchedChunk")]
pub struct MatchedChunk {
    /// Identity of the matched fact (carries parent id and span).
    fact_id: FactId,
    /// Markdown heading trail locating the chunk within its parent.
    heading_path: Vec<String>,
    /// The matched chunk's text.
    text: String,
}

/// Unvalidated wire shape for [`MatchedChunk`]; conversion enforces
/// id/text consistency.
#[derive(Deserialize)]
struct RawMatchedChunk {
    fact_id: FactId,
    heading_path: Vec<String>,
    text: String,
}

impl TryFrom<RawMatchedChunk> for MatchedChunk {
    type Error = MemoryError;

    fn try_from(raw: RawMatchedChunk) -> Result<Self, Self::Error> {
        Self::new(raw.fact_id, raw.heading_path, raw.text)
    }
}

impl MatchedChunk {
    /// Create matched-chunk provenance, enforcing that `fact_id` is true
    /// provenance for `text` (it must re-derive exactly from the text at
    /// the id's own coordinates).
    pub fn new(
        fact_id: FactId,
        heading_path: Vec<String>,
        text: String,
    ) -> Result<Self, MemoryError> {
        validate_id_matches_body(&fact_id, &text)?;
        Ok(Self {
            fact_id,
            heading_path,
            text,
        })
    }

    /// Identity of the matched fact (carries parent id and span).
    pub const fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    /// Markdown heading trail locating the chunk within its parent.
    pub fn heading_path(&self) -> &[String] {
        &self.heading_path
    }

    /// The matched chunk's text.
    pub fn text(&self) -> &str {
        &self.text
    }
}

#[cfg(test)]
#[path = "chunk_tests.rs"]
mod tests;
