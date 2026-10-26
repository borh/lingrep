// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

use lingrep_core::{
    execution::{ExecutionBatch, ExecutionLimits, SentenceBatchSource},
    token_match::TokenAttributeIndex,
};
use lingrep_corpus::{CorpusArtifact, PreparedSentenceSource};

use crate::artifact::build_two_document_artifact;

fn assert_prepared_source(_: &PreparedSentenceSource) {}

#[tokio::test]
async fn artifact_is_the_public_prepared_source_factory() {
    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let artifact = CorpusArtifact::open(&root).unwrap();

    let mut eager = artifact
        .sentence_source_with_attributes(&[TokenAttributeIndex::Orth])
        .unwrap();
    assert_prepared_source(&eager);
    let ExecutionBatch::Tokenized { sentences, .. } = eager
        .next_batch(&ExecutionLimits::default())
        .unwrap()
        .unwrap()
    else {
        panic!("prepared artifact returned text");
    };
    assert_eq!(sentences[0].value[0].orth(), Some("token-0"));
    assert_eq!(sentences[0].value[0].pos(), None);

    let deferred = artifact
        .deferred_sentence_source(&[TokenAttributeIndex::Orth.into()])
        .unwrap();
    assert_prepared_source(&deferred);
}

#[tokio::test]
async fn written_word_fallback_checks_residency_before_loading_a_token_block() {
    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let artifact = CorpusArtifact::open(&root).unwrap();
    let source = artifact.sentence_source().unwrap();
    assert_eq!(source.sentence_words_bounded(0, 0).unwrap(), None);
    assert_eq!(source.decoded_block_count(), 0);
    assert_eq!(
        source.sentence_words_bounded(0, 128 * 1024).unwrap(),
        Some(vec!["token-0".into()])
    );
    assert!(source.sentence_words_bounded(u64::MAX, usize::MAX).is_err());
}

#[tokio::test]
async fn section_selection_preserves_order_charges_and_membership_admission() {
    use lingrep_core::execution::SentenceAdmission;
    use std::sync::Arc;

    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let artifact = CorpusArtifact::open(&root).unwrap();
    let provenance = Arc::new(artifact.provenance_view().await.unwrap());
    let schema = artifact.token_schema().unwrap();
    let projection = schema.bind_projection([TokenAttributeIndex::Orth]).unwrap();
    for (included, unlabelled, first_document_only, expected, excluded) in [
        (None, true, false, vec![0, 1], 0),
        (Some(vec![]), true, false, vec![0, 1], 0),
        (Some(vec!["methods".into()]), false, false, vec![], 2),
        (None, false, true, vec![], 1),
    ] {
        let mut source = artifact
            .bound_sentence_source_with_sections(
                &projection,
                Arc::clone(&provenance),
                tokio::runtime::Handle::current(),
                included,
                unlabelled,
            )
            .unwrap();
        if first_document_only {
            source = source.with_admission(SentenceAdmission::Documents([0].into_iter().collect()));
        }
        let stats = source.scope_stats();
        let ordinals = tokio::task::spawn_blocking(move || {
            let mut ordinals = Vec::new();
            let limits = ExecutionLimits {
                max_stage_sentences: 1,
                ..Default::default()
            };
            while let Some(batch) = source.next_batch(&limits).unwrap() {
                let ExecutionBatch::BoundTokenized { sentences, charges } = batch else {
                    panic!("expected bound tokens")
                };
                assert_eq!(sentences.len(), charges.len());
                for (sentence, charge) in sentences.into_iter().zip(charges) {
                    assert_eq!(charge, sentence.value.resident_bytes().unwrap());
                    ordinals.push(sentence.location.corpus_sentence_ordinal);
                }
            }
            ordinals
        })
        .await
        .unwrap();
        assert_eq!(ordinals, expected);
        assert_eq!(stats.excluded_sentences(), excluded);
    }
}

#[tokio::test]
async fn section_exclusions_do_not_count_unsampled_cancelled_or_oversized_sentences() {
    use lingrep_core::execution::{
        CancellationFlag, CorpusSample, OversizedSentenceOmissions, TokenizedAdmission,
    };
    use std::sync::{Arc, Mutex};

    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let artifact = CorpusArtifact::open(&root).unwrap();
    let provenance = Arc::new(artifact.provenance_view().await.unwrap());
    let schema = artifact.token_schema().unwrap();
    let projection = schema.bind_projection([TokenAttributeIndex::Orth]).unwrap();
    for mode in ["sample", "cancel", "oversized"] {
        let omissions = Arc::new(Mutex::new(OversizedSentenceOmissions::default()));
        let mut source = artifact
            .bound_sentence_source_with_sections(
                &projection,
                Arc::clone(&provenance),
                tokio::runtime::Handle::current(),
                Some(vec![]),
                false,
            )
            .unwrap();
        let mut limits = ExecutionLimits::default();
        match mode {
            "sample" => {
                let sample = (0..100)
                    .map(|seed| CorpusSample::new(1, seed).unwrap())
                    .find(|sample| !sample.admits(0))
                    .unwrap();
                source = source.with_block_sample(Some(sample));
            }
            "cancel" => {
                let cancel = CancellationFlag::new();
                cancel.cancel();
                source = source.with_cancellation(cancel);
            }
            _ => {
                limits.max_tokenized_bytes = 1;
                source = source
                    .with_tokenized_admission(TokenizedAdmission::Omit(Arc::clone(&omissions)));
            }
        }
        let stats = source.scope_stats();
        tokio::task::spawn_blocking(move || {
            assert!(!source.wants_lookahead());
            assert!(source.next_batch(&limits).unwrap().is_none());
        })
        .await
        .unwrap();
        assert_eq!(stats.excluded_sentences(), 0, "{mode}");
        assert_eq!(
            omissions.lock().unwrap().count,
            if mode == "oversized" { 2 } else { 0 }
        );
    }
}

#[tokio::test]
async fn section_source_rejects_provenance_without_the_artifact_binding() {
    use std::sync::Arc;
    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let artifact = CorpusArtifact::open(&root).unwrap();
    let provenance = lingrep_corpus::ProvenanceView::open(
        &root.join("provenance.sqlite"),
        artifact.analysis_identity(),
    )
    .await
    .unwrap();
    let schema = artifact.token_schema().unwrap();
    let projection = schema.bind_projection([TokenAttributeIndex::Orth]).unwrap();
    assert!(matches!(
        artifact.bound_sentence_source_with_sections(
            &projection,
            Arc::new(provenance),
            tokio::runtime::Handle::current(),
            None,
            false,
        ),
        Err(lingrep_corpus::CorpusError::ArtifactBindingMismatch { .. })
    ));
}
