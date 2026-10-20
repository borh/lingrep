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
