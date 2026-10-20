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
use std::{fs, path::PathBuf, time::Duration};

use crate::support::test_context;
use lingrep_core::testing::MiniDictionary;
use lingrep_core::{corpus::SentenceId, token_match::TokenAttributeIndex};
use lingrep_web::{
    corpus_runtime::{CacheReadiness, CorpusRuntime, ReadyCorpus},
    provenance_view::HitProvenance,
};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

async fn wait_ready(runtime: &CorpusRuntime) -> ReadyCorpus {
    let mut readiness = runtime.readiness();
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            let state = readiness.borrow_and_update().clone();
            match state {
                CacheReadiness::Ready { .. } => return runtime.ready().await.unwrap(),
                CacheReadiness::Failed { category } => {
                    panic!("corpus preparation failed: {category:?}")
                }
                _ => readiness.changed().await.unwrap(),
            }
        }
    })
    .await
    .expect("corpus preparation timed out")
}

fn percentiles(values: &mut [u128]) -> (u128, u128) {
    values.sort_unstable();
    (
        values[values.len() / 2],
        values[(values.len() * 95 / 100).min(values.len() - 1)],
    )
}

#[tokio::test]
async fn multi_record_jsonl_composes_v6_locations_and_metadata_per_document() {
    let dictionary = MiniDictionary::new();
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    fs::write(
        &corpus,
        concat!(
            "{\"title\":\"first\",\"sentences\":[\"猫が鳴く。\"]}\n",
            "{\"title\":\"second\",\"sentences\":[\"犬が走る。\"]}\n",
        ),
    )
    .unwrap();

    let runtime = CorpusRuntime::new(test_context(dictionary.path()), &corpus).unwrap();
    runtime.start_or_observe().await;
    let ready = wait_ready(&runtime).await;
    let view = ready.artifact.provenance_view().await.unwrap();

    assert_eq!(ready.sentence_count().unwrap(), 2);
    assert_eq!(view.sentence_count(), 2);
    let first = ready.artifact.sentence_provenance(&view, 0).await.unwrap();
    let second = ready.artifact.sentence_provenance(&view, 1).await.unwrap();
    assert_eq!(first.location.document_ordinal, 0);
    assert_eq!(second.location.document_ordinal, 1);
    assert_eq!(
        HitProvenance::from_sentence(&first).document[0].values[0],
        "first"
    );
    assert_eq!(
        HitProvenance::from_sentence(&second).document[0].values[0],
        "second"
    );
}

#[test]
#[ignore = "large-cache sparse page characterization"]
fn large_cache_sparse_page_characterization() {
    let path = std::env::var_os("LINGREP_PAGE_ARTIFACT_PATH").expect("LINGREP_PAGE_ARTIFACT_PATH");
    let artifact = lingrep_corpus::CorpusArtifact::open(std::path::Path::new(&path)).unwrap();
    let source = artifact.sentence_source().unwrap();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(8)
        .build()
        .unwrap();
    let sentence_count = source.sentence_count();
    assert!(sentence_count > 0);
    for page_size in [50_u64, 100, 250] {
        let stride = (sentence_count / page_size).max(1);
        let indices = (0..page_size)
            .map(|ordinal| (ordinal * stride).min(sentence_count - 1))
            .collect::<Vec<_>>();
        let mut elapsed = Vec::with_capacity(10);
        for _ in 0..10 {
            let started = std::time::Instant::now();
            let decoded = source
                .decode_sentences_with_attributes(&pool, &indices, &[TokenAttributeIndex::Orth])
                .unwrap();
            std::hint::black_box(decoded);
            elapsed.push(started.elapsed().as_micros());
        }
        let (p50, p95) = percentiles(&mut elapsed);
        eprintln!(
            "sentences={} blocks={} sparse_page_rows={} sparse_parallel_p50_us={} sparse_parallel_p95_us={}",
            sentence_count,
            source.decoded_block_count(),
            page_size,
            p50,
            p95,
        );
    }
}

