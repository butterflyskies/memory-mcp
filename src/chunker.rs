//! Deterministic Markdown chunker (#262 slice 2, ADR-0042).
//!
//! Splits a parent memory's markdown body into fact-level retrieval
//! units: split on structural boundaries, coalesce adjacent units under
//! the embedding tokenizer's budget, deterministically split oversized
//! indivisible blocks, no overlap, one chunk for short memories. The
//! output is an ordered sequence of chunks whose spans tile the source
//! exactly — concatenating the chunk bodies in span order reproduces the
//! parent content byte for byte.
//!
//! This module is pure: nothing here touches the catalog, the indexes,
//! or the server. Wiring is #262 slices 3+.
//!
//! # Boundary policy
//!
//! Parsing is line-based and intentionally simpler than full CommonMark;
//! it exists to find *stable, deterministic* split points, not to render
//! markdown. The recognized structural units are:
//!
//! - **ATX headings** (`#`–`######` after at most 3 leading spaces,
//!   followed by whitespace or end of line). A heading is a single-line
//!   unit and updates the heading path; a level-N heading pops every
//!   stacked heading of level ≥ N. Trailing closing-hash sequences are
//!   stripped from the recorded heading text. Setext headings are not
//!   recognized (their underline reads as a paragraph line).
//! - **Fenced code blocks** (``` or `~~~`, at least 3 fence characters
//!   after at most 3 leading spaces; a backtick fence's info string must
//!   not contain a backtick). The whole fence, including its delimiter
//!   lines, is one atomic unit. An unclosed fence runs to end of input.
//!   Fences indented 4+ spaces or by tabs are treated as plain text.
//! - **List items**: a line whose content (after any indentation) starts
//!   with `-`/`+`/`*` or `1.`-style ordered markers followed by
//!   whitespace begins a new unit; following non-blank, non-marker lines
//!   attach to it (lazy continuation). Nested markers start their own
//!   units regardless of indent depth.
//! - **Paragraphs**: any other maximal run of non-blank lines.
//!
//! Blank lines terminate the current unit. Indented (4-space) code
//! blocks are not distinguished from paragraphs.
//!
//! # Whitespace policy
//!
//! Chunk spans tile the source with no gaps: the whitespace between two
//! units (blank lines, trailing newlines) attaches to the *preceding*
//! chunk, and whitespace before the first unit attaches to the *first*
//! chunk. Coverage is therefore exact — there is no "modulo whitespace"
//! caveat (ADR-0042 P2.4). A body consisting only of whitespace is a
//! single chunk.
//!
//! # Token budget
//!
//! Budget decisions use the *actual embedding tokenizer* via
//! [`TokenCounter`] — never a byte- or word-count heuristic. Counts
//! include special tokens (`[CLS]`/`[SEP]`), matching what the embedding
//! path truncates against (the failure this slice exists to fix: BGE's
//! 512-token truncation silently clipped 18 of 191 measured memories).
//! The counter and budget are constructor inputs, not constants: the
//! current instantiation is BGE-small-en-v1.5 at 512 tokens, and the
//! anticipated successor is ModernBERT, whose ~8192-token context
//! radically changes chunk economics (bigger budget, far fewer splits).
//! The chunker must work unchanged across that swap.
//!
//! # Oversized indivisible blocks
//!
//! A single unit over budget is split deterministically: first at line
//! boundaries (packing whole lines under the budget), then — for a
//! single line over budget — at character boundaries (packing
//! characters under the budget). Piece boundaries come from a bounded
//! doubling-then-bisection probe (see `packed_len`), never a
//! per-character scan over a growing substring, so the number of
//! tokenizer calls is logarithmic per piece — a maximum-size one-line
//! memory cannot turn catalog rebuild into ~10⁶ full tokenizations.
//! Every emitted piece is verified against the budget; because real
//! WordPiece counts are not monotone over prefixes, the probed
//! boundary is *a* deterministic fitting one (identical to a greedy
//! scan whenever counts are monotone). Pieces never re-merge across a
//! split. Documented fallback: if even a single character exceeds the budget
//! (only possible when the budget is smaller than the special-token
//! overhead plus one), that character is emitted alone as an over-budget
//! chunk — deterministic, and the only case ADR-0042 P2.6 permits over
//! budget.
//!
//! # Versioning
//!
//! [`CHUNKER_VERSION`] names this algorithm revision and is embedded in
//! every derived `FactId`. The full identity of a chunking run also
//! includes the tokenizer and budget: [`ChunkerConfig::fingerprint`]
//! combines all three, and the slice-3 catalog stamps it so that an
//! embedding-model swap (e.g. BGE → ModernBERT) is detectable staleness
//! forcing a full rebuild — chunk vintages from different tokenizers or
//! budgets are never silently mixed (ADR-0042 "stale schema/model"
//! ledger row).

