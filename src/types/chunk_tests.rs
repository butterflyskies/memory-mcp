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
    let a = FactId::derive(PARENT, v(1), span(0, 10), "body text").expect("derive");
    let b = FactId::derive(PARENT, v(1), span(0, 10), "body text").expect("derive");
    assert_eq!(a, b);
    assert_eq!(a.to_string(), b.to_string());
}

/// P1.2 — red if any identity component stops influencing the id.
/// Structural components (parent, version, span) are embedded verbatim,
/// so their distinctness is exact; body distinctness flows through the
/// truncated digest and is collision-resistance evidence, not proof.
#[test]
fn fact_id_distinguishes_every_component() {
    let base = FactId::derive(PARENT, v(1), span(0, 10), "body").expect("derive");

    let other_parent = FactId::derive(
        "650e8400-e29b-41d4-a716-446655440000",
        v(1),
        span(0, 10),
        "body",
    )
    .expect("derive");
    let other_version = FactId::derive(PARENT, v(2), span(0, 10), "body").expect("derive");
    let other_span = FactId::derive(PARENT, v(1), span(5, 15), "body").expect("derive");
    let other_body = FactId::derive(PARENT, v(1), span(0, 10), "different").expect("derive");

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

// ---------------------------------------------------------------------------
// FactId canonical form round-trips (P1.3, P1.4)
// ---------------------------------------------------------------------------

/// P1.3 — red if the rendered canonical form and the parser disagree;
/// every index key, telemetry row, and wire field uses this string.
#[test]
fn fact_id_display_from_str_round_trip() {
    let id = FactId::derive(PARENT, v(3), span(128, 512), "some chunk body").expect("derive");
    let rendered = id.to_string();
    assert!(rendered.starts_with(&format!("fact:v1:{PARENT}:3:128-512:")));

    let parsed: FactId = rendered.parse().expect("parse canonical form");
    assert_eq!(parsed, id);
    assert_eq!(parsed.parent_id(), PARENT);
    assert_eq!(parsed.chunker_version(), v(3));
    assert_eq!(parsed.span(), span(128, 512));
    assert_eq!(parsed.digest().len(), 16);
}

/// P1.4 — red if serde stops using the canonical string form, or accepts
/// a string the parser rejects; wire and internal addressing must agree.
#[test]
fn fact_id_serde_round_trip_as_string() {
    let id = FactId::derive(PARENT, v(1), span(0, 7), "chunk").expect("derive");
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

// ---------------------------------------------------------------------------
// FactRecord / MatchedChunk wire shapes (P1.4, P1.6)
// ---------------------------------------------------------------------------

fn sample_record() -> FactRecord {
    let id = FactId::derive(PARENT, v(1), span(0, 12), "chunk body\n").expect("derive");
    FactRecord {
        id,
        parent: MemoryRef::new(
            "org/team".parse::<Scope>().expect("scope"),
            MemoryName::new("design-notes").expect("name"),
        ),
        heading_path: vec!["Design".to_string(), "Retrieval".to_string()],
        body: "chunk body\n".to_string(),
        tags: vec!["retrieval".to_string()],
        refs_out: vec!["adr-0042".to_string()],
    }
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
    assert_eq!(record.span(), span(0, 12));
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
    let chunk = MatchedChunk {
        fact_id: FactId::derive(PARENT, v(1), span(3, 9), "hit").expect("derive"),
        heading_path: vec!["Ops".to_string()],
        text: "hit".to_string(),
    };
    let json = serde_json::to_string(&chunk).expect("serialize");
    let back: MatchedChunk = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, chunk);
}

/// P1.6 — red if deserialization starts rejecting unknown fields;
/// additive wire compatibility requires consumers of these shapes to
/// tolerate future additions.
#[test]
fn wire_shapes_tolerate_additive_fields() {
    let chunk = MatchedChunk {
        fact_id: FactId::derive(PARENT, v(1), span(3, 9), "hit").expect("derive"),
        heading_path: vec![],
        text: "hit".to_string(),
    };
    let mut value = serde_json::to_value(&chunk).expect("to_value");
    value["future_field"] = serde_json::json!("ignored");
    let back: MatchedChunk = serde_json::from_value(value).expect("additive field tolerated");
    assert_eq!(back, chunk);
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
    /// the domain instead of bouncing off a filter.
    fn span_strategy() -> impl Strategy<Value = SourceSpan> {
        (0usize..1_000_000, 1usize..10_000)
            .prop_map(|(start, len)| SourceSpan::new(start, start + len).expect("start < end"))
    }

    fn version_strategy() -> impl Strategy<Value = ChunkerVersion> {
        any::<u32>().prop_map(ChunkerVersion::new)
    }

    fn fact_id_strategy() -> impl Strategy<Value = FactId> {
        (
            parent_id_strategy(),
            version_strategy(),
            span_strategy(),
            ".{0,64}",
        )
            .prop_map(|(parent, version, span, body)| {
                FactId::derive(parent, version, span, &body).expect("valid parent id")
            })
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
            fact_id_strategy(),
            scope_strategy(),
            memory_name_strategy(),
            proptest::collection::vec(".{0,16}", 0..4),
            ".{0,64}",
            proptest::collection::vec("[a-z]{1,8}", 0..3),
            proptest::collection::vec("[a-z-]{1,12}", 0..3),
        )
            .prop_map(
                |(id, scope, name, heading_path, body, tags, refs_out)| FactRecord {
                    id,
                    parent: MemoryRef::new(scope, name),
                    heading_path,
                    body,
                    tags,
                    refs_out,
                },
            )
    }

    proptest! {
        /// P1.1 — determinism over the whole generated input domain, not
        /// just handpicked examples.
        #[test]
        fn derive_is_deterministic(
            parent in parent_id_strategy(),
            version in version_strategy(),
            span in span_strategy(),
            body in ".{0,64}",
        ) {
            let a = FactId::derive(parent.clone(), version, span, &body).expect("derive");
            let b = FactId::derive(parent, version, span, &body).expect("derive");
            prop_assert_eq!(&a, &b);
            prop_assert_eq!(a.to_string(), b.to_string());
        }

        /// P1.2 — collision-resistance evidence (not proof): distinct
        /// bodies at identical structural coordinates yield distinct ids
        /// across generated inputs.
        #[test]
        fn distinct_bodies_yield_distinct_ids(
            parent in parent_id_strategy(),
            version in version_strategy(),
            span in span_strategy(),
            body_a in ".{0,64}",
            body_b in ".{0,64}",
        ) {
            prop_assume!(body_a != body_b);
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

        /// P1.4 — serde round-trip totality for `MatchedChunk`.
        #[test]
        fn matched_chunk_serde_round_trips(
            fact_id in fact_id_strategy(),
            heading_path in proptest::collection::vec(".{0,16}", 0..4),
            text in ".{0,64}",
        ) {
            let chunk = MatchedChunk { fact_id, heading_path, text };
            let json = serde_json::to_string(&chunk).expect("serialize");
            let back: MatchedChunk = serde_json::from_str(&json).expect("deserialize");
            prop_assert_eq!(back, chunk);
        }
    }
}
