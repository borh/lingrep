//! The span stream's contract against a real, and a deliberately malformed,
//! provenance sidecar.
//!
//! `ORDER BY span_id` is `first_sentence` order only because the *writer* opens
//! spans as the sentence cursor advances. That is an invariant, not a schema
//! constraint, so a database that violates it cannot be authored through the
//! writer — it has to be forged. Spec obligation 15: the reader must detect it
//! rather than miscount, because a silently reordered stream is an undercount
//! wearing an exact contract.
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

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lingrep_core::testing::MiniDictionary;
use lingrep_web::corpus_runtime::CorpusRuntime;
use lingrep_web::section_tally::{SpanStreamError, spawn_span_producer};
use sqlx::{Connection, SqliteConnection};
use tempfile::{TempDir, tempdir};

/// A prepared artifact over `markdown`, plus the temporary root it lives in.
/// The dictionary is returned because dropping it removes the files the
/// artifact was built against.
async fn prepared(markdown: &str) -> (TempDir, MiniDictionary, Arc<CorpusRuntime>) {
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("markdown-corpus");
    fs::create_dir(&corpus).unwrap();
    fs::write(corpus.join("paper.md"), markdown).unwrap();

    let dictionary = MiniDictionary::new();
    let context =
        Arc::new(lingrep_web::server_context::ServerContext::new(dictionary.path()).unwrap());
    let runtime = Arc::new(CorpusRuntime::new(context, &corpus).unwrap());
    runtime.start_or_observe().await;
    tokio::time::timeout(Duration::from_secs(30), async {
        while runtime.ready().await.is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    (directory, dictionary, runtime)
}

/// The published artifact directory: a sibling of the corpus root, named for
/// the analysis identity. Located rather than computed, so this test does not
/// restate `corpus_runtime`'s naming scheme.
fn artifact_root(directory: &Path) -> PathBuf {
    fs::read_dir(directory)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "lingcorpus")
        })
        .expect("a prepared corpus publishes exactly one artifact")
}

/// Reads every event the producer yields, on a blocking thread: the stream's
/// source drains its channel with `blocking_recv` and must never be polled from
/// a runtime worker.
async fn drain(
    provenance: Arc<lingrep_corpus::ProvenanceView>,
    labels: &[&str],
) -> Result<Vec<(u32, u64, u64)>, SpanStreamError> {
    let labels = labels.iter().map(|label| (*label).to_string()).collect();
    let (handle, mut stream) = spawn_span_producer(provenance, labels);
    let verdict = tokio::task::spawn_blocking(move || {
        let mut seen = Vec::new();
        loop {
            match stream.peek() {
                Ok(Some(event)) => {
                    stream.take();
                    seen.push((event.slot, event.first, event.end));
                }
                Ok(None) => return Ok(seen),
                Err(error) => return Err(error),
            }
        }
    })
    .await
    .unwrap();
    let _ = handle.await;
    verdict
}

#[tokio::test(flavor = "multi_thread")]
async fn a_well_formed_span_table_streams_every_mapped_span() {
    let (_directory, _dictionary, runtime) =
        prepared("::: {.methods}\n一。\n二。\n:::\n::: {.results}\n三。\n:::\n").await;
    let ready = runtime.ready().await.unwrap();
    let view = Arc::new(ready.artifact.provenance_view().await.unwrap());
    let events = drain(view, &["methods", "results"]).await.unwrap();
    assert_eq!(
        events,
        vec![(0, 0, 2), (1, 2, 3)],
        "both spans arrive, mapped to their slots, in first-sentence order"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_label_outside_the_vocabulary_contributes_no_event() {
    let (_directory, _dictionary, runtime) =
        prepared("::: {.methods}\n一。\n:::\n::: {.results}\n二。\n:::\n").await;
    let ready = runtime.ready().await.unwrap();
    let view = Arc::new(ready.artifact.provenance_view().await.unwrap());

    let events = drain(view, &["methods"]).await.unwrap();
    assert_eq!(
        events,
        vec![(0, 0, 1)],
        "an unmapped label is dropped at the source, so the delivered sequence \
         stays a subsequence of the stored one and remains ordered"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_span_table_whose_id_order_disagrees_with_start_order_is_refused() {
    let (directory, _dictionary, runtime) = prepared("::: {.methods}\n一。\n:::\n").await;
    let root = artifact_root(directory.path());
    // Release every read-only handle on the sidecar before rewriting it.
    drop(runtime);
    let sidecar = root.join("provenance.sqlite");

    let mut connection = SqliteConnection::connect(&format!("sqlite://{}", sidecar.display()))
        .await
        .expect("the sidecar opens for writing");
    sqlx::query("DELETE FROM region_spans")
        .execute(&mut connection)
        .await
        .unwrap();
    // span_id 1 starts at 100; span_id 2 starts at 10. No writer can produce
    // this, because a span's id is assigned when it opens.
    for (span_id, first, end) in [(1i64, 100i64, 110i64), (2, 10, 20)] {
        sqlx::query(
            "INSERT INTO region_spans(span_id, label, first_sentence, end_sentence)
             VALUES(?, 'methods', ?, ?)",
        )
        .bind(span_id)
        .bind(first)
        .bind(end)
        .execute(&mut connection)
        .await
        .unwrap();
    }
    connection.close().await.unwrap();

    let artifact = lingrep_corpus::CorpusArtifact::open(&root).unwrap();
    let view = Arc::new(artifact.provenance_view().await.unwrap());
    assert!(
        matches!(
            drain(view, &["methods"]).await,
            Err(SpanStreamError::DecreasingStart {
                previous: 100,
                found: 10
            })
        ),
        "the reader refuses the contract violation rather than miscounting"
    );
}