use sha2::{Digest, Sha256};
use tokenizers::Tokenizer;

use crate::error::MemoryError;
use crate::types::{ChunkerVersion, FactId, FactRecord, MemoryRef, SourceSpan};

/// Revision of the chunking algorithm implemented by this module.
///
/// Bump on any change to the boundary policy, whitespace policy,
/// coalescing, or splitting behavior — every derived `FactId` embeds
/// this, so a bump invalidates the catalog and forces a rebuild from
/// git truth (ADR-0042). Tokenizer/budget changes are carried by
/// [`ChunkerConfig::fingerprint`] instead, so they do not require an
/// algorithm bump to trigger a rebuild.
pub(crate) const CHUNKER_VERSION: ChunkerVersion = ChunkerVersion::new(1);

// ---------------------------------------------------------------------------
// Token counting
// ---------------------------------------------------------------------------

/// Counts embedding-tokenizer tokens for budget decisions.
///
/// Implementations must be deterministic (same text, same count) and
/// must count with the same settings the embedding path encodes with —
/// including special tokens — so "fits the budget" here means "will not
/// be truncated there". `identity` participates in the chunker
/// fingerprint and MUST be bound to the tokenizer's actual behavior,
/// not just a human-readable label: two tokenizer artifacts that count
/// differently must never share an identity, or the slice-3 staleness
/// stamp would retain stale chunk vintages across an upstream tokenizer
/// revision. [`HfTokenCounter`] satisfies this by including a digest of
/// the tokenizer's canonical serialization. Swapping the embedding
/// model (BGE today, ModernBERT anticipated) — or silently receiving a
/// re-released tokenizer under the same model id — changes the
/// fingerprint and invalidates derived chunk state.
pub(crate) trait TokenCounter: Send + Sync {
    /// Stable identity of the tokenizer, bound to its actual content
    /// (e.g. `BAAI/bge-small-en-v1.5@sha256:<digest16>`).
    fn identity(&self) -> &str;

    /// Number of tokens `text` encodes to, including special tokens.
    fn count(&self, text: &str) -> Result<usize, MemoryError>;
}

/// [`TokenCounter`] backed by a HuggingFace `tokenizers` tokenizer —
/// the same tokenizer family the embedding path loads (BGE today; a
/// ModernBERT tokenizer drops in the same way).
pub(crate) struct HfTokenCounter {
    tokenizer: Tokenizer,
    identity: String,
}

impl HfTokenCounter {
    /// Wrap a tokenizer for counting. Truncation and padding are
    /// cleared so counts are true sequence lengths, not clipped or
    /// padded ones.
    ///
    /// `label` is the human-readable name (e.g. the HuggingFace model
    /// id); the counter's [`TokenCounter::identity`] combines it with a
    /// SHA-256 digest of the tokenizer's canonical serialization *as
    /// configured for counting* (truncation/padding cleared). The
    /// digest covers the whole pipeline — vocab, normalizer,
    /// pre-tokenizer, post-processor, added/special tokens — so two
    /// different tokenizer artifacts can never share an identity even
    /// under the same label, and a mutable HF revision resolving to new
    /// `tokenizer.json` bytes is a visible fingerprint change (a
    /// staleness signal for the slice-3 catalog), not a silent mix of
    /// chunk vintages.
    pub(crate) fn new(
        mut tokenizer: Tokenizer,
        label: impl Into<String>,
    ) -> Result<Self, MemoryError> {
        tokenizer.with_truncation(None).map_err(|e| {
            MemoryError::Embedding(format!("failed to clear tokenizer truncation: {e}"))
        })?;
        tokenizer.with_padding(None);
        let canonical = tokenizer
            .to_string(false)
            .map_err(|e| MemoryError::Embedding(format!("failed to serialize tokenizer: {e}")))?;
        let digest = Sha256::digest(canonical.as_bytes());
        let mut identity = label.into();
        identity.push_str("@sha256:");
        for byte in &digest[..8] {
            use std::fmt::Write;
            write!(identity, "{byte:02x}").expect("writing hex to a String cannot fail");
        }
        Ok(Self {
            tokenizer,
            identity,
        })
    }
}

