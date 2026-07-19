//! Chunker tests (#262 slice 2). Test names cite the ADR-0042 property
//! ledger (P2.x); each comment names the regression that would turn it
//! red, and each property's doc comment states its non-vacuity argument
//! (which stub implementation fails it), per the standard set in #324.
//!
//! Budget decisions are tested against the *actual* embedding tokenizer
//! (BGE-small-en-v1.5, loaded from the HuggingFace cache exactly like
//! `tests/candle_embedding.rs` loads the full model) — never a byte or
//! word heuristic. Only the fingerprint-identity test uses a stub
//! counter, because it asserts naming, not counting. Properties run
//! across multiple budgets (16 / 512 / 8192) to prove the chunker is
//! genuinely budget-parametric — 512 is today's BGE truncation limit,
//! 8192 is the ModernBERT-class context anticipated to replace it.

use std::sync::OnceLock;

use tokenizers::Tokenizer;

use super::*;
use crate::embedding::MODEL_ID;
use crate::types::{MemoryName, Scope};

// ---------------------------------------------------------------------------
// Real-tokenizer harness
// ---------------------------------------------------------------------------

/// The real BGE tokenizer, from the HuggingFace cache (downloading
/// `tokenizer.json` on a cold cache, same posture as the embedding
/// engine's own model loading).
fn bge_tokenizer() -> &'static Tokenizer {
    static TOKENIZER: OnceLock<Tokenizer> = OnceLock::new();
    TOKENIZER.get_or_init(|| {
        use hf_hub::{api::sync::ApiBuilder, Cache, Repo, RepoType};
        let repo = Repo::new(MODEL_ID.to_string(), RepoType::Model);
        let path = match Cache::from_env().repo(repo.clone()).get("tokenizer.json") {
            Some(path) => path,
            None => ApiBuilder::from_env()
                .with_progress(false)
                .build()
                .expect("build HuggingFace Hub api")
                .repo(repo)
                .get("tokenizer.json")
                .expect("download tokenizer.json"),
        };
        Tokenizer::from_file(path).expect("parse tokenizer.json")
    })
}

fn config(max_tokens: usize) -> ChunkerConfig {
    let counter = HfTokenCounter::new(bge_tokenizer().clone(), MODEL_ID).expect("wrap tokenizer");
    ChunkerConfig::new(Box::new(counter), max_tokens).expect("non-zero budget")
}

const PARENT: &str = "550e8400-e29b-41d4-a716-446655440000";

fn parent_ref() -> MemoryRef {
    MemoryRef::new(
        "notes".parse::<Scope>().expect("scope"),
        MemoryName::new("chunky").expect("name"),
    )
}

fn chunks_of(content: &str, cfg: &ChunkerConfig) -> Vec<Chunk> {
    chunk_markdown(content, cfg).expect("chunking succeeds")
}

/// Concatenate chunk bodies in span order.
fn reconstruct(content: &str, chunks: &[Chunk]) -> String {
    chunks
        .iter()
        .map(|c| c.span.slice_in(content).expect("span resolves"))
        .collect()
}

// ---------------------------------------------------------------------------
// Structural parsing examples (tokenizer-free)
// ---------------------------------------------------------------------------

/// Boundary policy — red if headings, paragraphs, list items, or fences
/// stop starting their own units; coalescing can only merge boundaries
/// that the parser actually produced.
#[test]
fn parse_units_splits_on_structural_boundaries() {
    let doc = "# Title\n\nfirst paragraph\nstill first\n\n- item one\n- item two\n\n```rust\nlet x = 1;\n```\n";
    let units = parse_units(doc);
    let spans: Vec<(usize, usize)> = units.iter().map(|u| (u.start, u.end)).collect();
    let find = |needle: &str| doc.find(needle).expect("needle present");
    assert_eq!(
        spans,
        vec![
            (0, find("first") - 1),                    // "# Title\n"
            (find("first"), find("- item one") - 1),   // paragraph, both lines
            (find("- item one"), find("- item two")),  // list item 1
            (find("- item two"), find("```rust") - 1), // list item 2
            (find("```rust"), doc.len()),              // whole fence, atomic
        ]
    );
    assert!(units
        .iter()
        .all(|u| u.heading_path == vec!["Title".to_string()]));
}

/// Boundary policy — red if the heading stack stops popping same-or-
/// deeper levels; heading paths would drift and mislabel every chunk
/// below the second heading.
#[test]
fn parse_units_tracks_heading_stack() {
    let doc = "# A\n\n## B\n\ntext b\n\n### C\n\ntext c\n\n## D\n\ntext d\n";
    let units = parse_units(doc);
    let path_of = |needle: &str| {
        let start = doc.find(needle).expect("needle");
        units
            .iter()
            .find(|u| u.start == start)
            .expect("unit at needle")
            .heading_path
            .clone()
    };
    assert_eq!(path_of("text b"), vec!["A", "B"]);
    assert_eq!(path_of("text c"), vec!["A", "B", "C"]);
    // "## D" pops both "### C" and "## B".
    assert_eq!(path_of("text d"), vec!["A", "D"]);
    // A heading's own unit includes itself in its trail.
    assert_eq!(path_of("### C"), vec!["A", "B", "C"]);
}

