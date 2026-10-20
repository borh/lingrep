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
use std::num::NonZeroUsize;
use std::sync::Arc;

use axum::{Router, body::Body, http::Request};
use http_body_util::BodyExt;
use lingrep_core::token_match::TokenInfo;
use lingrep_web::coordinator::{AdmitError, CoordinatorRegistry};
use lingrep_web::corpus_runtime::CorpusRuntime;
use lingrep_web::corpus_set::{CorpusName, CorpusSlot};
use lingrep_web::revision::AttemptPhase;
use lingrep_web::revision::{ErrorCategory, RevisionId, WorkbenchId};
use lingrep_web::state::{Config, DetailLimits, RequestStore};
use tempfile::tempdir;
use tower::ServiceExt;

use crate::v6_support::{default_slot, runtime_fixture};

fn ready_registry() -> (
    tempfile::TempDir,
    Arc<CorpusRuntime>,
    CorpusSlot,
    CoordinatorRegistry,
) {
    let sentence = vec![
        TokenInfo::default()
            .with_orth("食べる")
            .with_lemma("食べる")
            .with_pos("VERB"),
    ];
    let (fixture, runtime) = runtime_fixture("fixture", vec![sentence; 100]);
    let slot = default_slot(Arc::clone(&runtime));
    let registry = CoordinatorRegistry::new(5);
    (fixture.directory, runtime, slot, registry)
}

