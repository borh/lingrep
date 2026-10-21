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
use std::sync::Arc;
use std::time::Duration;

use lingrep_core::testing::MiniDictionary;
use lingrep_web::corpus_context::corpus_context_view;
use lingrep_web::corpus_runtime::{CacheReadiness, CorpusRuntime, ReadyCorpus};
use lingrep_web::corpus_set::CorpusName;
use lingrep_web::state::{Config, DetailLimits, RequestStore};
use tempfile::tempdir;

/// The display name `declared_corpora` would compute: the path's file stem.
fn display_name_of(path: &std::path::Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "corpus".to_string())
}

/// Poll the readiness watch until the corpus is `Ready` (or fail loudly).
/// Copied from `provenance_runtime_test.rs` so this test needs no shared harness.
async fn wait_ready(runtime: &CorpusRuntime) -> ReadyCorpus {
    let mut readiness = runtime.readiness();
    tokio::time::timeout(Duration::from_mins(3), async {
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

/// The main happy path: a ready corpus with shared artifact provenance resolves to
/// state 1 through `corpus_context_view` — corpus name (stem only), sentence and
/// document counts, at least one section row, and the identity digest, with
/// `note == None` and `polling == false`. Only the not-ready "preparing" state
/// is exercised elsewhere; this closes the state-1 end-to-end gap.
///
/// The morphology is incidental here; the owned miniature dictionary keeps the
/// artifact build real while avoiding a production-UniDic dependency.
#[tokio::test]
async fn corpus_context_view_maps_ready_corpus_to_state_one() {
    let dictionary = MiniDictionary::new();
    let directory = tempdir().unwrap();
    // A Markdown-directory corpus so the build emits a real section region: the
    // `.methods` div is a non-excluded label, so its sentence is admitted and
    // recorded as rich provenance that the web display projection counts (see
    // corpus_query_e2e_test.rs::kwic_views_surface_section_and_document_provenance).
    let corpus = directory.path().join("markdown-corpus");
    fs::create_dir(&corpus).unwrap();
    fs::write(
        corpus.join("01.md"),
        concat!(
            "---\ntitle: First paper\nyear: 2013\n---\n# First\n",
            "::: {.b .m .a}\n一。\n:::\n",
            "::: {.b .m .n}\n二。\n:::\n",
            "::: {.b .n}\n三。\n:::\n",
        ),
    )
    .unwrap();

    let context =
        Arc::new(lingrep_web::server_context::ServerContext::new(dictionary.path()).unwrap());
    let runtime = Arc::new(CorpusRuntime::new(context, &corpus).unwrap());
    runtime.start_or_observe().await;
    wait_ready(&runtime).await;

    let store = RequestStore::with_runtime(
        Config {
            dict_path: dictionary.path().to_string_lossy().into_owned(),
            corpora: vec![(
                CorpusName::default_key(),
                corpus.clone(),
                display_name_of(&corpus),
            )],
            input_format: None,
            markdown: lingrep_core::corpus::MarkdownReaderConfig::new(
                lingrep_core::corpus::SectionPolicy::default(),
            ),
            max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
            detail_limits: DetailLimits { max_kwic: 10 },
        },
        Arc::clone(&runtime),
    );

    let view = corpus_context_view(&store, None).await;

    // Name is the corpus path's file stem — never the path itself (trust boundary).
    assert_eq!(view.name, "markdown-corpus");
    assert!(
        !view.name.contains('/'),
        "the displayed name must not carry the corpus path"
    );

    // Identity digest: present, 12 lowercase-hex chars.
    let digest = view
        .identity_digest
        .as_deref()
        .expect("state 1 exposes the identity digest");
    assert_eq!(digest.len(), 12);
    assert!(
        digest
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "digest must be lowercase hex, got {digest:?}"
    );

    assert_eq!(view.sentence_count_label.as_deref(), Some("3"));
    assert_eq!(view.document_count_label.as_deref(), Some("1"));

    // State 1 (not a degraded state): no note, not polling.
    assert!(view.note.is_none(), "state 1 carries no note");
    assert!(!view.polling, "a ready corpus does not poll");

    assert_eq!(
        view.sections
            .iter()
            .map(|section| (section.label.as_str(), section.count_label.as_str()))
            .collect::<Vec<_>>(),
        [("b", "3"), ("m", "2"), ("n", "2"), ("a", "1")],
        "coverage sorts descending, then labels ascending for ties"
    );
}

#[tokio::test]
async fn corpus_context_counts_admitted_jsonl_documents_not_physical_sources() {
    let dictionary = MiniDictionary::new();
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("records.jsonl");
    fs::write(
        &corpus,
        "{\"sentences\":[\"一。\"]}\n{\"sentences\":[\"二。\"]}\n",
    )
    .unwrap();
    let context =
        Arc::new(lingrep_web::server_context::ServerContext::new(dictionary.path()).unwrap());
    let runtime = Arc::new(CorpusRuntime::new(context, &corpus).unwrap());
    runtime.start_or_observe().await;
    wait_ready(&runtime).await;
    let store = RequestStore::with_runtime(
        Config {
            dict_path: dictionary.path().to_string_lossy().into_owned(),
            corpora: vec![(
                CorpusName::default_key(),
                corpus.clone(),
                display_name_of(&corpus),
            )],
            input_format: None,
            markdown: lingrep_core::corpus::MarkdownReaderConfig::new(
                lingrep_core::corpus::SectionPolicy::default(),
            ),
            max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
            detail_limits: DetailLimits { max_kwic: 10 },
        },
        runtime,
    );

    let view = corpus_context_view(&store, None).await;

    assert_eq!(view.sentence_count_label.as_deref(), Some("2"));
    assert_eq!(view.document_count_label.as_deref(), Some("2"));
}