/// Boundary policy — red if an unclosed fence stops running to end of
/// input; splitting it as prose would cut inside code the author never
/// closed.
#[test]
fn parse_units_unclosed_fence_runs_to_eof() {
    let doc = "para\n\n```\ncode\n\nmore code\n";
    let units = parse_units(doc);
    assert_eq!(units.len(), 2);
    assert_eq!(units[1].start, doc.find("```").expect("fence"));
    assert_eq!(units[1].end, doc.len());
}

/// Boundary policy — red if fence detection drifts from the documented
/// CommonMark subset: a backtick "fence" whose info string contains a
/// backtick is inline code, and a 4-space-indented fence is plain text.
#[test]
fn parse_units_rejects_non_fences() {
    // Inline-code line: must NOT open a fence swallowing the rest.
    let doc = "```not a fence`\nplain\n\nnext\n";
    let units = parse_units(doc);
    assert_eq!(units.len(), 2, "backtick info string must not open a fence");

    let doc = "    ```\ncode?\n";
    let units = parse_units(doc);
    assert_eq!(units.len(), 1, "4-space-indented fence is plain text");
    // ...while a tilde fence takes an info string with backticks.
    let doc = "~~~info`with`ticks\nx\n~~~\n\nafter\n";
    assert_eq!(parse_units(doc).len(), 2);
}

/// Boundary policy — red if ATX detection loosens (no-space `#hash` is
/// a tag, not a heading) or stops stripping closing hashes / CR line
/// endings.
#[test]
fn parse_units_atx_heading_edge_cases() {
    assert_eq!(atx_heading("## Title ##\n"), Some((2, "Title".to_string())));
    assert_eq!(atx_heading("# Title\r\n"), Some((1, "Title".to_string())));
    assert_eq!(atx_heading("###\n"), Some((3, String::new())));
    assert_eq!(atx_heading("#hashtag\n"), None);
    assert_eq!(atx_heading("####### seven\n"), None);
    assert_eq!(atx_heading("    # indented\n"), None);
    // Closing hashes not preceded by whitespace belong to the text.
    assert_eq!(atx_heading("# C#\n"), Some((1, "C#".to_string())));
}

/// Boundary policy — red if list-marker detection drifts: markers need
/// trailing whitespace (or end of line), ordered markers cap at 9
/// digits, and emphasis (`*word*`) is not a bullet.
#[test]
fn parse_units_list_marker_edge_cases() {
    assert!(is_list_item("- item\n"));
    assert!(is_list_item("+ item\n"));
    assert!(is_list_item("* item\n"));
    assert!(is_list_item("  - nested\n"));
    assert!(is_list_item("3. ordered\n"));
    assert!(is_list_item("42) ordered\n"));
    assert!(is_list_item("-\n")); // empty item
    assert!(!is_list_item("*emphasis*\n"));
    assert!(!is_list_item("-dash-word\n"));
    assert!(!is_list_item("1234567890. too many digits\n"));
    assert!(!is_list_item("plain text\n"));
}

// ---------------------------------------------------------------------------
// Chunking examples (real tokenizer)
// ---------------------------------------------------------------------------

/// P2.5 edge — red if empty content produces a chunk; a `SourceSpan`
/// cannot be empty, so there is nothing to address.
#[test]
fn empty_content_yields_no_chunks() {
    assert!(chunks_of("", &config(512)).is_empty());
}

/// Whitespace policy — red if a whitespace-only body loses bytes or
/// splits; it is one chunk covering everything (coverage is exact,
/// with no "modulo whitespace" caveat).
#[test]
fn whitespace_only_content_is_one_chunk() {
    let content = "\n\n   \n\t\n";
    let chunks = chunks_of(content, &config(512));
    assert_eq!(chunks.len(), 1);
    assert_eq!(
        (chunks[0].span.start(), chunks[0].span.end()),
        (0, content.len())
    );
    assert!(chunks[0].heading_path.is_empty());
}

/// P2.5 — red if a short memory stops degenerating to exactly one
/// whole-body chunk; short memories must keep their current single-unit
/// retrieval behavior.
#[test]
fn short_memory_is_exactly_one_chunk() {
    let content = "# Notes\n\nA short memory, well under any real budget.\n\n- one\n- two\n";
    let cfg = config(512);
    let chunks = chunks_of(content, &cfg);
    assert_eq!(chunks.len(), 1);
    assert_eq!(
        (chunks[0].span.start(), chunks[0].span.end()),
        (0, content.len())
    );
    // The chunk starts with the heading, so its trail includes it.
    assert_eq!(chunks[0].heading_path, vec!["Notes"]);
}

/// Two-section fixture split by a budget that admits either section but
/// not both. The budget is *measured*, not guessed, so the test tracks
/// the real tokenizer instead of hardcoding token counts.
fn two_sections() -> (String, usize, ChunkerConfig) {
    let alpha = "## Alpha\n\nalpha section body with several words of prose in it.\n";
    let beta = "## Beta\n\nbeta section body with several more words of prose too.\n";
    let content = format!("{alpha}\n{beta}");
    let beta_start = content.find("## Beta").expect("beta heading");
    let beta_para = content.find("beta section").expect("beta paragraph");

    let cfg = config(512);
    let first = cfg.count(&content[..beta_start]).expect("count");
    let rest = cfg.count(&content[beta_start..]).expect("count");
    let budget = first.max(rest);
    // Preconditions that make the expected split forced: each section
    // fits alone, but the first section plus the next heading does not.
    let overflow = cfg.count(&content[..beta_para]).expect("count");
    assert!(
        overflow > budget,
        "fixture drift: first section + next heading ({overflow} tokens) must exceed \
         the budget ({budget})"
    );
    (content, beta_start, config(budget))
}