#[tokio::test]
#[ignore = "real-corpus capacity characterization"]
async fn structured_provenance_real_corpus_characterization() {
    let corpus_source =
        std::env::var_os("LINGREP_MARKDOWN_CORPUS_PATH").expect("LINGREP_MARKDOWN_CORPUS_PATH");
    let dictionary = PathBuf::from(lingrep_core::testing::dictionary_path());
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("markdown-corpus");
    fs::create_dir(&corpus).unwrap();
    for entry in fs::read_dir(&corpus_source).unwrap() {
        let entry = entry.unwrap();
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "md")
        {
            fs::copy(entry.path(), corpus.join(entry.file_name())).unwrap();
        }
    }

    let started = std::time::Instant::now();
    let runtime = CorpusRuntime::new(test_context(&dictionary), &corpus).unwrap();
    runtime.start_or_observe().await;
    let ready = wait_ready(&runtime).await;
    let build_ms = started.elapsed().as_millis();
    let view = ready.artifact.provenance_view().await.unwrap();
    let sentence_count = view.sentence_count();
    assert!(sentence_count > 0);
    let identity_sha256 = format!("{:x}", Sha256::digest(ready.identity.as_str().as_bytes()));
    eprintln!(
        "documents={} sentences={} build_ms={} corpus_identity_sha256={}",
        ready.artifact.inventory_stats().documents,
        sentence_count,
        build_ms,
        identity_sha256,
    );

    let projector =
        lingrep_web::page_projection::CorpusPageProjector::bind(ready.clone(), &ready.identity)
            .await
            .unwrap();
    let page_rows = 100_u64.min(sentence_count);
    let stride = (sentence_count / page_rows.max(1)).max(1);
    let sparse = (0..page_rows)
        .map(|ordinal| SentenceId::new((ordinal * stride).min(sentence_count - 1)))
        .collect::<Vec<_>>();
    let mut elapsed = Vec::with_capacity(30);
    for _ in 0..30 {
        let started = std::time::Instant::now();
        std::hint::black_box(projector.project(&sparse).await.unwrap());
        elapsed.push(started.elapsed().as_micros());
    }
    let (p50, p95) = percentiles(&mut elapsed);
    eprintln!(
        "projection=corpus_page rows={} sentences={} warm_rounds=30 warm_p50_us={} warm_p95_us={}",
        sparse.len(),
        sentence_count,
        p50,
        p95,
    );
}

#[tokio::test]
async fn content_artifact_reuse_does_not_require_a_legacy_manifest() {
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    fs::write(
        &corpus,
        "{\"title\":\"first\",\"sentences\":[\"猫が鳴く。\",\"犬も鳴く。\"]}\n",
    )
    .unwrap();
    let dictionary = MiniDictionary::new();

    let runtime = CorpusRuntime::new(test_context(dictionary.path()), &corpus).unwrap();
    runtime.start_or_observe().await;
    let ready = wait_ready(&runtime).await;
    let built_count = ready.sentence_count().unwrap();
    let analysis = *ready.artifact.analysis_identity();
    assert!(!corpus.with_extension("provenance.manifest.json").exists());
    drop(ready);

    let reused = CorpusRuntime::new(test_context(dictionary.path()), corpus).unwrap();
    reused.start_or_observe().await;
    let reused = wait_ready(&reused).await;
    let view = reused.artifact.provenance_view().await.unwrap();
    assert_eq!(reused.sentence_count().unwrap(), built_count);
    assert_eq!(view.sentence_count(), built_count);
    assert_eq!(reused.artifact.analysis_identity(), &analysis);
}

#[tokio::test]
async fn fresh_build_and_reuse_publish_equal_composed_sentence_counts() {
    let dictionary = MiniDictionary::new();
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    fs::write(
        &corpus,
        "{\"title\":\"first\",\"sentences\":[\"猫が走る。\",\"   \",\"犬が眠る。\"]}\n",
    )
    .unwrap();

    let first = CorpusRuntime::new(test_context(dictionary.path()), &corpus).unwrap();
    first.start_or_observe().await;
    let first = wait_ready(&first).await;
    let first_view = first.artifact.provenance_view().await.unwrap();
    assert_eq!(first.sentence_count().unwrap(), 2);
    assert_eq!(first_view.sentence_count(), 2);
    let analysis = *first.artifact.analysis_identity();
    drop(first_view);
    drop(first);

    let reused = CorpusRuntime::new(test_context(dictionary.path()), &corpus).unwrap();
    reused.start_or_observe().await;
    let reused = wait_ready(&reused).await;
    let reused_view = reused.artifact.provenance_view().await.unwrap();
    assert_eq!(reused_view.sentence_count(), 2);
    assert_eq!(reused.artifact.analysis_identity(), &analysis);
    assert_eq!(reused.sentence_count().unwrap(), 2);
}
