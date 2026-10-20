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

use lingrep_core::corpus::SentenceId;
use lingrep_web::corpus_runtime::{CorpusIdentity, CorpusRuntime, ReadyCorpus};

use crate::support::token;
use crate::v6_support::ready_fixture;

async fn page_fixture() -> (tempfile::TempDir, CorpusRuntime, ReadyCorpus) {
    let (fixture, base_ready) =
        ready_fixture("current", vec![vec![token("一")], vec![token("二")]]);
    let directory = fixture.directory;
    let context = Arc::new(lingrep_web::server_context::ServerContext::new("/tmp/dict").unwrap());
    let runtime = CorpusRuntime::from_ready(
        base_ready.identity,
        Arc::into_inner(base_ready.artifact).unwrap(),
        context,
    );
    let ready = runtime.ready().await.unwrap();
    (directory, runtime, ready)
}

#[test]
fn result_page_policy_keeps_one_hundred_as_default() {
    assert_eq!(lingrep_web::page_projection::DEFAULT_RESULT_PAGE_SIZE, 100);
    assert_eq!(
        lingrep_web::page_projection::RESULT_PAGE_SIZE_CHOICES,
        [50, 100, 250, 500],
    );
    assert!(!lingrep_web::page_projection::RESULT_PAGE_SIZE_CHOICES.contains(&1_000));
    assert_eq!(lingrep_web::page_projection::MAX_RESULT_PAGE_SIZE, 500);
}

#[tokio::test]
async fn projector_preserves_order_and_shares_duplicate_sentence_tokens() {
    let (_directory, _runtime, ready) = page_fixture().await;
    let identity = ready.identity.clone();
    let projector = lingrep_web::page_projection::CorpusPageProjector::bind(ready, &identity)
        .await
        .unwrap();
    let ids = [SentenceId::new(1), SentenceId::new(0), SentenceId::new(1)];

    let page = projector.project(&ids).await.unwrap();

    assert_eq!(
        page.iter()
            .map(|row| SentenceId::new(row.provenance.location.corpus_sentence_ordinal))
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(page[0].tokens[0].orth(), Some("二"));
    assert_eq!(page[1].tokens[0].orth(), Some("一"));
    assert!(Arc::ptr_eq(&page[0].tokens, &page[2].tokens));
    assert_eq!(page[0].provenance, page[2].provenance);
    assert_eq!(
        page[0].provenance.location.source_name.as_ref(),
        "fixture.md"
    );
}

#[tokio::test]
async fn projector_accepts_the_default_and_maximum_page_sizes() {
    let (_directory, _runtime, ready) = page_fixture().await;
    let identity = ready.identity.clone();
    let projector = lingrep_web::page_projection::CorpusPageProjector::bind(ready, &identity)
        .await
        .unwrap();

    let default_page = vec![SentenceId::new(0); 100];
    let max_page = vec![SentenceId::new(1); 500];
    assert_eq!(projector.project(&default_page).await.unwrap().len(), 100);
    assert_eq!(projector.project(&max_page).await.unwrap().len(), 500);
}

#[tokio::test]
async fn projector_rejects_stale_identity_and_oversized_page_before_decode() {
    let (_directory, _runtime, ready) = page_fixture().await;
    let cache = ready.clone();
    assert!(
        lingrep_web::page_projection::CorpusPageProjector::bind(
            ready.clone(),
            &CorpusIdentity::new("stale"),
        )
        .await
        .is_err()
    );
    assert_eq!(cache.decoded_block_count(), 0);

    let identity = ready.identity.clone();
    let projector = lingrep_web::page_projection::CorpusPageProjector::bind(ready, &identity)
        .await
        .unwrap();
    let oversized = vec![SentenceId::new(0); 501];
    assert!(projector.project(&oversized).await.is_err());
    assert_eq!(cache.decoded_block_count(), 0);
}

#[tokio::test]
async fn projector_returns_corruption_as_a_pure_value_and_does_not_flip_readiness() {
    // project() is pure: it detects corruption and returns it as a value. It
    // has no caller-owned invalidation site today, so it must not touch
    // global readiness itself — an eventual owner (if one is added) is
    // responsible for driving CacheReadiness::Failed.
    let (directory, runtime, ready) = page_fixture().await;
    let cache_path = directory.path().join("fixture.lingcorpus/tokens.v1.zst");
    crate::v6_support::corrupt_first_token_block(&cache_path);
    let identity = ready.identity.clone();
    let projector = lingrep_web::page_projection::CorpusPageProjector::bind(ready, &identity)
        .await
        .unwrap();
    let readiness = runtime.readiness();

    assert!(matches!(
        projector.project(&[SentenceId::new(1)]).await,
        Err(lingrep_web::page_projection::PageProjectionError::CacheCorruption)
    ));
    assert!(!readiness.has_changed().unwrap());
}

#[test]
fn projection_has_one_blocking_decode_boundary_and_no_point_reads() {
    let source = include_str!("../../src/page_projection.rs");
    assert_eq!(source.matches("spawn_blocking").count(), 1);
    assert_eq!(
        source.matches("decode_sentences_with_attributes").count(),
        1
    );
    assert!(!source.contains("try_get_sentence"));
}