/// P2.6 + boundary policy — red if coalescing stops preferring
/// structural boundaries: with a budget admitting either section but
/// not both, the split must land exactly at the second heading, and
/// each chunk carries its own section's heading trail.
#[test]
fn sections_split_at_structural_boundary() {
    let (content, beta_start, cfg) = two_sections();
    let chunks = chunks_of(&content, &cfg);
    assert_eq!(chunks.len(), 2);
    assert_eq!(
        (chunks[0].span.start(), chunks[0].span.end()),
        (0, beta_start)
    );
    assert_eq!(
        (chunks[1].span.start(), chunks[1].span.end()),
        (beta_start, content.len())
    );
    assert_eq!(chunks[0].heading_path, vec!["Alpha"]);
    assert_eq!(chunks[1].heading_path, vec!["Beta"]);
}

/// P2.6 — red if an oversized code fence splits mid-line: the
/// deterministic split is line-first, so every chunk boundary inside
/// the fence lands at a line start, every piece fits the budget, and
/// coverage stays exact.
#[test]
fn oversized_code_fence_splits_on_line_boundaries() {
    let mut content = String::from("```rust\n");
    for i in 0..24 {
        content.push_str(&format!("let value_{i} = compute({i}) + offset;\n"));
    }
    content.push_str("```\n");
    let cfg = config(32);
    assert!(
        cfg.count(&content).expect("count") > cfg.max_tokens(),
        "fixture drift: fence must exceed the budget"
    );

    let chunks = chunks_of(&content, &cfg);
    assert!(chunks.len() > 1, "oversized fence must split");
    for chunk in &chunks {
        let start = chunk.span.start();
        assert!(
            start == 0 || content.as_bytes()[start - 1] == b'\n',
            "chunk boundary at byte {start} is not a line start"
        );
        let body = chunk.span.slice_in(&content).expect("span resolves");
        assert!(cfg.count(body).expect("count") <= cfg.max_tokens());
    }
    assert_eq!(reconstruct(&content, &chunks), content);
}

/// P2.2 + P2.6 — red if splitting a single over-budget line lands off a
/// UTF-8 character boundary; the line is multi-byte throughout (2–4
/// byte characters), so a byte-windowed splitter fails loudly here.
#[test]
fn oversized_single_line_splits_on_char_boundaries() {
    let content = "é雪🦋".repeat(40) + "\n";
    let cfg = config(16);
    assert!(
        cfg.count(&content).expect("count") > cfg.max_tokens(),
        "fixture drift: line must exceed the budget"
    );

    let chunks = chunks_of(&content, &cfg);
    assert!(chunks.len() > 1, "oversized line must split");
    for chunk in &chunks {
        // slice_in errors off char boundaries — resolving is the test.
        chunk
            .span
            .slice_in(&content)
            .expect("span on char boundary");
    }
    assert_eq!(reconstruct(&content, &chunks), content);
}

/// P2.6 documented fallback — red if a budget below the special-token
/// overhead stops producing deterministic single-character chunks (the
/// only over-budget shape permitted) and instead loops, panics, or
/// silently drops content.
#[test]
fn single_char_over_budget_falls_back_deterministically() {
    let cfg = config(2);
    assert!(
        cfg.count("a").expect("count") > cfg.max_tokens(),
        "fixture drift: a single character must exceed a 2-token budget \
         once special tokens are counted"
    );
    let content = "ab";
    let chunks = chunks_of(content, &cfg);
    assert_eq!(chunks.len(), 2);
    assert_eq!(reconstruct(content, &chunks), content);
    for chunk in &chunks {
        assert_eq!(
            chunk
                .span
                .slice_in(content)
                .expect("resolves")
                .chars()
                .count(),
            1
        );
    }
}

/// True provenance (P1.8 carried into slice 2) — red if the chunker
/// emits records whose ids do not describe their bodies, drops the
/// chunker version stamp, forgets parent tags, or populates `refs_out`
/// before slice 8 exists.
#[test]
fn chunk_into_facts_builds_true_provenance_records() {
    let (content, _, cfg) = two_sections();
    let tags = vec!["retrieval".to_string()];
    let records = chunk_into_facts(PARENT, &parent_ref(), &content, &tags, &cfg).expect("facts");
    assert_eq!(records.len(), 2);
    let mut rebuilt = String::new();
    for record in &records {
        assert_eq!(record.parent_id(), PARENT);
        assert_eq!(record.chunker_version(), CHUNKER_VERSION);
        assert_eq!(record.parent(), &parent_ref());
        assert_eq!(record.tags(), tags.as_slice());
        assert!(record.refs_out().is_empty());
        assert_eq!(
            record.span().slice_in(&content).expect("resolves"),
            record.body()
        );
        rebuilt.push_str(record.body());
    }
    assert_eq!(rebuilt, content);
}