impl TokenCounter for HfTokenCounter {
    fn identity(&self) -> &str {
        &self.identity
    }

    fn count(&self, text: &str) -> Result<usize, MemoryError> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| MemoryError::Embedding(format!("tokenization failed: {e}")))?;
        Ok(encoding.get_ids().len())
    }
}

// ---------------------------------------------------------------------------
// ChunkerConfig
// ---------------------------------------------------------------------------

/// Chunking parameters: which tokenizer counts, and how many tokens a
/// chunk may occupy.
///
/// The budget is a constructor input taken from the embedding model's
/// configuration (its maximum sequence length), never a constant baked
/// in here — BGE-512 is the current instantiation, ModernBERT (~8192)
/// the anticipated next one.
pub(crate) struct ChunkerConfig {
    counter: Box<dyn TokenCounter>,
    max_tokens: usize,
}

impl ChunkerConfig {
    /// Create a config. `max_tokens` must be non-zero (a zero budget
    /// admits nothing, not even the tokenizer's special tokens).
    pub(crate) fn new(
        counter: Box<dyn TokenCounter>,
        max_tokens: usize,
    ) -> Result<Self, MemoryError> {
        if max_tokens == 0 {
            return Err(MemoryError::InvalidInput {
                reason: "chunker token budget must be non-zero".to_string(),
            });
        }
        Ok(Self {
            counter,
            max_tokens,
        })
    }

    /// The token budget a chunk must fit within.
    pub(crate) fn max_tokens(&self) -> usize {
        self.max_tokens
    }

    /// Number of tokens `text` encodes to under this config's tokenizer.
    pub(crate) fn count(&self, text: &str) -> Result<usize, MemoryError> {
        self.counter.count(text)
    }

    /// Full identity of a chunking run: algorithm revision, tokenizer
    /// identity (content-bound — see [`TokenCounter::identity`]), and
    /// token budget.
    ///
    /// The slice-3 catalog stamps this string; a mismatch on load —
    /// algorithm change, embedding-model swap (BGE → ModernBERT), an
    /// upstream tokenizer revision under the same model id, or a
    /// budget change — is detectable staleness and forces a full
    /// rebuild rather than mixing chunk vintages (ADR-0042).
    pub(crate) fn fingerprint(&self) -> String {
        format!(
            "chunker:v{}:tokenizer={}:budget={}",
            CHUNKER_VERSION,
            self.counter.identity(),
            self.max_tokens
        )
    }

    fn fits(&self, text: &str) -> Result<bool, MemoryError> {
        Ok(self.counter.count(text)? <= self.max_tokens)
    }
}

// ---------------------------------------------------------------------------
// Chunk
// ---------------------------------------------------------------------------

/// One chunk of a parent body: a span tiling the source plus the
/// markdown heading trail in effect at the chunk's first unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Chunk {
    /// Byte range into the parent body. Chunk spans are strictly
    /// ordered, non-overlapping, and tile the body exactly.
    pub(crate) span: SourceSpan,
    /// Heading trail from the document root to this chunk (empty for
    /// preamble before the first heading). A chunk that starts with a
    /// heading includes that heading in its own trail.
    pub(crate) heading_path: Vec<String>,
}

// ---------------------------------------------------------------------------
// Chunking
// ---------------------------------------------------------------------------

