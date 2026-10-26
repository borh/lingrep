use crate::v6_support::RevisionTestExt;
use lingrep_core::token_match::TokenInfo;
use lingrep_web::coordinator::CoordinatorRegistry;
use lingrep_web::corpus_runtime::CorpusIdentity;
use lingrep_web::revision::{RevisionId, WorkbenchId};
use lingrep_web::snapshot::{
    CompletedSnapshot, MatchRef, PatternDescriptor, PatternKey, PatternMetadataValue, PatternOrigin,
};

use crate::v6_support::{default_slot, runtime_fixture};

#[tokio::test]
async fn pattern_key_uses_compiled_order_not_lexical_name_order() {
    let (_fixture, runtime) = runtime_fixture(
        "snapshot-catalog",
        vec![vec![TokenInfo::default().with_orth("食べる")]],
    );
    let slot = default_slot(runtime);
    let registry = CoordinatorRegistry::new(10);
    let workbench = WorkbenchId::new("catalog");
    registry
        .accept_next_revision(
            workbench.clone(),
            &slot,
            "zeta := [text=食べる]. alpha := [text=食べる].".into(),
        )
        .await;
    let snapshot = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(snapshot) = registry
                .snapshot(&workbench)
                .await
                .and_then(|view| view.state.displayed_snapshot().cloned())
            {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    assert_eq!(
        snapshot.pattern(PatternKey::new(0).unwrap()).unwrap().name,
        "zeta"
    );
    assert_eq!(
        snapshot.pattern(PatternKey::new(1).unwrap()).unwrap().name,
        "alpha"
    );
    assert_eq!(snapshot.patterns[0].exact_total, 1);
    assert_eq!(snapshot.patterns[1].exact_total, 1);
}

#[tokio::test]
async fn snapshot_preserves_typed_pattern_metadata() {
    let (_fixture, runtime) = runtime_fixture("metadata-catalog", Vec::new());
    let slot = default_slot(runtime);
    let registry = CoordinatorRegistry::new(10);
    let workbench = WorkbenchId::new("metadata");
    registry
        .accept_next_revision(
            workbench.clone(),
            &slot,
            "category: \"modal\"\nreviewed: true\nfirst := [upos=NOUN].\ncategory: \"modal\"\nreviewed: true\nsecond := [upos=VERB].".into(),
        )
        .await;
    let snapshot = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(snapshot) = registry
                .snapshot(&workbench)
                .await
                .and_then(|view| view.state.displayed_snapshot().cloned())
            {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    assert_eq!(snapshot.patterns[0].name, "first");
    assert_eq!(
        snapshot.patterns[0].metadata.as_ref(),
        &[
            (
                "category".into(),
                PatternMetadataValue::String("modal".into())
            ),
            ("reviewed".into(), PatternMetadataValue::Bool(true)),
        ]
    );
}

#[test]
fn reference_ordinal_is_scoped_to_its_pattern() {
    let first = PatternKey::new(0).unwrap();
    let second = PatternKey::new(1).unwrap();
    let snapshot = CompletedSnapshot {
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
        corpus_identity: CorpusIdentity::new("fixture"),
        source: std::sync::Arc::from(""),
        derive_compounds: false,
        compound_category: lingrep_web::snapshot::CompoundCategory::Compound,
        patterns: vec![
            PatternDescriptor {
                key: first,
                name: "first".into(),
                exact_total: 1,
                metadata: Box::default(),
                origin: PatternOrigin::Authored { chained_total: 0 },
            },
            PatternDescriptor {
                key: second,
                name: "second".into(),
                exact_total: 1,
                metadata: Box::default(),
                origin: PatternOrigin::Authored { chained_total: 0 },
            },
        ]
        .into_boxed_slice(),
        match_refs: vec![
            MatchRef {
                sentence_index: 0,
                span: 0..1,
                pattern_key: first,
                captures: Box::default(),
                graph_bindings: Box::default(),
                lexical: None,
                in_chain: false,
            },
            MatchRef {
                sentence_index: 0,
                span: 1..2,
                pattern_key: second,
                captures: Box::default(),
                graph_bindings: Box::default(),
                lexical: None,
                in_chain: false,
            },
        ]
        .into_boxed_slice(),
        capture_catalog: Box::default(),
        oversized_omissions: None,
    };

    assert_eq!(snapshot.match_ref(second, 0).unwrap().pattern_key, second);
}