/// P2.7 example — red if editing only a suffix (appending a section)
/// disturbs the chunks — spans, bodies, and `FactId`s — of the
/// unchanged prefix. The final chunk may legitimately re-form (it
/// absorbs trailing whitespace and may coalesce with new content);
/// everything before it must not.
#[test]
fn appending_a_section_preserves_prefix_chunks() {
    let (content, _, cfg) = two_sections();
    let extended = format!("{content}\n## Gamma\n\ngamma words appended later.\n");

    let base = chunks_of(&content, &cfg);
    let grown = chunks_of(&extended, &cfg);
    let stable = base.len() - 1;
    assert_eq!(&grown[..stable], &base[..stable]);

    let base_ids: Vec<FactId> = chunk_into_facts(PARENT, &parent_ref(), &content, &[], &cfg)
        .expect("facts")
        .iter()
        .map(|r| r.id().clone())
        .collect();
    let grown_ids: Vec<FactId> = chunk_into_facts(PARENT, &parent_ref(), &extended, &[], &cfg)
        .expect("facts")
        .iter()
        .map(|r| r.id().clone())
        .collect();
    assert_eq!(&grown_ids[..stable], &base_ids[..stable]);
}

// ---------------------------------------------------------------------------
// Config identity
// ---------------------------------------------------------------------------

/// Stub counter for fingerprint-identity assertions only — every
/// budget-behavior test above uses the real tokenizer.
struct StubCounter(&'static str);

impl TokenCounter for StubCounter {
    fn identity(&self) -> &str {
        self.0
    }
    fn count(&self, text: &str) -> Result<usize, MemoryError> {
        Ok(text.chars().count())
    }
}

/// Rebuild detectability (ADR-0042 stale-model row) — red if the
/// fingerprint stops varying with any of algorithm revision, tokenizer
/// identity, or budget: an embedding-model swap (BGE → ModernBERT) or a
/// budget change (512 → 8192) must be a *visible* staleness signal for
/// the slice-3 catalog, never a silent mix of chunk vintages.
#[test]
fn fingerprint_incorporates_version_tokenizer_and_budget() {
    let bge_512 = config(512).fingerprint();
    let bge_8192 = config(8192).fingerprint();
    let modern_8192 = ChunkerConfig::new(Box::new(StubCounter("modernbert-test")), 8192)
        .expect("config")
        .fingerprint();

    assert!(bge_512.contains(MODEL_ID));
    assert!(bge_512.contains("512"));
    assert!(bge_512.contains(&format!("v{CHUNKER_VERSION}")));
    assert_ne!(bge_512, bge_8192, "budget must change the fingerprint");
    assert_ne!(
        bge_8192, modern_8192,
        "tokenizer must change the fingerprint"
    );
}

/// Rebuild detectability, artifact edition — red if the fingerprint
/// trusts the human-readable label instead of the tokenizer's actual
/// content: two tokenizer artifacts that count differently but carry
/// the same model id (e.g. an upstream re-release of `tokenizer.json`
/// under a mutable HF revision) must never share a fingerprint, or the
/// slice-3 stamp would retain stale chunk vintages — the exact failure
/// the fingerprint exists to prevent. Also red if the digest stops
/// being deterministic for identical tokenizer content.
#[test]
fn fingerprint_binds_tokenizer_content_not_just_label() {
    let a = HfTokenCounter::new(bge_tokenizer().clone(), MODEL_ID).expect("wrap");
    let b = HfTokenCounter::new(bge_tokenizer().clone(), MODEL_ID).expect("wrap");
    assert_eq!(
        a.identity(),
        b.identity(),
        "identical tokenizer content must digest identically"
    );
    assert!(
        a.identity().starts_with(&format!("{MODEL_ID}@sha256:")),
        "identity must carry the label and a content digest, got {:?}",
        a.identity()
    );

    // A behaviorally different artifact under the *same label*: one
    // extra token in the vocabulary changes both counting and bytes.
    let mut altered = bge_tokenizer().clone();
    altered
        .add_tokens([tokenizers::AddedToken::from("[[chunk-vintage]]", false)])
        .expect("add token");
    let altered = HfTokenCounter::new(altered, MODEL_ID).expect("wrap");
    assert_ne!(
        a.identity(),
        altered.identity(),
        "same label, different tokenizer bytes must not share an identity"
    );

    let fp_a = ChunkerConfig::new(Box::new(a), 512)
        .expect("config")
        .fingerprint();
    let fp_altered = ChunkerConfig::new(Box::new(altered), 512)
        .expect("config")
        .fingerprint();
    assert_ne!(
        fp_a, fp_altered,
        "same label, different tokenizer bytes must not share a fingerprint"
    );
}

/// Config validation — red if a zero budget is accepted; it admits
/// nothing (not even special tokens) and would degenerate every body
/// into per-character fallback chunks by construction.
#[test]
fn zero_budget_is_rejected() {
    let counter = Box::new(StubCounter("stub"));
    assert!(ChunkerConfig::new(counter, 0).is_err());
}

// ---------------------------------------------------------------------------
// Operation budget (rebuild-DoS regression)
// ---------------------------------------------------------------------------

/// Real BGE counter that also counts how many times it is invoked, so
/// tests can bound the chunker's tokenization work, not just its
/// output.
struct CountingCounter {
    inner: HfTokenCounter,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl TokenCounter for CountingCounter {
    fn identity(&self) -> &str {
        self.inner.identity()
    }
    fn count(&self, text: &str) -> Result<usize, MemoryError> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.count(text)
    }
}

fn counting_config(
    max_tokens: usize,
) -> (
    ChunkerConfig,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = CountingCounter {
        inner: HfTokenCounter::new(bge_tokenizer().clone(), MODEL_ID).expect("wrap tokenizer"),
        calls: std::sync::Arc::clone(&calls),
    };
    (
        ChunkerConfig::new(Box::new(counter), max_tokens).expect("non-zero budget"),
        calls,
    )
}

/// Finding-2 regression (Syne, PR #326 review) — red if splitting
/// regresses to a full tokenization per candidate over a growing
/// substring: a valid maximum-size one-line memory then costs on the
/// order of one `Tokenizer::encode` per character (~10⁵–10⁶ calls),
/// turning catalog rebuild/startup into a stored-input DoS once slice
/// 3 wires the chunker in. The bounded doubling+bisection probe needs
/// O(log piece) calls per emitted chunk; the asserted ceilings leave
/// generous headroom over the measured counts while sitting orders of
/// magnitude below the per-character cost.
#[test]
fn pathological_inputs_stay_within_operation_budget() {
    // One 100_000-character line (~50k tokens): exercises char_split.
    // The per-character scan needed >100_000 encodes here.
    let content = "a ".repeat(50_000);
    let (cfg, calls) = counting_config(512);
    let chunks = chunks_of(&content, &cfg);
    assert!(chunks.len() > 1, "pathological line must split");
    assert_eq!(reconstruct(&content, &chunks), content);
    let single_line_calls = calls.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        single_line_calls <= 64 * chunks.len() + 64,
        "single-line split made {single_line_calls} tokenizer calls for {} chunks",
        chunks.len()
    );

    // 50_000 two-character lines in one paragraph (blank-line-free):
    // exercises whole-line packing. The per-line growing-substring
    // scan needed one encode per line (50_000+).
    let content = "b\n".repeat(50_000);
    let (cfg, calls) = counting_config(512);
    let chunks = chunks_of(&content, &cfg);
    assert!(chunks.len() > 1, "pathological paragraph must split");
    assert_eq!(reconstruct(&content, &chunks), content);
    let multi_line_calls = calls.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        multi_line_calls <= 64 * chunks.len() + 64,
        "line packing made {multi_line_calls} tokenizer calls for {} chunks",
        chunks.len()
    );

    // 25_000 tiny structural units (list items): exercises unit
    // coalescing, which previously re-encoded the growing group once
    // per unit.
    let content = "- c\n".repeat(25_000);
    let (cfg, calls) = counting_config(512);
    let chunks = chunks_of(&content, &cfg);
    assert!(chunks.len() > 1, "pathological list must split");
    assert_eq!(reconstruct(&content, &chunks), content);
    let unit_calls = calls.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        unit_calls <= 64 * chunks.len() + 64,
        "coalescing made {unit_calls} tokenizer calls for {} chunks",
        chunks.len()
    );
}

