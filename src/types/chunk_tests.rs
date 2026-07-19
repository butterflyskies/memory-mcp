//! Contract tests for chunk identity, addressing, and wire shapes
//! (#262 slice 1). Test names cite the ADR-0042 property ledger (P1.x);
//! each comment names the contract regression that would turn it red.
//!
//! Deterministic example tests come first; property-based tests (the
//! `properties` module) hammer the same invariants over generated inputs.
//! Per the ledger, property tests provide collision-resistance *evidence*
//! for P1.2, never an injectivity proof. Shrunk counterexamples graduate
//! into named example regressions here.

use super::*;
use crate::types::{MemoryName, Scope, ScopePath};

fn span(start: usize, end: usize) -> SourceSpan {
    SourceSpan::new(start, end).expect("test span is valid")
}

fn v(version: u32) -> ChunkerVersion {
    ChunkerVersion::new(version)
}

const PARENT: &str = "550e8400-e29b-41d4-a716-446655440000";

// ---------------------------------------------------------------------------
// SourceSpan invariants (P1.5, P1.4)
// ---------------------------------------------------------------------------

/// P1.5 — red if construction accepts an empty or inverted span; a
/// zero-length chunk has no identity and an inverted span cannot resolve.
#[test]
fn source_span_rejects_empty_and_inverted() {
    assert!(SourceSpan::new(5, 5).is_err());
    assert!(SourceSpan::new(9, 3).is_err());
    assert!(SourceSpan::new(0, 1).is_ok());
}

