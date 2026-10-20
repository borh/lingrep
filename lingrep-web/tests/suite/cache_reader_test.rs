use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};
use lingrep_web::corpus_runtime::{
    CacheFailureCategory, CacheReadiness, CorpusIdentity, CorpusReadError, CorpusRuntime,
};

use crate::v6_support::{corrupt_first_token_block, ready_fixture};

fn token(orth: &str) -> TokenInfo {
    TokenInfo::default().with_orth(orth).with_pos("NOUN")
}

#[tokio::test]
async fn mismatched_identity_never_decodes_a_sentence() {
    let (_fixture, ready) = ready_fixture("current", vec![vec![token("猫")]]);
    let reader = ready.reader().unwrap();
    assert!(reader.bind(&CorpusIdentity::new("old")).is_none());
    assert_eq!(reader.decoded_block_count(), 0);
}

#[tokio::test]
async fn valid_bound_reader_decodes_a_sentence() {
    let (_fixture, ready) = ready_fixture("current", vec![vec![token("を"), token("食べる")]]);
    let sentence = ready
        .reader()
        .unwrap()
        .bind(&CorpusIdentity::new("current"))
        .unwrap()
        .sentence(0)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sentence[1].orth(), Some("食べる"));
}

#[tokio::test]
async fn corruption_is_a_pure_value_and_does_not_flip_readiness() {
    let (fixture, ready) = ready_fixture("current", vec![vec![token("食べる")]]);
    let reader = ready.reader().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let context =
        std::sync::Arc::new(lingrep_web::server_context::ServerContext::new("/tmp/dict").unwrap());
    let runtime = CorpusRuntime::new(context, directory.path().join("corpus")).unwrap();
    let mut readiness = runtime.readiness();
    corrupt_first_token_block(&fixture.path.join("tokens.v1.zst"));

    let result = reader
        .bind(&CorpusIdentity::new("current"))
        .unwrap()
        .sentence(0)
        .await;

    assert!(matches!(result, Err(CorpusReadError::Corruption(_))));
    assert!(!readiness.has_changed().unwrap());
    runtime.invalidator().invalidate_corrupt().await;
    readiness.changed().await.unwrap();
    assert_eq!(
        readiness.borrow_and_update().clone(),
        CacheReadiness::Failed {
            category: CacheFailureCategory::Corruption
        }
    );
}

#[test]
fn bound_reader_preserves_positions_but_reuses_blocks_and_projects_columns() {
    let (_fixture, ready) = ready_fixture(
        "current",
        vec![
            vec![token("猫"), token("が")],
            vec![token("犬"), token("も")],
        ],
    );
    let reader = ready.reader().unwrap();
    let bound = reader.bind(&CorpusIdentity::new("current")).unwrap();
    let out = bound
        .decode_sentences_with_attributes(&[1, 0, 1, 999], &[TokenAttributeIndex::Orth])
        .unwrap();

    assert_eq!(out.len(), 4);
    assert_eq!(out[0].as_ref().unwrap()[0].orth(), Some("犬"));
    assert_eq!(out[0], out[2]);
    assert_eq!(out[1].as_ref().unwrap()[0].orth(), Some("猫"));
    assert!(out[3].is_none());
    assert!(
        out.iter()
            .flatten()
            .flatten()
            .all(|token| token.pos().is_none())
    );
    assert_eq!(bound.decoded_block_count(), 1);
}

#[test]
fn decode_sentences_preserves_full_attributes_and_input_positions() {
    let (_fixture, ready) = ready_fixture(
        "current",
        vec![
            vec![token("猫"), token("が")],
            vec![token("犬"), token("も")],
        ],
    );
    let out = ready
        .reader()
        .unwrap()
        .bind(&CorpusIdentity::new("current"))
        .unwrap()
        .decode_sentences(&[1, 0, 1, 999])
        .unwrap();
    assert_eq!(out.len(), 4);
    assert_eq!(out[0], out[2]);
    assert_eq!(out[1].as_ref().unwrap()[0].pos(), Some("NOUN"));
    assert!(out[3].is_none());
}
