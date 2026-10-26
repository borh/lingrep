// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(
    clippy::string_slice,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable,
    clippy::exit,
    clippy::unchecked_time_subtraction,
    clippy::unwrap_used
)]
use std::sync::Arc;

use lingrep_core::token_match::TokenInfo;
use lingrep_web::corpus_runtime::{CorpusIdentity, CorpusReader};
use lingrep_web::inspection::{InspectionError, SnapshotInspector};
use lingrep_web::revision::RevisionId;
use lingrep_web::snapshot::{
    CaptureDescriptor, CaptureKey, CaptureSpanRef, CompletedSnapshot, MatchRef, PatternDescriptor,
    PatternKey, PatternOrigin,
};

use crate::v6_support::ready_fixture;

fn fixture(
    snapshot_identity: &str,
    cache_identity: &str,
) -> (tempfile::TempDir, Arc<CompletedSnapshot>, CorpusReader) {
    let (fixture, ready) = ready_fixture(
        cache_identity,
        vec![vec![
            TokenInfo::default().with_orth("を"),
            TokenInfo::default()
                .with_orth("食べる")
                .with_lemma("食べる")
                .with_pron_base("タベル"),
        ]],
    );
    let reader = ready.reader().unwrap();
    let key = PatternKey::new(0).unwrap();
    let snapshot = Arc::new(CompletedSnapshot {
        search_scope: Default::default(),
        excluded_sentences: 0,
        sections: lingrep_web::section_tally::SectionAvailability::Available(
            lingrep_web::section_tally::SectionTally::unlabelled(),
        ),
        observed_sentences: 0,
        matched_sentences: 0,
        documents: None,
        sentences_matched: 0,
        revision: RevisionId(1),
        corpus_identity: CorpusIdentity::new(snapshot_identity),
        source: std::sync::Arc::from(""),
        derive_compounds: false,
        compound_category: lingrep_web::snapshot::CompoundCategory::Compound,
        patterns: vec![PatternDescriptor {
            key,
            name: "eating".into(),
            exact_total: 1,
            metadata: Box::default(),
            origin: PatternOrigin::Authored { chained_total: 0 },
        }]
        .into_boxed_slice(),
        match_refs: vec![MatchRef {
            sentence_index: 0,
            span: 1..2,
            pattern_key: key,
            captures: Box::default(),
            graph_bindings: Box::default(),
            lexical: None,
            in_chain: false,
        }]
        .into_boxed_slice(),
        capture_catalog: Box::default(),
        oversized_omissions: None,
    });
    (fixture.directory, snapshot, reader)
}

fn fixture_two_refs(
    snapshot_identity: &str,
    cache_identity: &str,
) -> (tempfile::TempDir, Arc<CompletedSnapshot>, CorpusReader) {
    let (directory, mut snapshot, reader) = fixture(snapshot_identity, cache_identity);
    let snapshot_mut = Arc::make_mut(&mut snapshot);
    let key = snapshot_mut.match_refs[0].pattern_key;
    snapshot_mut.match_refs = vec![
        MatchRef {
            sentence_index: 0,
            span: 0..1,
            pattern_key: key,
            captures: Box::default(),
            graph_bindings: Box::default(),
            lexical: None,
            in_chain: false,
        },
        MatchRef {
            sentence_index: 0,
            span: 1..2,
            pattern_key: key,
            captures: Box::default(),
            graph_bindings: Box::default(),
            lexical: None,
            in_chain: false,
        },
    ]
    .into_boxed_slice();
    (directory, snapshot, reader)
}

#[test]
fn two_refs_in_one_sentence_project_from_a_single_decode() {
    // fixture_two_refs: same two-token sentence, but match_refs = [span 0..1, span 1..2]
    let (_dir, snapshot, reader) = fixture_two_refs("c", "c");
    let inspector = SnapshotInspector::bind(Arc::clone(&snapshot), reader).unwrap();
    let index = snapshot.match_refs[0].sentence_index;
    let tokens = inspector
        .reader_clone()
        .decode_sentences(&[index])
        .unwrap()
        .pop()
        .unwrap()
        .unwrap();

    let views: Vec<_> = snapshot
        .match_refs
        .iter()
        .map(|r| inspector.project_reference(&tokens, r, 3).unwrap())
        .collect();
    assert_eq!(
        views.len(),
        2,
        "both refs project from one decoded sentence"
    );
}

fn fixture_with_capture(
    descriptor_pattern: PatternKey,
    reference_pattern: PatternKey,
    match_span: std::ops::Range<usize>,
    capture_span: std::ops::Range<usize>,
) -> (tempfile::TempDir, Arc<CompletedSnapshot>, CorpusReader) {
    let (directory, mut snapshot, reader) = fixture("current", "current");
    let snapshot_mut = Arc::make_mut(&mut snapshot);
    snapshot_mut.match_refs[0].pattern_key = reference_pattern;
    snapshot_mut.match_refs[0].span = match_span;
    let capture_key = CaptureKey::new(0).unwrap();
    snapshot_mut.match_refs[0].captures = vec![CaptureSpanRef {
        capture_key,
        span: capture_span,
    }]
    .into_boxed_slice();
    snapshot_mut.capture_catalog = vec![CaptureDescriptor {
        key: capture_key,
        pattern_key: descriptor_pattern,
        name: "命題".into(),
        path: "outer/FRAGMENT#1/命題".into(),
    }]
    .into_boxed_slice();
    (directory, snapshot, reader)
}