/// Chunk a parent body deterministically under `config`.
///
/// Empty content yields no chunks (a `SourceSpan` cannot be empty);
/// any non-empty content yields at least one. Content whose total
/// token count fits the budget yields exactly one chunk spanning the
/// whole body.
pub(crate) fn chunk_markdown(
    content: &str,
    config: &ChunkerConfig,
) -> Result<Vec<Chunk>, MemoryError> {
    if content.is_empty() {
        return Ok(Vec::new());
    }

    let mut units = parse_units(content);
    if units.is_empty() {
        // Whitespace-only body: no structural units, one chunk.
        units.push(Unit {
            start: 0,
            end: content.len(),
            heading_path: Vec::new(),
        });
    }
    tile(&mut units, content.len());

    let mut chunks: Vec<Chunk> = Vec::new();
    let mut i = 0usize;
    while i < units.len() {
        // Coalescing: pack as many whole units as the budget admits,
        // measuring the *exact final body* (tiled slice, whitespace
        // included). The boundary comes from a bounded probe, not a
        // per-unit scan over a growing substring (see [`packed_len`]).
        let take = packed_len(units.len() - i, |n| {
            config.fits(&content[units[i].start..units[i + n - 1].end])
        })?;
        if take == 0 {
            // A single unit over budget on its own: split it.
            split_oversized(
                &content[units[i].start..units[i].end],
                units[i].start,
                &units[i].heading_path,
                config,
                &mut chunks,
            )?;
            i += 1;
        } else {
            push_chunk(
                &mut chunks,
                units[i].start,
                units[i + take - 1].end,
                &units[i].heading_path,
            )?;
            i += take;
        }
    }
    Ok(chunks)
}

/// Chunk a parent body and derive true-provenance catalog records.
///
/// Every record's `FactId` embeds `parent_id`, [`CHUNKER_VERSION`], the
/// chunk span, and the body digest; `FactRecord::new` re-verifies the
/// id against the body at construction, so the output cannot carry
/// false provenance. `refs_out` stays empty until #262 slice 8 lands
/// reference extraction.
pub(crate) fn chunk_into_facts(
    parent_id: &str,
    parent: &MemoryRef,
    content: &str,
    tags: &[String],
    config: &ChunkerConfig,
) -> Result<Vec<FactRecord>, MemoryError> {
    chunk_markdown(content, config)?
        .into_iter()
        .map(|chunk| {
            let body = chunk.span.slice_in(content)?;
            let id = FactId::derive(parent_id, CHUNKER_VERSION, chunk.span, body)?;
            FactRecord::new(
                id,
                parent.clone(),
                chunk.heading_path,
                body.to_string(),
                tags.to_vec(),
                Vec::new(),
            )
        })
        .collect()
}

