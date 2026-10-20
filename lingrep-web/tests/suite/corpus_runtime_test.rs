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
use lingrep_core::execution::SentenceAdmission;
use std::{
    fs::{self, File, FileTimes},
    path::{Path, PathBuf},
    time::Duration,
};

use lingrep_core::{
    corpus::{CorpusReadConfig, admission_identity_digest},
    dictionary::load_dictionary_data,
    execution::{
        CancellationFlag, ExecutionError, ExecutionLimits, MatchSink, SentenceLocation,
        SinkControl, TokenizedBatchObservation, execute,
    },
    fsm::{CompileLimits, Match, PatternSource, compile_catalog},
    testing::MiniDictionary,
    token_match::TokenSequence,
};
use lingrep_corpus::{
    AdmissionIdentity, AnalysisInputs, CorpusError, ReaderIdentity, SourceObservation,
    derive_analysis_identity,
};
use lingrep_web::corpus_runtime::{
    self, CacheFailureCategory, CacheReadiness, CorpusRuntime, ReadyCorpus,
};
use tempfile::tempdir;

use crate::support::{test_context, token};
use crate::v6_support::{build_artifact, open_artifact};

async fn wait_ready(runtime: &CorpusRuntime) -> ReadyCorpus {
    let mut readiness = runtime.readiness();
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            let state = readiness.borrow_and_update().clone();
            match state {
                CacheReadiness::Ready { .. } => return runtime.ready().await.unwrap(),
                CacheReadiness::Failed { category } => panic!("runtime failed: {category:?}"),
                _ => readiness.changed().await.unwrap(),
            }
        }
    })
    .await
    .expect("runtime readiness timed out")
}

async fn nested_alias_runtime() -> (tempfile::TempDir, MiniDictionary, CorpusRuntime) {
    let dictionary = MiniDictionary::new();
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("corpus");
    fs::create_dir_all(corpus.join("shard")).unwrap();
    fs::write(corpus.join("a.md"), "# Introduction\n猫。\n").unwrap();
    fs::write(corpus.join("shard/b.md"), "# Methods\n犬。\n").unwrap();
    std::os::unix::fs::symlink("b.md", corpus.join("shard/zz-alias.md")).unwrap();

    let runtime = CorpusRuntime::new(test_context(dictionary.path()), corpus).unwrap();
    runtime.start_or_observe().await;
    let _ = wait_ready(&runtime).await;
    (directory, dictionary, runtime)
}

fn artifact_directories(parent: &Path) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".lingcorpus"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

/// The analysis inputs a direct run derives, built from the production
/// digests rather than re-derived here.
///
/// This test asserts that the raw-source and artifact constructors expose
/// **one** identity — a plumbing claim. Re-deriving the three formulas locally
/// made it also a formula characterization it was never meant to be, so a
/// domain bump broke it for a reason unrelated to what it checks.
/// `lingrep-corpus/tests/identity.rs` owns the framing characterization and
/// re-derives deliberately; here, calling the real functions is what makes the
/// comparison meaningful.
fn direct_analysis_inputs(config: &CorpusReadConfig, dictionary: &[u8]) -> AnalysisInputs {
    AnalysisInputs::new(
        ReaderIdentity::derive(config, lingrep_core::analysis::NormalizationPolicy::Nfc),
        AdmissionIdentity::from_bytes(admission_identity_digest()),
        // The runtime derives its descriptor from the dictionary fingerprint
        // before loading anything; deriving it the same way here is what makes
        // the comparison a plumbing claim rather than a formula restatement.
        &lingrep_core::analysis::VibratoPlan::descriptor_for(
            *blake3::hash(dictionary).as_bytes(),
            lingrep_core::analysis::ExtractionProfile::Morphology,
            lingrep_core::analysis::SegmentationMode::Input,
            lingrep_core::analysis::NormalizationPolicy::Nfc,
        ),
    )
}

#[derive(Default)]
struct MatchLog(Vec<(u64, Vec<(usize, usize, String)>)>);

