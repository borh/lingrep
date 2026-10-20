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
use std::{path::PathBuf, time::Duration};

use crate::support::test_context;
use lingrep_core::testing::MiniDictionary;
use lingrep_web::corpus_runtime::{CacheReadiness, CorpusRuntime};
use tempfile::tempdir;

async fn wait_ready(runtime: &CorpusRuntime) {
    let mut readiness = runtime.readiness();
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            let state = readiness.borrow_and_update().clone();
            match state {
                CacheReadiness::Ready { .. } => return,
                CacheReadiness::Failed { category } => panic!("build failed: {category:?}"),
                _ => readiness.changed().await.unwrap(),
            }
        }
    })
    .await
    .expect("build timed out");
}

fn artifact_path(parent: &std::path::Path) -> PathBuf {
    std::fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".lingcorpus"))
        })
        .unwrap()
}

fn provenance_path(parent: &std::path::Path) -> PathBuf {
    artifact_path(parent).join("provenance.sqlite")
}

#[tokio::test]
async fn successful_build_leaves_no_temp_or_manifest_orphans() {
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    std::fs::write(
        &corpus,
        "{\"title\":\"first\",\"sentences\":[\"猫が鳴く。\"]}\n",
    )
    .unwrap();
    let dictionary = MiniDictionary::new();

    let runtime = CorpusRuntime::new(test_context(dictionary.path()), corpus.clone()).unwrap();
    runtime.start_or_observe().await;
    wait_ready(&runtime).await;

    let base = directory.path();
    let orphans: Vec<_> = std::fs::read_dir(base)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| {
            name.contains(".staging-")
                || name.ends_with(".building")
                || name.contains(".building-")
                || name.ends_with(".provenance.manifest.json")
        })
        .collect();
    assert!(
        orphans.is_empty(),
        "unexpected temp/manifest orphans after publish: {orphans:?}"
    );
}

#[tokio::test]
async fn published_provenance_db_is_a_single_file_with_no_journal_sidecars() {
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    std::fs::write(
        &corpus,
        "{\"title\":\"first\",\"sentences\":[\"猫が鳴く。\"]}\n",
    )
    .unwrap();
    let dictionary = MiniDictionary::new();

    let runtime = CorpusRuntime::new(test_context(dictionary.path()), corpus.clone()).unwrap();
    runtime.start_or_observe().await;
    wait_ready(&runtime).await;

    // The immutable artifact owns the SQLite component. Opening its shared view
    // leaves no `-wal`/`-shm`/`-journal` sidecars beside that component.
    let artifact = artifact_path(directory.path());
    let sidecars: Vec<_> = std::fs::read_dir(&artifact)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| {
            name == "provenance.sqlite-wal"
                || name == "provenance.sqlite-shm"
                || name == "provenance.sqlite-journal"
        })
        .collect();
    assert!(
        sidecars.is_empty(),
        "published provenance DB left journal sidecars: {sidecars:?}"
    );
    assert!(artifact.join("provenance.sqlite").exists());
    assert!(!corpus.with_extension("provenance.sqlite").exists());
}

#[tokio::test]
async fn rebuilding_a_missing_artifact_republishes_complete_shared_provenance() {
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    std::fs::write(&corpus, "{\"sentences\":[\"猫。\"]}\n").unwrap();
    let dictionary = MiniDictionary::new();
    let first = CorpusRuntime::new(test_context(dictionary.path()), &corpus).unwrap();
    first.start_or_observe().await;
    wait_ready(&first).await;
    let first_ready = first.ready().await.unwrap();
    let analysis = *first_ready.artifact.analysis_identity();
    assert_eq!(
        first_ready
            .artifact
            .provenance_view()
            .await
            .unwrap()
            .sentence_count(),
        1
    );
    std::fs::remove_dir_all(artifact_path(directory.path())).unwrap();

    let second = CorpusRuntime::new(test_context(dictionary.path()), &corpus).unwrap();
    second.start_or_observe().await;
    wait_ready(&second).await;
    let second_ready = second.ready().await.unwrap();
    assert_eq!(second_ready.artifact.analysis_identity(), &analysis);
    assert_eq!(
        second_ready
            .artifact
            .provenance_view()
            .await
            .unwrap()
            .sentence_count(),
        1
    );
    assert!(provenance_path(directory.path()).exists());
    assert!(!corpus.with_extension("provenance.sqlite").exists());
}