#[test]
fn project_reference_uses_borrowed_ref_without_ordinal_lookup() {
    let (_dir, snapshot, reader) = fixture("eating-corpus", "eating-corpus");
    let inspector = SnapshotInspector::bind(Arc::clone(&snapshot), reader).unwrap();

    // Decode the sentence the ordinary way, then project the borrowed ref.
    let tokens = decode_first_sentence(&snapshot, &inspector); // helper below
    let reference = &snapshot.match_refs[0];

    let view = inspector.project_reference(&tokens, reference, 3).unwrap();
    assert!(
        view.tokens
            .iter()
            .any(|t| t.role == lingrep_web::inspection::TokenRole::Match),
        "the match token is present in the projection"
    );
    assert_eq!(
        view.visible_range.start,
        reference.span.start.saturating_sub(3)
    );
}

// Decode sentence 0 via the pure batch API (no dictionary needed).
fn decode_first_sentence(
    snapshot: &CompletedSnapshot,
    inspector: &SnapshotInspector,
) -> Vec<lingrep_core::token_match::TokenInfo> {
    let index = snapshot.match_refs[0].sentence_index;
    inspector
        .reader_clone()
        .decode_sentences(&[index])
        .unwrap()
        .pop()
        .unwrap()
        .unwrap()
}

#[test]
fn stale_corpus_fails_before_decode() {
    let (_directory, snapshot, reader) = fixture("snapshot", "cache");
    let counter = reader.clone();
    assert!(matches!(
        SnapshotInspector::bind(snapshot, reader),
        Err(InspectionError::StaleCorpus)
    ));
    assert_eq!(counter.decoded_block_count(), 0);
}

#[tokio::test]
async fn unknown_token_cannot_escape_visible_context() {
    let (_directory, snapshot, reader) = fixture("current", "current");
    let inspector = SnapshotInspector::bind(snapshot, reader).unwrap();
    assert_eq!(
        inspector.token(PatternKey::new(0).unwrap(), 0, 99, 3).await,
        Err(InspectionError::UnknownToken),
    );
}

#[tokio::test]
async fn valid_token_preserves_all_available_fields() {
    let (_directory, snapshot, reader) = fixture("current", "current");
    let inspector = SnapshotInspector::bind(snapshot, reader).unwrap();
    let token = inspector
        .token(PatternKey::new(0).unwrap(), 0, 1, 3)
        .await
        .unwrap();
    assert_eq!(token.summary.orth.as_deref(), Some("食べる"));
    assert_eq!(
        token.field("pronbase").unwrap().value.as_deref(),
        Some("タベル")
    );
}

#[tokio::test]
async fn cross_pattern_capture_key_is_rejected_without_partial_output() {
    let key = PatternKey::new(0).unwrap();
    let other = PatternKey::new(1).unwrap();
    let (_directory, snapshot, reader) = fixture_with_capture(other, key, 1..2, 1..2);
    let inspector = SnapshotInspector::bind(snapshot, reader).unwrap();
    assert_eq!(
        inspector.match_view(key, 0, 3).await,
        Err(InspectionError::InvalidSpan)
    );
    assert_eq!(
        inspector.token(key, 0, 1, 3).await,
        Err(InspectionError::InvalidSpan)
    );
}

#[tokio::test]
async fn capture_outside_top_level_match_is_rejected() {
    let key = PatternKey::new(0).unwrap();
    let (_directory, snapshot, reader) = fixture_with_capture(key, key, 1..2, 0..2);
    let inspector = SnapshotInspector::bind(snapshot, reader).unwrap();
    assert_eq!(
        inspector.match_view(key, 0, 3).await,
        Err(InspectionError::InvalidSpan)
    );
    assert_eq!(
        inspector.token(key, 0, 1, 3).await,
        Err(InspectionError::InvalidSpan)
    );
}

#[tokio::test]
async fn valid_capture_preserves_name_path_span_and_text() {
    let key = PatternKey::new(0).unwrap();
    let (_directory, snapshot, reader) = fixture_with_capture(key, key, 1..2, 1..2);
    let view = SnapshotInspector::bind(snapshot, reader)
        .unwrap()
        .match_view(key, 0, 3)
        .await
        .unwrap();
    assert_eq!(view.captures[0].name, "命題");
    assert_eq!(view.captures[0].path, "outer/FRAGMENT#1/命題");
    assert_eq!(view.captures[0].span, 1..2);
    assert_eq!(view.captures[0].text, "食べる");
}

#[tokio::test]
async fn token_rejects_missing_and_inverted_capture_references() {
    let key = PatternKey::new(0).unwrap();
    let (_directory, mut missing_snapshot, missing_reader) =
        fixture_with_capture(key, key, 1..2, 1..2);
    Arc::make_mut(&mut missing_snapshot).match_refs[0].captures[0].capture_key =
        CaptureKey::new(9).unwrap();
    let missing = SnapshotInspector::bind(missing_snapshot, missing_reader).unwrap();
    assert_eq!(
        missing.token(key, 0, 1, 3).await,
        Err(InspectionError::InvalidSpan)
    );

    let inverted_start = 2;
    let inverted_end = 1;
    let (_directory, inverted_snapshot, inverted_reader) =
        fixture_with_capture(key, key, 1..2, inverted_start..inverted_end);
    let inverted = SnapshotInspector::bind(inverted_snapshot, inverted_reader).unwrap();
    assert_eq!(
        inverted.token(key, 0, 1, 3).await,
        Err(InspectionError::InvalidSpan)
    );
}