fn push_chunk(
    chunks: &mut Vec<Chunk>,
    start: usize,
    end: usize,
    heading_path: &[String],
) -> Result<(), MemoryError> {
    chunks.push(Chunk {
        span: SourceSpan::new(start, end)?,
        heading_path: heading_path.to_vec(),
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Bounded packing search
// ---------------------------------------------------------------------------

/// Deterministic bounded search for a packing boundary: how many of the
/// `total` leading candidates (structural units, lines, or characters)
/// the next piece takes.
///
/// `fits(n)` reports whether the piece formed by the first `n`
/// candidates fits the token budget. The probe doubles `n` until a
/// non-fitting count brackets the boundary, then bisects the bracket,
/// so `fits` runs O(log total) times per piece instead of once per
/// candidate over a growing substring — the previous per-candidate
/// scan made a maximum-size single-line memory cost ~10⁶ full
/// tokenizations, a rebuild/startup DoS once the chunker is wired into
/// the catalog (see the operation-budget regression test).
///
/// Returns the chosen count; `0` means even the first candidate alone
/// is over budget (the caller applies its indivisible-fallback
/// policy). Real WordPiece token counts are not monotone over prefixes
/// (BGE collapses >100-char words to `[UNK]`), so the result is *a*
/// deterministic fitting boundary — identical to the greedy
/// stop-at-first-overflow scan whenever counts are monotone — and
/// every returned `n > 0` was verified by `fits` before being chosen.
fn packed_len(
    total: usize,
    mut fits: impl FnMut(usize) -> Result<bool, MemoryError>,
) -> Result<usize, MemoryError> {
    debug_assert!(total > 0, "packing an empty candidate list");
    let mut lo = 0usize; // largest count known to fit
    let mut hi = loop {
        // Doubling probe: 1, 2, 4, … until over budget or exhausted.
        let n = lo.saturating_mul(2).max(1).min(total);
        if !fits(n)? {
            break n;
        }
        lo = n;
        if n == total {
            return Ok(total);
        }
    };
    // Bisect the (fits, does-not-fit] bracket.
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if fits(mid)? {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Ok(lo)
}

// ---------------------------------------------------------------------------
// Oversized-unit splitting
// ---------------------------------------------------------------------------

/// Split one over-budget unit (`text`, at absolute offset `base`) into
/// deterministic pieces: whole-line packing first, then
/// character-boundary packing for any single line over budget. Piece
/// boundaries come from [`packed_len`]'s bounded probe. Pieces tile
/// `text` exactly and never re-merge across a split boundary.
fn split_oversized(
    text: &str,
    base: usize,
    heading_path: &[String],
    config: &ChunkerConfig,
    chunks: &mut Vec<Chunk>,
) -> Result<(), MemoryError> {
    // bounds[k] is the byte offset after the first k lines.
    let mut bounds = vec![0usize];
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        bounds.push(offset);
    }
    let lines = bounds.len() - 1;

    let mut start = 0usize; // index into `bounds`
    while start < lines {
        let take = packed_len(lines - start, |n| {
            config.fits(&text[bounds[start]..bounds[start + n]])
        })?;
        if take == 0 {
            // A single line over budget: split at character boundaries.
            char_split(
                &text[bounds[start]..bounds[start + 1]],
                base + bounds[start],
                heading_path,
                config,
                chunks,
            )?;
            start += 1;
        } else {
            push_chunk(
                chunks,
                base + bounds[start],
                base + bounds[start + take],
                heading_path,
            )?;
            start += take;
        }
    }
    Ok(())
}

/// Split a single over-budget line (`seg`, at absolute offset `base`)
/// at UTF-8 character boundaries, each piece a [`packed_len`]-probed
/// run of characters that fits. Documented fallback: a single
/// character that alone exceeds the budget is emitted alone, over
/// budget — the only over-budget chunk shape this chunker produces
/// (ADR-0042 P2.6).
fn char_split(
    seg: &str,
    base: usize,
    heading_path: &[String],
    config: &ChunkerConfig,
    chunks: &mut Vec<Chunk>,
) -> Result<(), MemoryError> {
    // bounds[k] is the byte offset after the first k characters.
    let mut bounds: Vec<usize> = seg.char_indices().map(|(i, _)| i).collect();
    bounds.push(seg.len());
    let chars = bounds.len() - 1;

    let mut start = 0usize; // index into `bounds`
    while start < chars {
        let take = packed_len(chars - start, |n| {
            config.fits(&seg[bounds[start]..bounds[start + n]])
        })?
        // Indivisible fallback: one character over budget on its own.
        .max(1);
        push_chunk(
            chunks,
            base + bounds[start],
            base + bounds[start + take],
            heading_path,
        )?;
        start += take;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Structural parsing
// ---------------------------------------------------------------------------

/// One structural unit of the source, before tiling: `start..end` is
/// the unit's own lines (including their newlines), and `heading_path`
/// is the heading trail in effect at the unit.
#[derive(Debug)]
struct Unit {
    start: usize,
    end: usize,
    heading_path: Vec<String>,
}

/// Extend unit spans so they tile the whole body: each unit absorbs the
/// whitespace up to the next unit's start, the first unit absorbs any
/// leading whitespace, and the last runs to end of input.
fn tile(units: &mut [Unit], content_len: usize) {
    let next_starts: Vec<usize> = units.iter().skip(1).map(|u| u.start).collect();
    for (unit, next_start) in units.iter_mut().zip(next_starts) {
        unit.end = next_start;
    }
    if let Some(last) = units.last_mut() {
        last.end = content_len;
    }
    if let Some(first) = units.first_mut() {
        first.start = 0;
    }
}

/// Line-based structural parse (boundary policy in the module docs).
/// Deterministic by construction: a single forward pass with no
/// lookahead beyond the current line.
fn parse_units(content: &str) -> Vec<Unit> {
    /// Inside a fenced code block opened by `marker` × `len`.
    struct Fence {
        marker: char,
        len: usize,
    }

    let mut units: Vec<Unit> = Vec::new();
    let mut open: Option<Unit> = None;
    let mut fence: Option<Fence> = None;
    // (level, text) pairs; the path snapshot is the texts in order.
    let mut heading_stack: Vec<(usize, String)> = Vec::new();
    let snapshot = |stack: &[(usize, String)]| stack.iter().map(|(_, text)| text.clone()).collect();

    let mut offset = 0usize;
    for line in content.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        let line_end = offset;

        if let Some(active) = &fence {
            let unit = open
                .as_mut()
                .expect("an active fence always has an open unit");
            unit.end = line_end;
            if is_fence_close(line, active.marker, active.len) {
                units.extend(open.take());
                fence = None;
            }
            continue;
        }

        if line.trim().is_empty() {
            units.extend(open.take());
        } else if let Some((marker, len)) = fence_opener(line) {
            units.extend(open.take());
            open = Some(Unit {
                start: line_start,
                end: line_end,
                heading_path: snapshot(&heading_stack),
            });
            fence = Some(Fence { marker, len });
        } else if let Some((level, text)) = atx_heading(line) {
            units.extend(open.take());
            while heading_stack.last().is_some_and(|(l, _)| *l >= level) {
                heading_stack.pop();
            }
            heading_stack.push((level, text));
            units.push(Unit {
                start: line_start,
                end: line_end,
                heading_path: snapshot(&heading_stack),
            });
        } else if is_list_item(line) {
            units.extend(open.take());
            open = Some(Unit {
                start: line_start,
                end: line_end,
                heading_path: snapshot(&heading_stack),
            });
        } else {
            match &mut open {
                Some(unit) => unit.end = line_end,
                None => {
                    open = Some(Unit {
                        start: line_start,
                        end: line_end,
                        heading_path: snapshot(&heading_stack),
                    });
                }
            }
        }
    }
    // EOF closes whatever is open — including an unclosed fence.
    units.extend(open);
    units
}

/// The line's classifiable text: trailing `\n` / `\r\n` stripped.
fn classifiable(line: &str) -> &str {
    let text = line.strip_suffix('\n').unwrap_or(line);
    text.strip_suffix('\r').unwrap_or(text)
}

/// Strip up to 3 leading spaces; `None` when indented 4+ (such lines
/// never open fences or headings — CommonMark's code-indent threshold).
fn after_indent(text: &str) -> Option<&str> {
    let spaces = text.len() - text.trim_start_matches(' ').len();
    (spaces <= 3).then(|| &text[spaces..])
}

/// A fence-opening line: `(marker char, fence length)`.
fn fence_opener(line: &str) -> Option<(char, usize)> {
    let rest = after_indent(classifiable(line))?;
    let marker = rest.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }
    let len = rest.chars().take_while(|&c| c == marker).count();
    if len < 3 {
        return None;
    }
    // CommonMark: a backtick fence's info string cannot contain a
    // backtick (that line is inline code, not a fence).
    if marker == '`' && rest[len..].contains('`') {
        return None;
    }
    Some((marker, len))
}

/// A fence-closing line for a fence of `marker` × `min_len`: at least as
/// long a run of the same marker, nothing but whitespace after.
fn is_fence_close(line: &str, marker: char, min_len: usize) -> bool {
    let Some(rest) = after_indent(classifiable(line)) else {
        return false;
    };
    let len = rest.chars().take_while(|&c| c == marker).count();
    len >= min_len && rest[len..].trim().is_empty()
}

/// An ATX heading line: `(level, heading text)` with the trailing
/// closing-hash sequence stripped per CommonMark.
fn atx_heading(line: &str) -> Option<(usize, String)> {
    let rest = after_indent(classifiable(line))?;
    let level = rest.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let after = &rest[level..];
    if !(after.is_empty() || after.starts_with(' ') || after.starts_with('\t')) {
        return None;
    }
    let mut text = after.trim();
    let stripped = text.trim_end_matches('#');
    if stripped.len() != text.len() {
        if stripped.is_empty() {
            text = "";
        } else if stripped.ends_with(' ') || stripped.ends_with('\t') {
            text = stripped.trim_end();
        }
    }
    Some((level, text.to_string()))
}

/// A list-item line: optional indentation (any depth — nested items
/// split too), then a bullet (`-`/`+`/`*`) or ordered (`1-9 digits`
/// then `.`/`)`) marker followed by whitespace or end of line.
fn is_list_item(line: &str) -> bool {
    let rest = classifiable(line).trim_start_matches([' ', '\t']);
    if let Some(after) = rest.strip_prefix(['-', '+', '*']) {
        return after.is_empty() || after.starts_with(' ') || after.starts_with('\t');
    }
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if (1..=9).contains(&digits) {
        if let Some(after) = rest[digits..].strip_prefix(['.', ')']) {
            return after.is_empty() || after.starts_with(' ') || after.starts_with('\t');
        }
    }
    false
}

#[cfg(test)]
#[path = "chunker_tests.rs"]
mod tests;