async fn run_to_terminal(
    registry: &CoordinatorRegistry,
    slot: &CorpusSlot,
    wb: &WorkbenchId,
    src: &str,
) {
    registry
        .accept_next_revision(wb.clone(), slot, src.into())
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if registry
                .snapshot(wb)
                .await
                .is_some_and(|v| v.state.is_terminal())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn wait_for_completed_snapshot(
    registry: &CoordinatorRegistry,
    workbench: &WorkbenchId,
    revision: RevisionId,
) -> Arc<lingrep_web::snapshot::CompletedSnapshot> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let view = registry.snapshot(workbench).await.unwrap();
            if view.state.current_attempt().unwrap().phase == AttemptPhase::Completed {
                break registry
                    .displayed_snapshot(workbench, revision)
                    .await
                    .expect("completed revision must be displayed");
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("coordinator must complete within five seconds")
}

#[tokio::test]
async fn subscribed_entry_is_pinned_and_new_id_is_rejected_when_full() {
    let (_dir, _runtime, slot, _r) = ready_registry();
    let registry = CoordinatorRegistry::with_max_entries(5, NonZeroUsize::new(1).unwrap());
    let a = WorkbenchId::new("a");
    run_to_terminal(&registry, &slot, &a, "verb := [upos=VERB].").await;

    let _sub = registry.subscribe(&a).await.unwrap(); // pins `a` even though terminal
    let rejected = registry
        .try_accept_next_revision(
            WorkbenchId::new("b"),
            &slot,
            "verb := [upos=VERB].".into(),
            None,
        )
        .await;
    assert!(
        rejected.is_err(),
        "cap full of a pinned entry rejects a new id"
    );
    assert!(
        registry.snapshot(&a).await.is_some(),
        "pinned entry survived"
    );
}

#[tokio::test]
async fn terminal_unsubscribed_entry_is_evicted_for_a_new_id() {
    let (_dir, _runtime, slot, _r) = ready_registry();
    let registry = CoordinatorRegistry::with_max_entries(5, NonZeroUsize::new(1).unwrap());
    let a = WorkbenchId::new("a");
    run_to_terminal(&registry, &slot, &a, "verb := [upos=VERB].").await;
    // no subscriber held → a is terminal + unsubscribed + finished

    assert!(
        registry
            .try_accept_next_revision(
                WorkbenchId::new("b"),
                &slot,
                "verb := [upos=VERB].".into(),
                None
            )
            .await
            .is_ok(),
        "evicts the terminal, unsubscribed entry to admit a new id"
    );
    assert!(
        registry.snapshot(&a).await.is_none(),
        "evicted entry is gone"
    );
}

#[tokio::test]
async fn coordinator_allocates_monotonic_revisions() {
    let (_directory, _runtime, slot, registry) = ready_registry();
    let workbench = WorkbenchId::new("atomic-next");
    let first = registry
        .accept_next_revision(workbench.clone(), &slot, "first := [upos=NOUN].".into())
        .await;
    let second = registry
        .accept_next_revision(workbench, &slot, "second := [upos=VERB].".into())
        .await;

    assert_eq!((first, second), (RevisionId(1), RevisionId(2)));
}

#[tokio::test]
async fn concurrent_acceptance_never_reuses_a_revision() {
    let (_directory, _runtime, slot, registry) = ready_registry();
    let workbench = WorkbenchId::new("atomic-concurrent");
    let (left, right) = tokio::join!(
        registry.accept_next_revision(workbench.clone(), &slot, "left := [upos=NOUN].".into()),
        registry.accept_next_revision(workbench, &slot, "right := [upos=VERB].".into()),
    );
    let mut ids = [left.0, right.0];
    ids.sort_unstable();

    assert_eq!(ids, [1, 2]);
}

#[tokio::test]
async fn compile_failure_retains_sanitized_diagnostic() {
    const SECRET: &str = "COORDINATOR_SECRET_71D2";
    let (_directory, _runtime, slot, registry) = ready_registry();
    let workbench = WorkbenchId::new("compile-detail");
    let source = format!("broken := [text~/{SECRET}/.");
    registry
        .accept_next_revision(workbench.clone(), &slot, source.clone())
        .await;

    let view = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let view = registry.snapshot(&workbench).await.unwrap();
            if view.state.current_attempt().unwrap().phase == AttemptPhase::Failed {
                break view;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let failure = view
        .state
        .current_attempt()
        .unwrap()
        .failure
        .as_ref()
        .unwrap();

    assert_eq!(failure.category, ErrorCategory::Compile);
    assert!(failure.message.contains("lingrep-web.lx:1:"));
    assert!(!failure.message.contains(SECRET), "{}", failure.message);
    assert!(!failure.message.contains(&source), "{}", failure.message);
}

#[tokio::test]
async fn newer_revision_replaces_waiting_revision() {
    let directory = tempdir().unwrap();
    let context = Arc::new(lingrep_web::server_context::ServerContext::new("/tmp/dict").unwrap());
    let runtime =
        Arc::new(CorpusRuntime::new(context, directory.path().join("missing-corpus")).unwrap());
    let slot = default_slot(runtime);
    let registry = CoordinatorRegistry::new(10);
    let workbench = WorkbenchId::new("one");
    assert_eq!(
        registry
            .accept_next_revision(workbench.clone(), &slot, "a := [upos=NOUN].".into())
            .await,
        RevisionId(1)
    );
    assert_eq!(
        registry
            .accept_next_revision(workbench.clone(), &slot, "b := [upos=VERB].".into())
            .await,
        RevisionId(2)
    );
    assert_eq!(
        registry
            .snapshot(&workbench)
            .await
            .unwrap()
            .state
            .current_attempt()
            .unwrap()
            .revision,
        RevisionId(2)
    );
}

#[tokio::test]
async fn failed_runtime_terminates_waiting_revision() {
    let directory = tempdir().unwrap();
    let context = Arc::new(lingrep_web::server_context::ServerContext::new("/tmp/dict").unwrap());
    let runtime =
        Arc::new(CorpusRuntime::new(context, directory.path().join("missing-corpus")).unwrap());
    runtime.start_or_observe().await;
    let slot = default_slot(runtime);
    let registry = CoordinatorRegistry::new(10);
    let workbench = WorkbenchId::new("failed-runtime");
    registry
        .accept_next_revision(workbench.clone(), &slot, "verb := [upos=VERB].".into())
        .await;

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let phase = registry
                .snapshot(&workbench)
                .await
                .unwrap()
                .state
                .current_attempt()
                .unwrap()
                .phase;
            if phase == AttemptPhase::Failed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn workbenches_have_independent_latest_slots() {
    let directory = tempdir().unwrap();
    let context = Arc::new(lingrep_web::server_context::ServerContext::new("/tmp/dict").unwrap());
    let runtime =
        Arc::new(CorpusRuntime::new(context, directory.path().join("missing-corpus")).unwrap());
    let slot = default_slot(runtime);
    let registry = CoordinatorRegistry::new(10);
    for id in ["a", "b"] {
        assert_eq!(
            registry
                .accept_next_revision(WorkbenchId::new(id), &slot, "p := [upos=NOUN].".into())
                .await,
            RevisionId(1)
        );
    }
    assert!(registry.snapshot(&WorkbenchId::new("a")).await.is_some());
    assert!(registry.snapshot(&WorkbenchId::new("b")).await.is_some());
}

#[tokio::test]
async fn latest_completed_revision_is_the_only_published_snapshot() {
    let (_directory, _runtime, slot, registry) = ready_registry();
    let workbench = WorkbenchId::new("latest");
    registry
        .accept_next_revision(workbench.clone(), &slot, "first := [text=食べる].".into())
        .await;
    let revision = registry
        .accept_next_revision(workbench.clone(), &slot, "second := [text=食べる].".into())
        .await;
    wait_for_completed_snapshot(&registry, &workbench, revision).await;
    let view = registry.snapshot(&workbench).await.unwrap();
    let snapshot = view.state.displayed_snapshot().unwrap();
    assert_eq!(snapshot.revision, RevisionId(2));
    assert_eq!(snapshot.patterns[0].name, "second");
    assert_eq!(snapshot.patterns[0].exact_total, 100);
    assert_eq!(snapshot.match_refs.len(), 5);
}

#[tokio::test]
async fn coordinator_publishes_ordered_snapshot_from_parallel_cache_blocks() {
    let (_fixture, runtime) = runtime_fixture(
        "parallel-fixture",
        (0..12)
            .map(|_| vec![TokenInfo::default().with_orth("食べる").with_pos("VERB")])
            .collect(),
    );
    let slot = default_slot(runtime);
    let registry = CoordinatorRegistry::new(12);
    let workbench = WorkbenchId::new("parallel-web");
    let revision = registry
        .accept_next_revision(workbench.clone(), &slot, "verb := [upos=VERB].".into())
        .await;

    let snapshot = wait_for_completed_snapshot(&registry, &workbench, revision).await;
    assert_eq!(snapshot.patterns[0].exact_total, 12);
    assert_eq!(
        snapshot
            .match_refs
            .iter()
            .map(|reference| reference.sentence_index)
            .collect::<Vec<_>>(),
        (0..12).collect::<Vec<_>>()
    );
    registry.shutdown().await;
}

#[tokio::test]
async fn completed_snapshot_resolves_surface_and_kwic_lazily() {
    let (_directory, runtime, _slot, _registry) = ready_registry();
    let store = RequestStore::with_runtime(
        Config {
            dict_path: String::new(),
            corpora: Vec::new(),
            input_format: None,
            markdown: lingrep_core::corpus::MarkdownReaderConfig::new(
                lingrep_core::corpus::SectionPolicy::default(),
            ),
            max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
            detail_limits: DetailLimits { max_kwic: 5 },
        },
        runtime,
    );
    let workbench = WorkbenchId::new("details");
    let slot = store
        .corpora
        .resolve(None)
        .expect("a validated configuration declares a default corpus");
    store
        .coordinators
        .accept_next_revision(workbench.clone(), slot, "eating := [text=食べる].".into())
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if store
                .coordinators
                .snapshot(&workbench)
                .await
                .unwrap()
                .state
                .displayed_snapshot()
                .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let app = Router::new()
        .merge(lingrep_web::handlers::pattern_detail::routes())
        .with_state(store);
    for path in ["/pattern-detail/details/1/0"] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(response.status().is_success());
        let html = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("食べる"), "{path}: {html}");
    }
}

/// Two declared corpora sharing one registry, the shape B2 introduced and B3
/// makes admission aware of.
struct TwoCorpusHarness {
    _dir_a: tempfile::TempDir,
    _dir_b: tempfile::TempDir,
    registry: CoordinatorRegistry,
    slot_a: CorpusSlot,
    slot_b: CorpusSlot,
}

impl TwoCorpusHarness {
    fn slot_a(&self) -> &CorpusSlot {
        &self.slot_a
    }

    fn slot_b(&self) -> &CorpusSlot {
        &self.slot_b
    }
}

async fn two_corpus_registry() -> TwoCorpusHarness {
    two_corpus_registry_with_capacity(256).await
}

async fn two_corpus_registry_with_capacity(max_entries: usize) -> TwoCorpusHarness {
    let max_entries = NonZeroUsize::new(max_entries).expect("capacity must be nonzero");
    let sentence = vec![TokenInfo::default().with_orth("食べる").with_pos("VERB")];
    let (fixture_a, ready_a) = crate::v6_support::ready_fixture("corpus-a", vec![sentence.clone()]);
    let (fixture_b, ready_b) = crate::v6_support::ready_fixture("corpus-b", vec![sentence]);
    let context = Arc::new(lingrep_web::server_context::ServerContext::new("/tmp/dict").unwrap());
    let runtime_a = Arc::new(CorpusRuntime::from_ready(
        ready_a.identity,
        Arc::into_inner(ready_a.artifact).expect("fixture has one artifact owner"),
        Arc::clone(&context),
    ));
    let runtime_b = Arc::new(CorpusRuntime::from_ready(
        ready_b.identity,
        Arc::into_inner(ready_b.artifact).expect("fixture has one artifact owner"),
        context,
    ));
    let slot_a = CorpusSlot::new(
        CorpusName::parse_declared("corpus-a").unwrap(),
        runtime_a,
        "Corpus A".into(),
    );
    let slot_b = CorpusSlot::new(
        CorpusName::parse_declared("corpus-b").unwrap(),
        runtime_b,
        "Corpus B".into(),
    );
    let registry = CoordinatorRegistry::with_max_entries(5, max_entries);
    TwoCorpusHarness {
        _dir_a: fixture_a.directory,
        _dir_b: fixture_b.directory,
        registry,
        slot_a,
        slot_b,
    }
}

/// A workbench's corpus is fixed at admission. The id arrives in a
/// client-controlled path, so this must be enforced rather than assumed from
/// the fact that navigation mints a fresh id.
#[tokio::test]
async fn readmitting_a_workbench_under_a_different_corpus_is_a_conflict() {
    let harness = two_corpus_registry().await;
    let workbench = WorkbenchId::random();

    harness
        .registry
        .try_accept_next_revision(workbench.clone(), harness.slot_a(), "p := [].".into(), None)
        .await
        .expect("first admission succeeds");

    let conflict = harness
        .registry
        .try_accept_next_revision(workbench.clone(), harness.slot_b(), "p := [].".into(), None)
        .await;

    assert!(matches!(conflict, Err(AdmitError::CorpusConflict)));
}

#[tokio::test]
async fn readmitting_under_the_same_corpus_still_succeeds() {
    let harness = two_corpus_registry().await;
    let workbench = WorkbenchId::random();
    for _ in 0..2 {
        harness
            .registry
            .try_accept_next_revision(workbench.clone(), harness.slot_a(), "p := [].".into(), None)
            .await
            .expect("same-corpus readmission is ordinary");
    }
}

/// The global cap and its LRU are shared across corpora — the reason the
/// registry stays singular.
#[tokio::test]
async fn the_entry_cap_is_global_across_corpora() {
    let harness = two_corpus_registry_with_capacity(1).await;
    let first = WorkbenchId::random();
    harness
        .registry
        .try_accept_next_revision(first.clone(), harness.slot_a(), "p := [].".into(), None)
        .await
        .unwrap();

    // Hold a live subscription on `first` so its entry stays subscribed (and
    // therefore non-evictable) across the second admission below. Without
    // this, the first revision's background scan can reach terminal before
    // the second admission takes the entries lock, making the first entry
    // evictable and the second admission succeed — flaking the assertion for
    // a timing reason that has nothing to do with corpora.
    let _sub = harness.registry.subscribe(&first).await.unwrap();

    // A different corpus does not get its own capacity.
    let second = WorkbenchId::random();
    let result = harness
        .registry
        .try_accept_next_revision(second, harness.slot_b(), "p := [].".into(), None)
        .await;
    assert!(matches!(result, Err(AdmitError::RegistryFull)));
}