impl MatchSink for MatchLog {
    fn push(
        &mut self,
        location: &SentenceLocation,
        _sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        self.0.push((
            location.corpus_sentence_ordinal,
            matches
                .iter()
                .map(|matched| (matched.start, matched.end, matched.pattern.to_string()))
                .collect(),
        ));
        Ok(SinkControl::Continue {
            records_emitted: matches.len(),
        })
    }
}

#[tokio::test]
async fn web_runtime_executes_nested_alias_artifact_through_v6() {
    let nested = build_artifact(vec![
        ("a.md", 0, false, vec![vec![token("猫")]]),
        ("nested/b.md", 1, true, vec![vec![token("犬")]]),
        ("alias.md", 1, true, vec![vec![token("猫")]]),
    ]);
    let flat = build_artifact(vec![
        ("a.md", 0, false, vec![vec![token("猫")]]),
        ("b.md", 1, true, vec![vec![token("犬")]]),
        ("alias.md", 1, true, vec![vec![token("猫")]]),
    ]);
    assert_ne!(
        open_artifact(&nested).source_identity(),
        open_artifact(&flat).source_identity(),
        "a nested logical admission must affect source identity"
    );

    let runtime = CorpusRuntime::open(&nested.path, test_context("/tmp/dict"))
        .await
        .unwrap();
    let ready = runtime.ready().await.unwrap();
    assert_eq!(ready.artifact.inventory_stats().documents, 3);
    assert_eq!(ready.artifact.inventory_stats().alias_documents, 1);
    assert_eq!(ready.artifact.inventory_stats().external_targets, 2);

    let compiled = compile_catalog(
        PatternSource::named("web-v6.lx", "cat := [text=猫]."),
        CompileLimits::default(),
    )
    .unwrap();
    let mut sink = MatchLog::default();
    let mut progress = |_, _: &MatchLog| {};
    let mut observer = |_: TokenizedBatchObservation| Ok(());
    execute(
        ready.artifact.sentence_source().unwrap(),
        |_| unreachable!("V7 sentences are tokenized"),
        &compiled.matcher,
        &ready.pool,
        &ExecutionLimits::default(),
        512,
        &CancellationFlag::new(),
        &mut sink,
        &mut progress,
        &mut observer,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();
    assert_eq!(
        sink.0,
        vec![
            (0, vec![(0, 1, "cat".into())]),
            (1, vec![]),
            (2, vec![(0, 1, "cat".into())]),
        ]
    );
}

#[tokio::test]
async fn rich_web_sentence_composes_dense_location_with_rich_metadata() {
    let (_directory, _dictionary, runtime) = nested_alias_runtime().await;
    let ready = runtime.ready().await.unwrap();
    let view = ready.artifact.provenance_view().await.unwrap();
    let sentence = ready.artifact.sentence_provenance(&view, 1).await.unwrap();
    assert_eq!(sentence.location.source_name.as_ref(), "shard/b.md");
    assert_eq!(sentence.location.document_sentence_ordinal, 0);
    assert!(
        sentence
            .rich
            .structure
            .divisions
            .iter()
            .any(|division| division.kind == "section"
                && lingrep_core::corpus::divisions::metadata_json(&division.metadata)["title"]
                    == "Methods")
    );
}

#[tokio::test]
async fn prepared_jsonl_identity_matches_direct_physical_source_observation() {
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    let raw = concat!(
        "{\"title\":\"first\",\"sentences\":[\"猫が鳴く。\"]}\n",
        "{\"title\":\"second\",\"sentences\":[\"犬が走る。\"]}\n",
    );
    fs::write(&corpus, raw).unwrap();
    let dictionary = MiniDictionary::new();
    let dictionary_path = dictionary.path();

    let runtime = CorpusRuntime::new(test_context(dictionary_path), &corpus).unwrap();
    runtime.start_or_observe().await;
    let ready = wait_ready(&runtime).await;

    let observation = SourceObservation::new();
    observation.begin_source("corpus.jsonl").unwrap();
    observation.update_source_bytes(raw.as_bytes()).unwrap();
    observation.end_source().unwrap();
    let observed = observation.finish().unwrap();
    let config = corpus_runtime::read_config(
        &corpus,
        None,
        lingrep_core::corpus::MarkdownReaderConfig::new(
            lingrep_core::corpus::SectionPolicy::default(),
        ),
    );
    let dictionary = load_dictionary_data(dictionary_path.to_str().unwrap()).unwrap();
    let expected_analysis =
        derive_analysis_identity(&observed, &direct_analysis_inputs(&config, &dictionary));

    assert_eq!(ready.artifact.source_identity(), observed.source_identity());
    assert_eq!(ready.artifact.analysis_identity(), &expected_analysis);
    let artifact_path = artifact_directories(directory.path()).pop().unwrap();
    let reopened = CorpusRuntime::open(artifact_path, test_context("/tmp/dict"))
        .await
        .unwrap();
    assert_eq!(
        ready.identity,
        reopened.ready().await.unwrap().identity,
        "raw-source and artifact constructors must expose one V7 analysis identity"
    );
}

#[tokio::test]
async fn artifact_reuse_depends_on_content_not_timestamps() {
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    let first = "{\"sentences\":[\"猫。\"]}\n";
    let second = "{\"sentences\":[\"犬。\"]}\n";
    assert_eq!(first.len(), second.len());
    fs::write(&corpus, first).unwrap();
    let original_modified = fs::metadata(&corpus).unwrap().modified().unwrap();
    let dictionary = MiniDictionary::new();

    let first_runtime = CorpusRuntime::new(test_context(dictionary.path()), &corpus).unwrap();
    first_runtime.start_or_observe().await;
    let first_ready = wait_ready(&first_runtime).await;
    assert_eq!(artifact_directories(directory.path()).len(), 1);

    File::options()
        .write(true)
        .open(&corpus)
        .unwrap()
        .set_times(FileTimes::new().set_modified(original_modified + Duration::from_secs(60)))
        .unwrap();
    let touched_runtime = CorpusRuntime::new(test_context(dictionary.path()), &corpus).unwrap();
    touched_runtime.start_or_observe().await;
    let touched_ready = wait_ready(&touched_runtime).await;
    assert_eq!(touched_ready.identity, first_ready.identity);
    assert_eq!(artifact_directories(directory.path()).len(), 1);

    fs::write(&corpus, second).unwrap();
    File::options()
        .write(true)
        .open(&corpus)
        .unwrap()
        .set_times(FileTimes::new().set_modified(original_modified))
        .unwrap();
    let changed_runtime = CorpusRuntime::new(test_context(dictionary.path()), &corpus).unwrap();
    changed_runtime.start_or_observe().await;
    let changed_ready = wait_ready(&changed_runtime).await;
    assert_ne!(changed_ready.identity, first_ready.identity);
    let tokens = changed_ready
        .reader()
        .unwrap()
        .bind(&changed_ready.identity)
        .unwrap()
        .sentence(0)
        .await
        .unwrap()
        .unwrap();
    assert!(tokens.iter().any(|token| token.orth() == Some("犬")));
    assert_eq!(artifact_directories(directory.path()).len(), 2);
}

fn record_limit_runtime(corpus: &Path, dictionary: &Path, limit: usize) -> CorpusRuntime {
    CorpusRuntime::new_with_override(
        test_context(dictionary),
        corpus,
        None,
        lingrep_core::corpus::MarkdownReaderConfig::new(
            lingrep_core::corpus::SectionPolicy::default(),
        ),
        limit,
    )
    .unwrap()
}

async fn terminal_readiness(runtime: &CorpusRuntime) -> CacheReadiness {
    let mut readiness = runtime.readiness();
    runtime.start_or_observe().await;
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            let state = readiness.borrow_and_update().clone();
            if matches!(
                state,
                CacheReadiness::Ready { .. } | CacheReadiness::Failed { .. }
            ) {
                return state;
            }
            readiness.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn oversized_jsonl_record_fails_before_unbounded_staging() {
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    let mut oversized = serde_json::to_vec(&serde_json::json!({
        "sentences": vec![""; 50_000],
    }))
    .unwrap();
    oversized.push(b'\n');
    let limit = 64 * 1024;
    assert!(oversized.len() > limit);
    fs::write(&corpus, oversized).unwrap();
    let dictionary = MiniDictionary::new();
    let runtime = record_limit_runtime(&corpus, dictionary.path(), limit);

    assert_eq!(
        terminal_readiness(&runtime).await,
        CacheReadiness::Failed {
            category: CacheFailureCategory::Corpus
        }
    );
}

/// A JSONL document is one physical record, and its size is a property of the
/// corpus rather than of how execution batches raw bytes. Bounding a record at
/// the stage batch budget refused ordinary corpora — a science corpus whose
/// three largest articles exceeded 128 KiB — so the record bound is its own
/// quantity and defaults an order of magnitude higher.
#[tokio::test]
async fn a_record_above_the_stage_batch_budget_still_builds() {
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    let sentence = "猫。".repeat(130);
    let mut record = serde_json::to_vec(&serde_json::json!({
        "sentences": vec![sentence; 300],
    }))
    .unwrap();
    record.push(b'\n');
    assert!(record.len() > lingrep_core::execution::DEFAULT_MAX_STAGE_INPUT_BYTES);
    assert!(record.len() < lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES);
    fs::write(&corpus, record).unwrap();
    let dictionary = MiniDictionary::new();
    let runtime = CorpusRuntime::new(test_context(dictionary.path()), &corpus).unwrap();

    assert!(matches!(
        terminal_readiness(&runtime).await,
        CacheReadiness::Ready { .. }
    ));

    // The same record against the stage batch budget, so the two bounds are
    // demonstrably distinct.
    let at_stage_budget = record_limit_runtime(
        &corpus,
        dictionary.path(),
        lingrep_core::execution::DEFAULT_MAX_STAGE_INPUT_BYTES,
    );
    assert_eq!(
        terminal_readiness(&at_stage_budget).await,
        CacheReadiness::Failed {
            category: CacheFailureCategory::Corpus
        }
    );
}

#[tokio::test]
async fn corruption_retry_discards_the_published_artifact_and_rebuilds() {
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    fs::write(&corpus, "{\"sentences\":[\"猫。\"]}\n").unwrap();
    let dictionary = MiniDictionary::new();
    let runtime = CorpusRuntime::new(test_context(dictionary.path()), &corpus).unwrap();
    runtime.start_or_observe().await;
    let ready = wait_ready(&runtime).await;
    let identity = ready.identity.clone();
    let artifact_path = artifact_directories(directory.path()).pop().unwrap();
    crate::v6_support::corrupt_first_token_block(&artifact_path.join("tokens.v1.zst"));
    assert!(
        ready
            .reader()
            .unwrap()
            .bind(&identity)
            .unwrap()
            .sentence(0)
            .await
            .is_err()
    );

    runtime.invalidator().invalidate_corrupt().await;
    drop(ready);
    runtime.retry().await;
    let rebuilt = wait_ready(&runtime).await;

    assert_eq!(rebuilt.identity, identity);
    assert!(
        rebuilt
            .reader()
            .unwrap()
            .bind(&identity)
            .unwrap()
            .sentence(0)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn runtime_open_rejects_a_regular_file_as_not_an_artifact() {
    let directory = tempdir().unwrap();
    let cache = directory.path().join("not-an-artifact.bin");
    fs::write(&cache, b"not an artifact directory").unwrap();

    assert!(matches!(
        CorpusRuntime::open(cache, test_context("/tmp/dict")).await,
        Err(CorpusError::ExpectedArtifactDirectory { .. })
    ));
}

#[test]
fn web_execution_has_no_v4_or_cached_executor_dependency() {
    for path in runtime_rust_sources(Path::new(env!("CARGO_MANIFEST_DIR")).join("src")) {
        let source = fs::read_to_string(&path).unwrap();
        for forbidden in [
            "StreamingLazyCache",
            "StreamingLazyWriter",
            "lingrep_core::cached_executor",
        ] {
            assert!(
                !source.contains(forbidden),
                "{forbidden} in {}",
                path.display()
            );
        }
    }
}

#[test]
fn web_has_no_competing_provenance_or_identity_owner() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    for path in [
        "lingrep-web/src/provenance/reader.rs",
        "lingrep-web/src/provenance/writer.rs",
        "lingrep-web/src/corpus_source.rs",
    ] {
        assert!(!workspace.join(path).exists(), "{path} survived");
    }
    for path in runtime_rust_sources(workspace.join("lingrep-web/src")) {
        let source = fs::read_to_string(&path).unwrap();
        assert!(!source.contains("sqlx::SqlitePool"), "{}", path.display());
        assert!(
            !source.contains("SqliteConnectOptions"),
            "{}",
            path.display()
        );
        assert!(!source.contains("identity_component"), "{}", path.display());
    }
}

fn runtime_rust_sources(root: impl AsRef<Path>) -> Vec<std::path::PathBuf> {
    fn walk(path: &Path, output: &mut Vec<std::path::PathBuf>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let file_type = entry.file_type().unwrap();
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                walk(&entry.path(), output);
            } else if file_type.is_file()
                && entry.path().extension().is_some_and(|value| value == "rs")
            {
                output.push(entry.path());
            }
        }
    }
    let mut output = Vec::new();
    walk(root.as_ref(), &mut output);
    output.sort();
    output
}

#[tokio::test]
async fn concurrent_starts_publish_one_failure() {
    let directory = tempdir().unwrap();
    let runtime = CorpusRuntime::new(
        test_context(directory.path().join("missing.dic")),
        directory.path().join("missing.jsonl"),
    )
    .unwrap();
    let mut readiness = runtime.readiness();

    tokio::join!(runtime.start_or_observe(), runtime.start_or_observe());
    tokio::time::timeout(Duration::from_secs(2), async {
        while !matches!(*readiness.borrow(), CacheReadiness::Failed { .. }) {
            readiness.changed().await.unwrap();
        }
    })
    .await
    .unwrap();

    assert_eq!(
        readiness.borrow().clone(),
        CacheReadiness::Failed {
            category: CacheFailureCategory::Configuration
        }
    );
}

#[tokio::test]
async fn corruption_invalidation_is_published() {
    let directory = tempdir().unwrap();
    let runtime = CorpusRuntime::new(
        test_context(directory.path().join("dict.dic")),
        directory.path().join("corpus.jsonl"),
    )
    .unwrap();
    let mut readiness = runtime.readiness();

    runtime.invalidator().invalidate_corrupt().await;
    readiness.changed().await.unwrap();
    assert_eq!(
        readiness.borrow().clone(),
        CacheReadiness::Failed {
            category: CacheFailureCategory::Corruption
        }
    );
}

#[tokio::test]
async fn repeated_invalidation_broadcasts_failed_once() {
    let directory = tempdir().unwrap();
    let runtime = CorpusRuntime::new(
        test_context(directory.path().join("dict.dic")),
        directory.path().join("corpus.jsonl"),
    )
    .unwrap();
    let mut readiness = runtime.readiness();
    let invalidator = runtime.invalidator();

    // Three concurrent readers each report the same corruption. Each call
    // returns whether *it* performed the teardown; the guard must let
    // exactly one of the three claim it.
    let (a, b, c) = tokio::join!(
        invalidator.invalidate_corrupt(),
        invalidator.invalidate_corrupt(),
        invalidator.invalidate_corrupt(),
    );

    readiness.changed().await.unwrap();
    assert_eq!(
        readiness.borrow_and_update().clone(),
        CacheReadiness::Failed {
            category: CacheFailureCategory::Corruption
        }
    );
    // No further transition is pending: the guard debounced the duplicates.
    assert!(!readiness.has_changed().unwrap());
    assert_eq!(
        [a, b, c].iter().filter(|result| **result).count(),
        1,
        "teardown should run exactly once for 3 concurrent corruption reports"
    );
}

/// A subscriber arriving after the fact must still observe a terminal
/// readiness published while nobody was listening. That is what `send_replace`
/// buys over `send`, which reports an error and leaves the value untouched
/// when no receiver is alive, so `readiness()` would hand back a stale
/// `Building` forever.
///
/// The wait polls a receiver it drops again immediately rather than holding
/// one open, because a receiver alive at the moment of publication is exactly
/// the condition that makes the wrong call succeed, and would retire the
/// guard. It replaced a flat 100ms sleep, which stood in no relation to how
/// long a failing configuration probe takes and could simply expire early
/// under load.
#[tokio::test]
async fn readiness_published_before_subscription_is_retained() {
    let directory = tempdir().unwrap();
    let runtime = CorpusRuntime::new(
        test_context(directory.path().join("missing.dic")),
        directory.path().join("missing.jsonl"),
    )
    .unwrap();

    runtime.start_or_observe().await;
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            let settled = matches!(
                runtime.readiness().borrow().clone(),
                CacheReadiness::Failed { .. } | CacheReadiness::Ready { .. }
            );
            if settled {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the configuration probe must settle");
    let readiness = runtime.readiness();

    assert_eq!(
        readiness.borrow().clone(),
        CacheReadiness::Failed {
            category: CacheFailureCategory::Configuration
        }
    );
}

/// The provenance view is opened **once** per ready corpus, not once per
/// caller.
///
/// `CorpusArtifact::provenance_view` validates every sentence row in the
/// artifact, so it costs O(corpus) — measured at roughly 1.5 s on a
/// 523 883-sentence corpus. A handler calling it directly would put that scan
/// on the index page, on every corpus-context poll, and on every pattern- and
/// hit-detail request.
///
/// This pins the property that prevents it, and does so by identity rather than
/// by timing: two `ready()` handles — which is exactly what two requests
/// get — must hand back the *same* view, not two equal ones. A regression to
/// per-call opens still returns a valid view and still passes every
/// behavioural test, so `Arc::ptr_eq` is what catches it.
#[tokio::test]
async fn the_provenance_view_is_opened_once_and_shared_by_every_reader() {
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    fs::write(&corpus, "{\"sentences\":[\"猫が走る。\",\"犬が寝る。\"]}\n").unwrap();
    let dictionary = MiniDictionary::new();
    let runtime = CorpusRuntime::new(test_context(dictionary.path()), &corpus).unwrap();
    runtime.start_or_observe().await;
    wait_ready(&runtime).await;

    // Two separate `ready()` handles: the shape two concurrent requests have.
    let first = runtime.ready().await.unwrap();
    let second = runtime.ready().await.unwrap();
    let view_a = first.provenance().await.unwrap();
    let view_b = second.provenance().await.unwrap();
    assert!(
        std::sync::Arc::ptr_eq(&view_a, &view_b),
        "two readers of one ready corpus must share one opened provenance view"
    );

    // And a repeat call on the same handle, which is the within-request shape.
    let view_c = first.provenance().await.unwrap();
    assert!(std::sync::Arc::ptr_eq(&view_a, &view_c));
    assert_eq!(view_a.sentence_count(), 2);
}

/// Corruption invalidation drops the cached view.
///
/// This is not hygiene. `corruption_retry_discards_the_published_artifact_and_
/// rebuilds` above asserts that the rebuild carries the **same** analysis
/// identity, because it reads the same source under the same analyzer — so the
/// cache's identity key alone would happily hand back a view onto the artifact
/// that was just deleted. Clearing on invalidation is the only thing that
/// prevents it.
#[tokio::test]
async fn corruption_invalidation_drops_the_cached_provenance_view() {
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    fs::write(&corpus, "{\"sentences\":[\"猫。\"]}\n").unwrap();
    let dictionary = MiniDictionary::new();
    let runtime = CorpusRuntime::new(test_context(dictionary.path()), &corpus).unwrap();
    runtime.start_or_observe().await;
    let ready = wait_ready(&runtime).await;
    let identity = ready.identity.clone();
    let stale_view = ready.provenance().await.unwrap();

    let artifact_path = artifact_directories(directory.path()).pop().unwrap();
    crate::v6_support::corrupt_first_token_block(&artifact_path.join("tokens.v1.zst"));
    runtime.invalidator().invalidate_corrupt().await;
    drop(ready);
    runtime.retry().await;
    let rebuilt = wait_ready(&runtime).await;

    // Same identity — which is precisely why the key cannot be trusted here.
    assert_eq!(rebuilt.identity, identity);
    let fresh_view = rebuilt.provenance().await.unwrap();
    assert!(
        !std::sync::Arc::ptr_eq(&stale_view, &fresh_view),
        "the view onto the discarded artifact must not survive invalidation"
    );
    assert_eq!(fresh_view.sentence_count(), 1);
}

/// A prepared artifact named directly as the corpus is served as-is.
///
/// This is what makes a corpus format lingrep-web cannot *read* usable anyway: a
/// BCCWJ distribution prepared by `ling corpus build --input-format bccwj`
/// is an ordinary artifact by the time it reaches here, so no C-XML reader is
/// needed on this side. Nothing about the source is consulted — there is no
/// source beside the artifact to consult.
#[tokio::test]
async fn a_declared_artifact_is_served_without_building_anything() {
    let fixture = crate::v6_support::build_artifact(vec![(
        "fixture.md",
        0,
        false,
        vec![vec![token("猫"), token("が"), token("走る")]],
    )]);
    let dictionary = MiniDictionary::new();
    // The artifact directory itself is the declared corpus path.
    let runtime = CorpusRuntime::new(test_context(dictionary.path()), &fixture.path).unwrap();
    runtime.start_or_observe().await;
    let ready = wait_ready(&runtime).await;

    assert_eq!(ready.sentence_count().unwrap(), 1);
    let sentence = ready
        .reader()
        .unwrap()
        .bind(&ready.identity)
        .unwrap()
        .sentence(0)
        .await
        .unwrap()
        .expect("the declared artifact's sentence decodes");
    assert_eq!(sentence.len(), 3);

    // No sibling artifact was published: the declared one is read in place.
    let siblings = artifact_directories(fixture.path.parent().unwrap());
    assert_eq!(
        siblings,
        vec![fixture.path.clone()],
        "declaring an artifact must not build a second one beside it"
    );
}

/// A declared artifact is never discarded as though it were this runtime's own
/// cache.
///
/// `invalidate_corrupt` deletes the artifact it owns, because a self-built one
/// is reproducible: the source is right there and `retry` rebuilds it. A
/// declared artifact is **not** reproducible from anything lingrep-web holds, so the
/// same deletion would destroy the operator's data with no way back. The
/// runtime still flips to `Failed` — it genuinely cannot serve corrupt bytes —
/// but the bytes stay on disk.
#[tokio::test]
async fn a_declared_artifact_survives_corruption_invalidation() {
    let fixture =
        crate::v6_support::build_artifact(vec![("fixture.md", 0, false, vec![vec![token("猫")]])]);
    let dictionary = MiniDictionary::new();
    let runtime = CorpusRuntime::new(test_context(dictionary.path()), &fixture.path).unwrap();
    runtime.start_or_observe().await;
    wait_ready(&runtime).await;

    runtime.invalidator().invalidate_corrupt().await;

    assert!(
        fixture.path.is_dir(),
        "a declared artifact must not be deleted by corruption invalidation"
    );
    assert!(
        fixture.path.join("manifest.json").is_file(),
        "the declared artifact's components must still be intact"
    );
    assert!(
        matches!(*runtime.readiness().borrow(), CacheReadiness::Failed { .. }),
        "the runtime still refuses to serve it"
    );
}