/// P1.5 — red if deserialization bypasses the constructor invariant;
/// strict serde (ADR-0019) means invalid derived state fails loudly on
/// read, so no `SourceSpan` value can exist outside `start < end`.
#[test]
fn source_span_deserialize_enforces_invariant() {
    let ok: SourceSpan = serde_json::from_str(r#"{"start":2,"end":7}"#).expect("valid span");
    assert_eq!((ok.start(), ok.end()), (2, 7));

    assert!(serde_json::from_str::<SourceSpan>(r#"{"start":7,"end":7}"#).is_err());
    assert!(serde_json::from_str::<SourceSpan>(r#"{"start":7,"end":2}"#).is_err());
}

/// P1.4 — red if a span serialization round-trip loses or reorders
/// offsets.
#[test]
fn source_span_serde_round_trip() {
    let original = span(10, 42);
    let json = serde_json::to_string(&original).expect("serialize");
    let back: SourceSpan = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(original, back);
}

/// P1.5 — red if `slice_in` returns text for an out-of-bounds span
/// instead of an error; silently resolving a stale span against edited
/// content would serve wrong provenance.
#[test]
fn slice_in_rejects_out_of_bounds() {
    let content = "hello world";
    assert_eq!(span(0, 5).slice_in(content).expect("in bounds"), "hello");
    assert!(span(6, 20).slice_in(content).is_err());
}

/// P1.5 — red if `slice_in` slices through the middle of a multi-byte
/// UTF-8 sequence instead of erroring; the "validated UTF-8 source span"
/// contract from the #262 design.
#[test]
fn slice_in_rejects_non_char_boundary() {
    let content = "a\u{00e9}b"; // 'é' occupies bytes 1..3
    assert!(span(0, 2).slice_in(content).is_err());
    assert_eq!(span(0, 3).slice_in(content).expect("boundary"), "aé");
}

// ---------------------------------------------------------------------------
// FactId determinism and domain separation (P1.1, P1.2)
// ---------------------------------------------------------------------------

/// P1.1 — red if derivation stops being deterministic; rebuild
/// idempotency and stable index addressing both rest on this.
#[test]
fn fact_id_derivation_is_deterministic() {
    let a = FactId::derive(PARENT, v(1), span(0, 9), "body text").expect("derive");
    let b = FactId::derive(PARENT, v(1), span(0, 9), "body text").expect("derive");
    assert_eq!(a, b);
    assert_eq!(a.to_string(), b.to_string());
}

/// P1.2 — red if any identity component stops influencing the id.
/// Structural components (parent, version, span) are embedded verbatim,
/// so their distinctness is exact; body distinctness flows through the
/// truncated digest and is collision-resistance evidence, not proof.
/// The variant body keeps the base body's byte length — span length and
/// body length must agree (P1.8), so only same-length bodies can share
/// a span.
#[test]
fn fact_id_distinguishes_every_component() {
    let base = FactId::derive(PARENT, v(1), span(0, 4), "body").expect("derive");

    let other_parent = FactId::derive(
        "650e8400-e29b-41d4-a716-446655440000",
        v(1),
        span(0, 4),
        "body",
    )
    .expect("derive");
    let other_version = FactId::derive(PARENT, v(2), span(0, 4), "body").expect("derive");
    let other_span = FactId::derive(PARENT, v(1), span(5, 9), "body").expect("derive");
    let other_body = FactId::derive(PARENT, v(1), span(0, 4), "ydob").expect("derive");

    for other in [&other_parent, &other_version, &other_span, &other_body] {
        assert_ne!(&base, other);
        assert_ne!(base.digest(), other.digest());
    }
}

/// P1.2 — red if identical bodies at different positions collapse into
/// one id; a repeated paragraph must yield distinct facts (span is
/// embedded verbatim, so this holds even under digest collision).
#[test]
fn identical_bodies_at_different_spans_are_distinct_facts() {
    let first = FactId::derive(PARENT, v(1), span(0, 4), "same").expect("derive");
    let second = FactId::derive(PARENT, v(1), span(100, 104), "same").expect("derive");
    assert_ne!(first, second);
}

/// P1.3 — red if a parent id that cannot survive the colon-delimited
/// canonical form is accepted; such an id would not round-trip.
#[test]
fn fact_id_rejects_unrenderable_parent_ids() {
    assert!(FactId::derive("", v(1), span(0, 1), "x").is_err());
    assert!(FactId::derive("has:colon", v(1), span(0, 1), "x").is_err());
}

/// P1.8 — red if `derive` blesses a body whose byte length disagrees
/// with the span it claims to describe; an id over a (span, body) pair
/// that cannot both be true is false provenance.
#[test]
fn fact_id_derive_rejects_span_body_length_mismatch() {
    assert!(FactId::derive(PARENT, v(1), span(0, 10), "short").is_err());
    assert!(FactId::derive(PARENT, v(1), span(0, 3), "toolong").is_err());
    assert!(FactId::derive(PARENT, v(1), span(0, 5), "short").is_ok());
}

// ---------------------------------------------------------------------------
// FactId canonical form round-trips (P1.3, P1.4)
// ---------------------------------------------------------------------------

/// P1.3 — red if the rendered canonical form and the parser disagree;
/// every index key, telemetry row, and wire field uses this string.
#[test]
fn fact_id_display_from_str_round_trip() {
    let id = FactId::derive(PARENT, v(3), span(128, 143), "some chunk body").expect("derive");
    let rendered = id.to_string();
    assert!(rendered.starts_with(&format!("fact:v1:{PARENT}:3:128-143:")));

    let parsed: FactId = rendered.parse().expect("parse canonical form");
    assert_eq!(parsed, id);
    assert_eq!(parsed.parent_id(), PARENT);
    assert_eq!(parsed.chunker_version(), v(3));
    assert_eq!(parsed.span(), span(128, 143));
    assert_eq!(parsed.digest().len(), 16);
}

/// P1.4 — red if serde stops using the canonical string form, or accepts
/// a string the parser rejects; wire and internal addressing must agree.
#[test]
fn fact_id_serde_round_trip_as_string() {
    let id = FactId::derive(PARENT, v(1), span(0, 5), "chunk").expect("derive");
    let json = serde_json::to_string(&id).expect("serialize");
    assert_eq!(json, format!("\"{id}\""));

    let back: FactId = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, id);
}

/// P1.3 — red if the strict parser starts accepting malformed ids;
/// lenient parsing would let corrupt index keys masquerade as valid
/// identities (ADR-0019 posture).
#[test]
fn fact_id_parser_rejects_malformed_input() {
    let cases: &[&str] = &[
        "",
        "fact:v2:parent:1:0-4:0123456789abcdef", // unknown format version
        "chunk:v1:parent:1:0-4:0123456789abcdef", // wrong prefix
        "fact:v1:parent:1:0-4",                  // missing digest
        "fact:v1:parent:1:0-4:0123456789abcdef:x", // trailing field
        "fact:v1::1:0-4:0123456789abcdef",       // empty parent
        "fact:v1:parent:one:0-4:0123456789abcdef", // non-numeric version
        "fact:v1:parent:1:4:0123456789abcdef",   // span missing '-'
        "fact:v1:parent:1:9-4:0123456789abcdef", // inverted span
        "fact:v1:parent:1:4-4:0123456789abcdef", // empty span
        "fact:v1:parent:1:0-4:0123456789abcde",  // digest too short
        "fact:v1:parent:1:0-4:0123456789ABCDEF", // uppercase digest
        "fact:v1:parent:1:0-4:0123456789abcdeg", // non-hex digest
    ];
    for case in cases {
        assert!(
            case.parse::<FactId>().is_err(),
            "parser accepted malformed fact id: {case:?}"
        );
    }
}

/// P1.3 — red if the parser launders noncanonical numeric spellings
/// (leading zeros, signs) into normalized values instead of rejecting
/// them; acceptance must imply `render(parse(s)) == s`, byte for byte.
/// Rust's integer `FromStr` tolerates these spellings, so each one is a
/// distinct laundering path.
#[test]
fn fact_id_parser_rejects_noncanonical_spellings() {
    let cases: &[&str] = &[
        "fact:v1:parent:01:0-4:0123456789abcdef", // leading-zero version
        "fact:v1:parent:1:00-04:0123456789abcdef", // leading-zero span pair
        "fact:v1:parent:1:00-4:0123456789abcdef", // leading-zero span start
        "fact:v1:parent:1:0-04:0123456789abcdef", // leading-zero span end
        "fact:v1:parent:1:0-004:0123456789abcdef", // multi-zero padding
        "fact:v1:parent:+1:0-4:0123456789abcdef", // signed version
        "fact:v1:parent:1:+0-4:0123456789abcdef", // signed span start
        "fact:v1:parent:1:0-+4:0123456789abcdef", // signed span end
    ];
    for case in cases {
        assert!(
            case.parse::<FactId>().is_err(),
            "parser laundered noncanonical fact id spelling: {case:?}"
        );
    }
    // The canonical spelling of the same coordinates still parses.
    assert!("fact:v1:parent:1:0-4:0123456789abcdef"
        .parse::<FactId>()
        .is_ok());
}

// ---------------------------------------------------------------------------
// FactRecord / MatchedChunk wire shapes (P1.4, P1.6, P1.8)
// ---------------------------------------------------------------------------

fn parent_ref() -> MemoryRef {
    MemoryRef::new(
        "org/team".parse::<Scope>().expect("scope"),
        MemoryName::new("design-notes").expect("name"),
    )
}

const SAMPLE_BODY: &str = "chunk body\n"; // 11 bytes

fn sample_record() -> FactRecord {
    let id = FactId::derive(PARENT, v(1), span(0, SAMPLE_BODY.len()), SAMPLE_BODY).expect("derive");
    FactRecord::new(
        id,
        parent_ref(),
        vec!["Design".to_string(), "Retrieval".to_string()],
        SAMPLE_BODY.to_string(),
        vec!["retrieval".to_string()],
        vec!["adr-0042".to_string()],
    )
    .expect("consistent record")
}

fn sample_chunk() -> MatchedChunk {
    MatchedChunk::new(
        FactId::derive(PARENT, v(1), span(3, 6), "hit").expect("derive"),
        vec!["Ops".to_string()],
        "hit".to_string(),
    )
    .expect("consistent chunk")
}

/// P1.4 — red if a catalog record stops surviving a serialization
/// round-trip; the derived catalog (slice 3) persists and reloads exactly
/// this shape.
#[test]
fn fact_record_serde_round_trip() {
    let record = sample_record();
    let json = serde_json::to_string(&record).expect("serialize");
    let back: FactRecord = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, record);
}

/// P1.2 — red if record accessors diverge from the identity; parent id,
/// span, and chunker version are deliberately not duplicated as fields,
/// so the accessors are the only path and must reflect the id.
#[test]
fn fact_record_accessors_delegate_to_id() {
    let record = sample_record();
    assert_eq!(record.parent_id(), PARENT);
    assert_eq!(record.chunker_version(), v(1));
    assert_eq!(record.span(), span(0, 11));
    assert_eq!(record.body(), SAMPLE_BODY);
}

/// P1.8 — red if construction blesses an id paired with a body it was
/// not derived from; a record whose id and content disagree is false
/// provenance and must never exist as a value.
#[test]
fn fact_record_new_rejects_id_body_mismatch() {
    let id = FactId::derive(PARENT, v(1), span(0, 4), "abcd").expect("derive");

    // Same length, different content: digest mismatch.
    assert!(FactRecord::new(
        id.clone(),
        parent_ref(),
        vec![],
        "wxyz".to_string(),
        vec![],
        vec![],
    )
    .is_err());

    // Different length: span/body length gate.
    assert!(FactRecord::new(
        id.clone(),
        parent_ref(),
        vec![],
        "too long for the span".to_string(),
        vec![],
        vec![],
    )
    .is_err());

    // The consistent pair constructs.
    assert!(FactRecord::new(id, parent_ref(), vec![], "abcd".to_string(), vec![], vec![]).is_ok());
}

/// P1.8 mutation regression — red if a persisted record whose body bytes
/// were corrupted (bitrot, catalog bug) loads cleanly instead of failing
/// closed; deserialization must re-verify id/body consistency.
#[test]
fn fact_record_deserialize_rejects_corrupted_body() {
    let mut value = serde_json::to_value(sample_record()).expect("to_value");

    // Same-length corruption: caught by the digest re-derivation.
    value["body"] = serde_json::json!("chunk bodY\n");
    assert!(serde_json::from_value::<FactRecord>(value.clone()).is_err());

    // Length-changing corruption: caught by the span/body length gate.
    value["body"] = serde_json::json!("evil");
    assert!(serde_json::from_value::<FactRecord>(value).is_err());
}

/// P1.8 mutation regression — red if a persisted record whose *id* was
/// corrupted (digest bytes flipped, span rewritten) loads cleanly while
/// pointing at content it does not describe.
#[test]
fn fact_record_deserialize_rejects_corrupted_id() {
    let good = serde_json::to_value(sample_record()).expect("to_value");
    let id_str = good["id"].as_str().expect("id serializes as string");

    // Flip the last digest character (hex, so '0' <-> '1' is safe).
    let mut corrupted = id_str.to_string();
    let last = corrupted.pop().expect("non-empty id");
    corrupted.push(if last == '0' { '1' } else { '0' });
    let mut value = good.clone();
    value["id"] = serde_json::json!(corrupted);
    assert!(serde_json::from_value::<FactRecord>(value).is_err());

    // Rewrite the span so its length disagrees with the body.
    let mut value = good.clone();
    value["id"] = serde_json::json!(id_str.replace(":0-11:", ":0-12:"));
    assert!(serde_json::from_value::<FactRecord>(value).is_err());
}

/// P1.4 — red if a record with an invalid embedded name or scope
/// deserializes; MemoryRef's components validate on deserialize, and
/// catalog records must not smuggle invalid refs past those boundaries.
#[test]
fn fact_record_rejects_invalid_parent_ref() {
    let record = sample_record();
    let mut value = serde_json::to_value(&record).expect("to_value");
    value["parent"]["name"] = serde_json::json!("bad name with spaces");
    assert!(serde_json::from_value::<FactRecord>(value).is_err());
}

/// P1.4 — red if the additive matched-chunk shape (slice 7) stops
/// round-tripping.
#[test]
fn matched_chunk_serde_round_trip() {
    let chunk = sample_chunk();
    let json = serde_json::to_string(&chunk).expect("serialize");
    let back: MatchedChunk = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, chunk);
}

/// P1.8 — red if matched-chunk provenance can pair a fact id with text
/// the id was not derived from — the same false-provenance failure as
/// `FactRecord`, on the recall wire instead of the catalog.
#[test]
fn matched_chunk_new_rejects_id_text_mismatch() {
    let id = FactId::derive(PARENT, v(1), span(3, 6), "hit").expect("derive");
    assert!(MatchedChunk::new(id.clone(), vec![], "miss".to_string()).is_err());
    assert!(MatchedChunk::new(id.clone(), vec![], "hit!".to_string()).is_err());
    assert!(MatchedChunk::new(id, vec![], "hit".to_string()).is_ok());
}

/// P1.8 mutation regression — red if a serialized matched chunk with
/// corrupted text deserializes cleanly.
#[test]
fn matched_chunk_deserialize_rejects_corrupted_text() {
    let mut value = serde_json::to_value(sample_chunk()).expect("to_value");
    value["text"] = serde_json::json!("hix");
    assert!(serde_json::from_value::<MatchedChunk>(value).is_err());
}

/// P1.6 — red if deserialization starts rejecting unknown fields;
/// additive wire compatibility requires consumers of these shapes to
/// tolerate future additions.
#[test]
fn wire_shapes_tolerate_additive_fields() {
    let chunk = sample_chunk();
    let mut value = serde_json::to_value(&chunk).expect("to_value");
    value["future_field"] = serde_json::json!("ignored");
    let back: MatchedChunk = serde_json::from_value(value).expect("additive field tolerated");
    assert_eq!(back, chunk);

    let record = sample_record();
    let mut value = serde_json::to_value(&record).expect("to_value");
    value["future_field"] = serde_json::json!("ignored");
    let back: FactRecord = serde_json::from_value(value).expect("additive field tolerated");
    assert_eq!(back, record);
}

/// P1.4 — red if `ChunkerVersion` stops serializing as a bare number;
/// the transparent representation is part of the wire contract.
#[test]
fn chunker_version_serializes_transparently() {
    assert_eq!(serde_json::to_string(&v(7)).expect("serialize"), "7");
    let back: ChunkerVersion = serde_json::from_str("7").expect("deserialize");
    assert_eq!(back, v(7));
}

// ---------------------------------------------------------------------------
// Property-based tests (ADR-0042 ledger, generated-input coverage)
// ---------------------------------------------------------------------------

mod properties {
    use super::*;
    use proptest::prelude::*;

    /// Any non-empty string without `:` — the exact domain `derive`
    /// accepts for parent ids. `\PC` = printable (non-control) Unicode,
    /// so generated ids exercise multi-byte parents too.
    fn parent_id_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[^:\\p{Cc}]{1,32}")
            .expect("valid regex")
            .prop_filter("no colon", |s| !s.contains(':'))
    }

    /// Spans built as (start, len) so every generated value satisfies the
    /// `start < end` invariant by construction — shrinking stays inside
    /// the domain instead of bouncing off a filter. For standalone
    /// `SourceSpan` properties only; fact-id spans are always sized to a
    /// generated body (P1.8).
    fn span_strategy() -> impl Strategy<Value = SourceSpan> {
        (0usize..1_000_000, 1usize..10_000)
            .prop_map(|(start, len)| SourceSpan::new(start, start + len).expect("start < end"))
    }

    fn version_strategy() -> impl Strategy<Value = ChunkerVersion> {
        any::<u32>().prop_map(ChunkerVersion::new)
    }

    /// A consistent (id, body) pair: body first, span sized to the body,
    /// id derived from both. The generator never pairs an id with a body
    /// it was not derived from — pairing independently generated values
    /// and blessing them via round-trip would test a laundered invariant
    /// instead of the real one (P1.8).
    fn fact_with_body_strategy() -> impl Strategy<Value = (FactId, String)> {
        (
            parent_id_strategy(),
            version_strategy(),
            0usize..1_000_000,
            ".{1,64}",
        )
            .prop_map(|(parent, version, start, body)| {
                let span = SourceSpan::new(start, start + body.len())
                    .expect("non-empty body yields non-empty span");
                let id = FactId::derive(parent, version, span, &body).expect("consistent inputs");
                (id, body)
            })
    }

    fn fact_id_strategy() -> impl Strategy<Value = FactId> {
        fact_with_body_strategy().prop_map(|(id, _body)| id)
    }

    /// Id-shaped strings whose numeric fields may carry noncanonical
    /// spellings (leading zeros, a leading `+`) — the domain where a
    /// laundering parser would silently normalize instead of rejecting.
    fn id_shaped_string_strategy() -> impl Strategy<Value = String> {
        let num = || proptest::string::string_regex("(\\+)?0{0,2}[0-9]{1,4}").expect("valid regex");
        (parent_id_strategy(), num(), num(), num(), "[0-9a-f]{16}").prop_map(
            |(parent, version, start, end, digest)| {
                format!("fact:v1:{parent}:{version}:{start}-{end}:{digest}")
            },
        )
    }

    /// Valid memory names: alphanumeric/`-`/`_`/`.` components, no dot
    /// prefix, ≤ 3 components — mirrors `MemoryName::validate`.
    fn memory_name_strategy() -> impl Strategy<Value = MemoryName> {
        proptest::string::string_regex("[a-zA-Z0-9_-][a-zA-Z0-9_.-]{0,12}")
            .expect("valid regex")
            .prop_map(|s| MemoryName::new(s).expect("regex generates valid names"))
    }

    fn scope_strategy() -> impl Strategy<Value = Scope> {
        prop_oneof![
            Just(Scope::Root),
            proptest::string::string_regex("[a-z]{1,6}(/[a-z]{1,6}){0,2}")
                .expect("valid regex")
                .prop_map(|s| Scope::Path(ScopePath::new(s).expect("valid scope path"))),
        ]
    }

    fn record_strategy() -> impl Strategy<Value = FactRecord> {
        (
            fact_with_body_strategy(),
            scope_strategy(),
            memory_name_strategy(),
            proptest::collection::vec(".{0,16}", 0..4),
            proptest::collection::vec("[a-z]{1,8}", 0..3),
            proptest::collection::vec("[a-z-]{1,12}", 0..3),
        )
            .prop_map(|((id, body), scope, name, heading_path, tags, refs_out)| {
                FactRecord::new(
                    id,
                    MemoryRef::new(scope, name),
                    heading_path,
                    body,
                    tags,
                    refs_out,
                )
                .expect("generator derives id from body")
            })
    }

    proptest! {
        /// P1.1 — determinism over the whole generated input domain, not
        /// just handpicked examples.
        #[test]
        fn derive_is_deterministic(
            parent in parent_id_strategy(),
            version in version_strategy(),
            start in 0usize..1_000_000,
            body in ".{1,64}",
        ) {
            let span = SourceSpan::new(start, start + body.len()).expect("non-empty body");
            let a = FactId::derive(parent.clone(), version, span, &body).expect("derive");
            let b = FactId::derive(parent, version, span, &body).expect("derive");
            prop_assert_eq!(&a, &b);
            prop_assert_eq!(a.to_string(), b.to_string());
        }

        /// P1.2 — collision-resistance evidence (not proof): distinct
        /// bodies at identical structural coordinates yield distinct ids
        /// across generated inputs. Bodies share a byte length so they
        /// can legally share a span (P1.8).
        #[test]
        fn distinct_bodies_yield_distinct_ids(
            parent in parent_id_strategy(),
            version in version_strategy(),
            start in 0usize..1_000_000,
            (body_a, body_b) in (1usize..64).prop_flat_map(|n| (
                proptest::collection::vec(proptest::char::range('a', 'z'), n),
                proptest::collection::vec(proptest::char::range('a', 'z'), n),
            )),
        ) {
            let body_a: String = body_a.into_iter().collect();
            let body_b: String = body_b.into_iter().collect();
            prop_assume!(body_a != body_b);
            let span = SourceSpan::new(start, start + body_a.len()).expect("non-empty body");
            let a = FactId::derive(parent.clone(), version, span, &body_a).expect("derive");
            let b = FactId::derive(parent, version, span, &body_b).expect("derive");
            prop_assert_ne!(a, b);
        }

        /// P1.3 — canonical-form totality: parse ∘ render is the identity
        /// for every derivable id, including multi-byte parent ids.
        #[test]
        fn canonical_form_round_trips(id in fact_id_strategy()) {
            let parsed: FactId = id.to_string().parse().expect("canonical form parses");
            prop_assert_eq!(parsed, id);
        }

        /// P1.3 — the other direction of canonicality: for any accepted
        /// string, render ∘ parse is the identity on the *input bytes*.
        /// Generated strings include noncanonical numeric spellings; a
        /// parser that launders them (accepts `01`, renders `1`) fails
        /// this property.
        #[test]
        fn accepted_strings_render_byte_identically(s in id_shaped_string_strategy()) {
            if let Ok(id) = s.parse::<FactId>() {
                prop_assert_eq!(id.to_string(), s);
            }
        }

        /// P1.4 — serde round-trip totality for `FactId`.
        #[test]
        fn fact_id_serde_round_trips(id in fact_id_strategy()) {
            let json = serde_json::to_string(&id).expect("serialize");
            let back: FactId = serde_json::from_str(&json).expect("deserialize");
            prop_assert_eq!(back, id);
        }

        /// P1.4 — serde round-trip totality for `SourceSpan`.
        #[test]
        fn span_serde_round_trips(span in span_strategy()) {
            let json = serde_json::to_string(&span).expect("serialize");
            let back: SourceSpan = serde_json::from_str(&json).expect("deserialize");
            prop_assert_eq!(back, span);
        }

        /// P1.5 — deserialization admits exactly the valid domain: an
        /// arbitrary (start, end) pair deserializes iff `start < end`, so
        /// no invalid span value can be constructed through the wire.
        #[test]
        fn span_deserializes_iff_valid(start in any::<u32>(), end in any::<u32>()) {
            let json = format!(r#"{{"start":{start},"end":{end}}}"#);
            let result = serde_json::from_str::<SourceSpan>(&json);
            prop_assert_eq!(result.is_ok(), start < end);
        }

        /// P1.5 — `slice_in` is total (never panics) and honest: when it
        /// resolves, the slice has exactly the span's length and matches
        /// the parent content at those offsets.
        #[test]
        fn slice_in_is_total_and_honest(content in ".{0,64}", span in span_strategy()) {
            if let Ok(text) = span.slice_in(&content) {
                prop_assert_eq!(text.len(), span.len());
                prop_assert_eq!(text, &content[span.start()..span.end()]);
            }
        }

        /// P1.5 — the full-body span of any non-empty content always
        /// resolves to the content itself.
        #[test]
        fn whole_body_span_always_resolves(content in ".{1,64}") {
            let span = SourceSpan::new(0, content.len()).expect("non-empty content");
            prop_assert_eq!(span.slice_in(&content).expect("whole body resolves"), &content);
        }

        /// P1.7 — `ChunkerVersion` ordering coincides with numeric
        /// ordering of the revision.
        #[test]
        fn version_ordering_matches_numeric(a in any::<u32>(), b in any::<u32>()) {
            prop_assert_eq!(
                ChunkerVersion::new(a).cmp(&ChunkerVersion::new(b)),
                a.cmp(&b)
            );
        }

        /// P1.4 — serde round-trip totality for `ChunkerVersion`,
        /// including the bare-number transparent representation.
        #[test]
        fn version_serde_round_trips(version in version_strategy()) {
            let json = serde_json::to_string(&version).expect("serialize");
            prop_assert_eq!(&json, &version.get().to_string());
            let back: ChunkerVersion = serde_json::from_str(&json).expect("deserialize");
            prop_assert_eq!(back, version);
        }

        /// P1.4 — serde round-trip totality for `FactRecord` (and its
        /// embedded `MemoryRef`) over generated scopes, names, and bodies.
        #[test]
        fn fact_record_serde_round_trips(record in record_strategy()) {
            let json = serde_json::to_string(&record).expect("serialize");
            let back: FactRecord = serde_json::from_str(&json).expect("deserialize");
            prop_assert_eq!(back, record);
        }

        /// P1.8 — a record whose serialized body is replaced with any
        /// *different* body never deserializes; false provenance fails
        /// closed for every generated record, not just the handpicked
        /// mutation examples.
        #[test]
        fn fact_record_body_mutations_fail_closed(
            record in record_strategy(),
            other_body in ".{1,64}",
        ) {
            prop_assume!(other_body != record.body());
            let mut value = serde_json::to_value(&record).expect("to_value");
            value["body"] = serde_json::Value::String(other_body);
            prop_assert!(serde_json::from_value::<FactRecord>(value).is_err());
        }

        /// P1.4 — serde round-trip totality for `MatchedChunk`.
        #[test]
        fn matched_chunk_serde_round_trips(
            (fact_id, text) in fact_with_body_strategy(),
            heading_path in proptest::collection::vec(".{0,16}", 0..4),
        ) {
            let chunk = MatchedChunk::new(fact_id, heading_path, text)
                .expect("consistent chunk");
            let json = serde_json::to_string(&chunk).expect("serialize");
            let back: MatchedChunk = serde_json::from_str(&json).expect("deserialize");
            prop_assert_eq!(back, chunk);
        }
    }
}