// ---------------------------------------------------------------------------
// Property-based tests (ADR-0042 P2.1–P2.7)
// ---------------------------------------------------------------------------

mod properties {
    use super::*;
    use proptest::prelude::*;

    /// Budgets each property runs under, proving budget-parametricity:
    /// 16 forces coalescing/splitting decisions on small generated
    /// docs, 512 is the real BGE truncation limit, 8192 is the
    /// ModernBERT-class context anticipated to replace it.
    const PBT_BUDGETS: [usize; 3] = [16, 512, 8192];

    fn budget_strategy() -> impl Strategy<Value = usize> {
        proptest::sample::select(&PBT_BUDGETS[..])
    }

    /// One shared config per budget — cloning the tokenizer per case
    /// would dominate property runtime.
    fn shared_config(max_tokens: usize) -> &'static ChunkerConfig {
        static CONFIGS: OnceLock<Vec<(usize, ChunkerConfig)>> = OnceLock::new();
        CONFIGS
            .get_or_init(|| PBT_BUDGETS.iter().map(|&b| (b, config(b))).collect())
            .iter()
            .find(|(b, _)| *b == max_tokens)
            .map(|(_, cfg)| cfg)
            .expect("budget from PBT_BUDGETS")
    }

    /// Inline characters mixing 1-byte (ASCII), 2-byte (é/ß), 3-byte
    /// (→/雪), and 4-byte (🦋) UTF-8, plus a combining mark. `safe`
    /// excludes everything that could open a fence or read as a
    /// structural marker — required only by the prefix-stability
    /// property, whose append-only contract an unclosed trailing fence
    /// legitimately breaks (appending *inside* an open fence edits the
    /// last block, not the suffix).
    fn text_char(safe: bool) -> BoxedStrategy<char> {
        let base = prop_oneof![
            8 => proptest::char::range('a', 'z'),
            2 => proptest::char::range('0', '9'),
            2 => Just(' '),
            1 => Just('é'),
            1 => Just('ß'),
            1 => Just('→'),
            1 => Just('雪'),
            1 => Just('🦋'),
            1 => Just('\u{0301}'),
        ];
        if safe {
            base.boxed()
        } else {
            prop_oneof![
                6 => base,
                1 => Just('`'),
                1 => Just('~'),
                1 => Just('#'),
                1 => Just('-'),
                1 => Just('.'),
            ]
            .boxed()
        }
    }

    fn inline_text(safe: bool) -> impl Strategy<Value = String> {
        proptest::collection::vec(text_char(safe), 0..12).prop_map(String::from_iter)
    }

    fn heading_block(safe: bool) -> impl Strategy<Value = String> {
        (1usize..=6, inline_text(safe))
            .prop_map(|(level, text)| format!("{} {text}\n", "#".repeat(level)))
    }

    fn paragraph_block(safe: bool) -> impl Strategy<Value = String> {
        proptest::collection::vec(inline_text(safe), 1..3)
            .prop_map(|lines| lines.iter().map(|l| format!("{l}\n")).collect())
    }

    fn list_block(safe: bool) -> impl Strategy<Value = String> {
        (
            proptest::collection::vec(inline_text(safe), 1..4),
            any::<bool>(),
        )
            .prop_map(|(items, ordered)| {
                items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| {
                        if ordered {
                            format!("{}. {item}\n", i + 1)
                        } else {
                            format!("- {item}\n")
                        }
                    })
                    .collect()
            })
    }

    /// Fenced code block; when `safe`, always closed (and the safe
    /// charset keeps fence markers out of info/lines, so it stays
    /// closed under parsing).
    fn fence_block(safe: bool) -> impl Strategy<Value = String> {
        (
            inline_text(safe),
            proptest::collection::vec(inline_text(safe), 0..4),
            any::<bool>(),
        )
            .prop_map(move |(info, lines, closed)| {
                let mut block = format!("```{info}\n");
                for line in &lines {
                    block.push_str(line);
                    block.push('\n');
                }
                if safe || closed {
                    block.push_str("```\n");
                }
                block
            })
    }

    fn block(safe: bool) -> impl Strategy<Value = String> {
        prop_oneof![
            2 => heading_block(safe),
            3 => paragraph_block(safe),
            2 => list_block(safe),
            2 => fence_block(safe),
        ]
    }

    /// A markdown document: blocks separated by blank lines, with
    /// optional leading blank lines.
    fn structured_doc(safe: bool) -> impl Strategy<Value = String> {
        (0usize..3, proptest::collection::vec(block(safe), 0..7))
            .prop_map(|(leading, blocks)| format!("{}{}", "\n".repeat(leading), blocks.join("\n")))
    }

    /// Completely arbitrary text — newlines, tabs, markers, and
    /// multi-byte characters anywhere. The chunker's invariants hold
    /// for *any* input, not just well-formed markdown.
    fn raw_doc() -> impl Strategy<Value = String> {
        let ch = prop_oneof![
            10 => proptest::char::range(' ', '~'),
            2 => Just('\n'),
            1 => Just('\t'),
            1 => Just('é'),
            1 => Just('雪'),
            1 => Just('🦋'),
            1 => Just('\u{0301}'),
        ];
        proptest::collection::vec(ch, 0..300).prop_map(String::from_iter)
    }

    fn any_doc() -> impl Strategy<Value = String> {
        prop_oneof![
            3 => structured_doc(false),
            2 => raw_doc(),
        ]
    }

    fn fact_ids(content: &str, cfg: &ChunkerConfig) -> Vec<FactId> {
        chunk_into_facts(PARENT, &parent_ref(), content, &[], cfg)
            .expect("facts")
            .iter()
            .map(|r| r.id().clone())
            .collect()
    }

    proptest! {
        /// P2.1 — same content + same config ⇒ byte-identical chunk
        /// sequence (spans, heading paths) and identical `FactId`s.
        /// Non-vacuity: fails under any chunker with hidden state or
        /// unordered iteration (e.g. hash-map-ordered emission or a
        /// random tie-break in coalescing).
        #[test]
        fn chunking_is_deterministic(content in any_doc(), budget in budget_strategy()) {
            let cfg = shared_config(budget);
            prop_assert_eq!(chunks_of(&content, cfg), chunks_of(&content, cfg));
            prop_assert_eq!(fact_ids(&content, cfg), fact_ids(&content, cfg));
        }

        /// P2.2 — every emitted span is in bounds and on UTF-8
        /// character boundaries (`slice_in` succeeds). Non-vacuity: a
        /// fixed-byte-window chunker (every N bytes) fails on the
        /// generated 2–4-byte characters.
        #[test]
        fn every_span_resolves_within_the_source(
            content in any_doc(),
            budget in budget_strategy(),
        ) {
            let cfg = shared_config(budget);
            for chunk in chunks_of(&content, cfg) {
                prop_assert!(
                    chunk.span.slice_in(&content).is_ok(),
                    "span {} does not resolve in {} bytes",
                    chunk.span,
                    content.len()
                );
            }
        }

        /// P2.3 — spans are strictly ordered by start and pairwise
        /// non-overlapping. Non-vacuity: any overlapping-window chunker
        /// (the sliding-window RAG default ADR-0042 rejected) fails.
        #[test]
        fn spans_are_ordered_and_non_overlapping(
            content in any_doc(),
            budget in budget_strategy(),
        ) {
            let cfg = shared_config(budget);
            let chunks = chunks_of(&content, cfg);
            for pair in chunks.windows(2) {
                prop_assert!(pair[0].span.start() < pair[1].span.start());
                prop_assert!(pair[0].span.end() <= pair[1].span.start());
            }
        }

        /// P2.4 — zero loss, zero duplication: chunks tile the source
        /// exactly (first starts at 0, each starts where the previous
        /// ended, last ends at EOF) and concatenated bodies reproduce
        /// the source byte for byte. The whitespace policy makes this
        /// exact — no "modulo whitespace" escape hatch. Non-vacuity: a
        /// chunker that trims boundary whitespace, drops empty-looking
        /// regions, or overlaps windows fails; so does one that emits
        /// anything for empty input.
        #[test]
        fn chunks_reconstruct_the_source_exactly(
            content in any_doc(),
            budget in budget_strategy(),
        ) {
            let cfg = shared_config(budget);
            let chunks = chunks_of(&content, cfg);
            if content.is_empty() {
                prop_assert!(chunks.is_empty());
                return Ok(());
            }
            prop_assert!(!chunks.is_empty());
            prop_assert_eq!(chunks.first().expect("non-empty").span.start(), 0);
            prop_assert_eq!(chunks.last().expect("non-empty").span.end(), content.len());
            for pair in chunks.windows(2) {
                prop_assert_eq!(pair[0].span.end(), pair[1].span.start());
            }
            prop_assert_eq!(reconstruct(&content, &chunks), content);
        }

        /// P2.5 — content whose total token count fits the budget is
        /// exactly one chunk spanning the whole body. Non-vacuity: a
        /// chunker that always splits per structural block (no
        /// coalescing) fails for any short multi-block document.
        #[test]
        fn short_input_degenerates_to_one_chunk(
            content in any_doc(),
            budget in budget_strategy(),
        ) {
            let cfg = shared_config(budget);
            prop_assume!(!content.is_empty());
            prop_assume!(cfg.count(&content).expect("count") <= cfg.max_tokens());
            let chunks = chunks_of(&content, cfg);
            prop_assert_eq!(chunks.len(), 1);
            prop_assert_eq!(chunks[0].span.start(), 0);
            prop_assert_eq!(chunks[0].span.end(), content.len());
        }

        /// P2.6 — every chunk fits the tokenizer budget, except the
        /// documented indivisible fallback: a single character that
        /// alone exceeds it (impossible at real budgets; reachable at
        /// budget 16 only by pathological inputs, and covered
        /// deterministically by the budget-2 example test).
        /// Non-vacuity: a purely structural chunker with no budget
        /// enforcement fails as soon as a generated block exceeds 16
        /// tokens; a byte-length heuristic drifts from the real
        /// tokenizer on multi-byte and word-merging inputs and fails
        /// under one of the three budgets.
        #[test]
        fn chunks_fit_the_budget_or_are_indivisible(
            content in any_doc(),
            budget in budget_strategy(),
        ) {
            let cfg = shared_config(budget);
            for chunk in chunks_of(&content, cfg) {
                let body = chunk.span.slice_in(&content).expect("span resolves");
                let tokens = cfg.count(body).expect("count");
                prop_assert!(
                    tokens <= cfg.max_tokens() || body.chars().count() == 1,
                    "chunk of {} tokens exceeds budget {} and is not the \
                     single-character fallback: {:?}",
                    tokens,
                    cfg.max_tokens(),
                    body
                );
            }
        }

        /// P2.7 — appending new blocks after a complete document leaves
        /// every chunk but the last — spans, bodies, and `FactId`s —
        /// intact. The last chunk may re-form: it owns the trailing
        /// whitespace and may coalesce with appended content. The safe
        /// generator keeps fences closed because appending inside an
        /// unclosed fence edits the *last block*, which is not a
        /// suffix-only edit. Non-vacuity: fails under any chunker whose
        /// grouping uses global lookahead (balanced/DP packing spreads
        /// a suffix change into earlier group boundaries) and under any
        /// id scheme that hashes whole-document position context into
        /// every chunk.
        #[test]
        fn appended_suffix_preserves_prefix_chunks(
            base in structured_doc(true),
            suffix_blocks in proptest::collection::vec(block(true), 1..4),
            budget in budget_strategy(),
        ) {
            let cfg = shared_config(budget);
            let base_chunks = chunks_of(&base, cfg);
            prop_assume!(!base_chunks.is_empty());
            let stable = base_chunks.len() - 1;

            let grown = format!("{base}\n{}", suffix_blocks.join("\n"));
            let grown_chunks = chunks_of(&grown, cfg);
            prop_assert!(grown_chunks.len() >= stable);
            prop_assert_eq!(&grown_chunks[..stable], &base_chunks[..stable]);

            let base_ids = fact_ids(&base, cfg);
            let grown_ids = fact_ids(&grown, cfg);
            prop_assert_eq!(&grown_ids[..stable], &base_ids[..stable]);
        }

        /// Fact assembly — every record carries the chunker version
        /// stamp and the parent id, and bodies reconstruct the source
        /// (the record-level view of P2.4). Non-vacuity: fails if
        /// `chunk_into_facts` derives ids under a different version
        /// than [`CHUNKER_VERSION`], reorders records, or re-slices
        /// bodies from anywhere but the span.
        #[test]
        fn fact_records_stamp_identity_and_reconstruct(
            content in any_doc(),
            budget in budget_strategy(),
        ) {
            let cfg = shared_config(budget);
            let records = chunk_into_facts(PARENT, &parent_ref(), &content, &[], cfg)
                .expect("facts");
            let mut rebuilt = String::new();
            for record in &records {
                prop_assert_eq!(record.parent_id(), PARENT);
                prop_assert_eq!(record.chunker_version(), CHUNKER_VERSION);
                rebuilt.push_str(record.body());
            }
            prop_assert_eq!(rebuilt, content);
        }
    }

    // -----------------------------------------------------------------------
    // Budget-scaled properties (finding 4, Syne's PR #326 review)
    // -----------------------------------------------------------------------
    //
    // `any_doc` tops out around 300 characters, so at the 512 and 8192
    // budgets the properties above exercise almost entirely the
    // one-chunk path — an implementation that secretly clamped to 512
    // would survive them. The generators here scale document size
    // *around each budget* (below, at, and beyond it), so the
    // multi-chunk paths — coalescing boundaries, oversized-block
    // splitting — run under the real large budgets too. Case counts
    // are small because each case tokenizes tens of kilobytes; the
    // sizing knob (`quarters`) shrinks toward the budget threshold on
    // failure.

    /// Words the scaled filler draws from: prose-ish ASCII plus
    /// multi-byte characters, so scaling is not ASCII-only.
    fn filler_word() -> impl Strategy<Value = &'static str> {
        proptest::sample::select(
            &[
                "alpha",
                "budget",
                "chunk",
                "memory",
                "retrieval",
                "span",
                "tokens",
                "détente",
                "雪山",
                "🦋",
            ][..],
        )
    }

    /// A document sized relative to `budget`: `quarters` blocks of
    /// filler prose totalling roughly `budget × quarters / 4` tokens
    /// (the filler vocabulary measures ~5–6 chars/token, and the char
    /// target uses 6, so high `quarters` values reliably overshoot the
    /// budget). Paragraph blocks close every ~2 000 characters so
    /// large documents contain many structural units, exercising
    /// coalescing rather than a single oversized block.
    fn scaled_doc(
        budget: usize,
        quarters: std::ops::RangeInclusive<usize>,
    ) -> impl Strategy<Value = String> {
        (
            quarters,
            proptest::collection::vec(filler_word(), 6..12),
            1usize..=6,
        )
            .prop_map(move |(quarters, words, level)| {
                let sentence = words.join(" ");
                let target_chars = budget * 6 * quarters / 4;
                let mut doc = format!("{} scaled fixture\n\n", "#".repeat(level));
                let mut para_chars = 0usize;
                while doc.len() < target_chars {
                    doc.push_str(&sentence);
                    doc.push('\n');
                    para_chars += sentence.len() + 1;
                    if para_chars > 2_000 {
                        doc.push('\n');
                        para_chars = 0;
                    }
                }
                doc
            })
    }

    /// The large budgets paired with a document scaled to them.
    /// `quarters` picks the size range in budget-quarters: `1..=10`
    /// spans below/at/beyond the budget; `6..=10` lands beyond it
    /// (used where a case under the budget would just be discarded).
    fn scaled_budget_and_doc(
        quarters: std::ops::RangeInclusive<usize>,
    ) -> impl Strategy<Value = (usize, String)> {
        proptest::sample::select(&[512usize, 8192][..]).prop_flat_map(move |budget| {
            scaled_doc(budget, quarters.clone()).prop_map(move |doc| (budget, doc))
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 8, ..ProptestConfig::default() })]

        /// P2.4/P2.5/P2.6 at scale — red if the chunker misbehaves on
        /// documents that genuinely cross the 512/8192 budgets: a
        /// document measuring over the budget must span multiple
        /// chunks (an implementation secretly clamping to 512 tokens,
        /// or one whose 8192 path was never exercised, fails here),
        /// every chunk must fit the budget, and tiling/reconstruction
        /// must stay exact at tens-of-kilobytes scale.
        #[test]
        fn scaled_docs_cross_their_budgets((budget, content) in scaled_budget_and_doc(1..=10)) {
            let cfg = shared_config(budget);
            let chunks = chunks_of(&content, cfg);
            let total = cfg.count(&content).expect("count");
            if total > cfg.max_tokens() {
                prop_assert!(
                    chunks.len() >= 2,
                    "{total} tokens at budget {} must span multiple chunks",
                    cfg.max_tokens()
                );
            } else {
                prop_assert_eq!(chunks.len(), 1);
            }
            prop_assert_eq!(chunks.first().map(|c| c.span.start()), Some(0));
            prop_assert_eq!(chunks.last().map(|c| c.span.end()), Some(content.len()));
            for pair in chunks.windows(2) {
                prop_assert_eq!(pair[0].span.end(), pair[1].span.start());
            }
            for chunk in &chunks {
                let body = chunk.span.slice_in(&content).expect("span resolves");
                prop_assert!(cfg.count(body).expect("count") <= cfg.max_tokens());
            }
            prop_assert_eq!(reconstruct(&content, &chunks), content);
        }

        /// P2.7 at scale — red if appending closed blocks after a
        /// *multi-chunk* document disturbs any chunk but the last;
        /// the small-document property above rarely has more than one
        /// chunk at 512/8192, so this is the append-stability check
        /// that actually exercises interior chunk boundaries at the
        /// real budgets.
        #[test]
        fn scaled_docs_preserve_prefix_chunks_on_append(
            (budget, base) in scaled_budget_and_doc(6..=10),
            suffix in paragraph_block(true),
        ) {
            let cfg = shared_config(budget);
            let base_chunks = chunks_of(&base, cfg);
            prop_assume!(base_chunks.len() >= 2);
            let stable = base_chunks.len() - 1;

            let grown = format!("{base}\n{suffix}");
            let grown_chunks = chunks_of(&grown, cfg);
            prop_assert!(grown_chunks.len() >= stable);
            prop_assert_eq!(&grown_chunks[..stable], &base_chunks[..stable]);

            let base_ids = fact_ids(&base, cfg);
            let grown_ids = fact_ids(&grown, cfg);
            prop_assert_eq!(&grown_ids[..stable], &base_ids[..stable]);
        }
    }
}
