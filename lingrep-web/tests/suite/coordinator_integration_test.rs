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
use crate::v6_support::RevisionTestExt;
use std::num::NonZeroUsize;
use std::sync::Arc;

use axum::{Router, body::Body, http::Request};
use http_body_util::BodyExt;
use lingrep_core::token_match::TokenInfo;
use lingrep_web::coordinator::{AdmitError, CoordinatorRegistry};
use lingrep_web::corpus_runtime::CorpusRuntime;
use lingrep_web::corpus_set::{CorpusName, CorpusSlot};
use lingrep_web::revision::{AttemptPhase, CancelReason};
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
        .accept_revision(
            WorkbenchId::new("b"),
            &slot,
            lingrep_web::coordinator::RevisionRequest {
                source: "verb := [upos=VERB].".into(),
                retained: None,
                ..Default::default()
            },
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
            .accept_revision(
                WorkbenchId::new("b"),
                &slot,
                lingrep_web::coordinator::RevisionRequest {
                    source: "verb := [upos=VERB].".into(),
                    retained: None,
                    ..Default::default()
                }
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
async fn a_dependency_where_clause_over_a_schema_without_edges_is_reported_as_a_binding_failure() {
    let (_directory, runtime, slot, registry) = ready_registry();
    let workbench = WorkbenchId::new("binding-failure");
    registry
        .accept_next_revision(
            workbench.clone(),
            &slot,
            include_str!("../../../patterns/double-wo-dep.lx").into(),
        )
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

    assert_eq!(
        view.state.current_attempt().unwrap().failure,
        Some(lingrep_web::revision::AttemptFailure {
            category: ErrorCategory::Binding,
            message: "The catalog cannot bind to this corpus's token schema (vibrato morphology): \
                      dependency `where` clauses require dep.head, dep.rel, which the token schema \
                      does not carry."
                .into(),
        })
    );
    assert!(
        matches!(
            *runtime.readiness().borrow(),
            lingrep_web::corpus_runtime::CacheReadiness::Ready { .. }
        ),
        "a rejected catalog leaves the corpus ready"
    );
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

struct HeldPool {
    run_one: std::sync::mpsc::Sender<()>,
    ran: std::sync::mpsc::Receiver<()>,
}

impl HeldPool {
    async fn run_one_queued_job(&self) {
        self.run_one.send(()).unwrap();
        let deadline = std::time::Instant::now() + WAIT;
        while self.ran.try_recv().is_err() {
            assert!(
                std::time::Instant::now() < deadline,
                "no job was queued on the held pool"
            );
            tokio::task::yield_now().await;
        }
    }
}

const WAIT: std::time::Duration = std::time::Duration::from_secs(10);

fn occupy(pool: &rayon::ThreadPool) -> HeldPool {
    let threads = pool.current_num_threads();
    let (run_one, commands) = std::sync::mpsc::channel::<()>();
    let commands = Arc::new(std::sync::Mutex::new(commands));
    let (ran_tx, ran) = std::sync::mpsc::channel::<()>();
    let started = Arc::new(std::sync::Barrier::new(threads + 1));
    for _ in 0..threads {
        let commands = Arc::clone(&commands);
        let ran_tx = ran_tx.clone();
        let started = Arc::clone(&started);
        pool.spawn(move || {
            started.wait();
            loop {
                let command = commands.lock().unwrap().recv();
                if command.is_err() {
                    break;
                }
                let deadline = std::time::Instant::now() + WAIT;
                while rayon::yield_now() != Some(rayon::Yield::Executed) {
                    if std::time::Instant::now() >= deadline {
                        return;
                    }
                    std::thread::yield_now();
                }
                let _ = ran_tx.send(());
            }
        });
    }
    started.wait();
    HeldPool { run_one, ran }
}

async fn phase_of(registry: &CoordinatorRegistry, workbench: &WorkbenchId) -> AttemptPhase {
    let view = registry.snapshot(workbench).await.unwrap();
    view.state.current_attempt().unwrap().phase
}

async fn run_ready_tasks() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

fn multi_batch_registry() -> (
    tempfile::TempDir,
    Arc<CorpusRuntime>,
    CorpusSlot,
    CoordinatorRegistry,
) {
    let sentence = vec![TokenInfo::default().with_orth("食べる").with_pos("VERB")];
    let (fixture, runtime) = runtime_fixture("multi-batch", vec![sentence; 8 * 1024]);
    let slot = default_slot(Arc::clone(&runtime));
    (
        fixture.directory,
        runtime,
        slot,
        CoordinatorRegistry::new(5),
    )
}

#[tokio::test]
async fn a_superseding_revision_starts_scanning_only_after_the_superseded_scan_ends() {
    let (_directory, runtime, slot, registry) = multi_batch_registry();
    let held = occupy(&runtime.context().pool());
    let workbench = WorkbenchId::new("superseded-scan");
    registry
        .accept_next_revision(workbench.clone(), &slot, "first := [text=食べる].".into())
        .await;
    let mut views = registry.subscribe(&workbench).await.unwrap();
    while views.borrow_and_update().live_summary.is_none() {
        held.run_one_queued_job().await;
        run_ready_tasks().await;
    }

    let second = registry
        .accept_next_revision(workbench.clone(), &slot, "second := [text=食べる].".into())
        .await;
    views.borrow_and_update();
    views.changed().await.unwrap();
    run_ready_tasks().await;
    assert_eq!(phase_of(&registry, &workbench).await, AttemptPhase::Queued);

    drop(held);
    let snapshot = wait_for_completed_snapshot(&registry, &workbench, second).await;
    assert_eq!(snapshot.patterns[0].name, "second");
}

async fn wait_for_phase(
    registry: &CoordinatorRegistry,
    workbench: &WorkbenchId,
    reached: impl Fn(AttemptPhase) -> bool,
) -> AttemptPhase {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let phase = phase_of(registry, workbench).await;
        if reached(phase) {
            return phase;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the revision stayed {phase:?}"
        );
        tokio::task::yield_now().await;
    }
}

fn is_terminal(phase: AttemptPhase) -> bool {
    matches!(
        phase,
        AttemptPhase::Completed | AttemptPhase::Cancelled(_) | AttemptPhase::Failed
    )
}

async fn advance(duration: std::time::Duration) {
    let mut remaining = duration;
    while !remaining.is_zero() {
        let step = remaining.min(std::time::Duration::from_secs(1));
        tokio::time::advance(step).await;
        run_ready_tasks().await;
        remaining -= step;
    }
}

const GRACE_ELAPSED: std::time::Duration = std::time::Duration::from_millis(30_001);
const WITHIN_GRACE: std::time::Duration = std::time::Duration::from_millis(29_999);

// Pause Tokio time only after the blocking scan starts; SQLx pool acquisition
// uses Tokio timeouts (sqlx::pool::PoolOptions::acquire_timeout).

#[tokio::test]
async fn a_running_scan_without_a_subscriber_for_30_seconds_is_cancelled_and_frees_the_permit() {
    let (_directory, runtime, slot, registry) = multi_batch_registry();
    let held = occupy(&runtime.context().pool());
    let closed = WorkbenchId::new("closed-page");
    registry
        .accept_next_revision(closed.clone(), &slot, "first := [text=食べる].".into())
        .await;
    let page = registry.subscribe(&closed).await.unwrap();
    held.run_one_queued_job().await;
    let waiting = WorkbenchId::new("waiting");
    let second = registry
        .accept_next_revision(waiting.clone(), &slot, "second := [text=食べる].".into())
        .await;
    let _waiting_page = registry.subscribe(&waiting).await.unwrap();

    tokio::time::pause();
    drop(page);
    run_ready_tasks().await;
    advance(GRACE_ELAPSED).await;
    tokio::time::resume();
    drop(held);

    assert_eq!(
        wait_for_phase(&registry, &closed, is_terminal).await,
        AttemptPhase::Cancelled(CancelReason::Abandoned)
    );
    let snapshot = wait_for_completed_snapshot(&registry, &waiting, second).await;
    assert_eq!(snapshot.patterns[0].name, "second");
}

#[tokio::test]
async fn a_queued_revision_without_a_subscriber_for_30_seconds_is_cancelled_before_it_scans() {
    let (_directory, runtime, slot, registry) = multi_batch_registry();
    let held = occupy(&runtime.context().pool());
    let scanning = WorkbenchId::new("scanning");
    let first = registry
        .accept_next_revision(scanning.clone(), &slot, "first := [text=食べる].".into())
        .await;
    let _page = registry.subscribe(&scanning).await.unwrap();
    held.run_one_queued_job().await;
    tokio::time::pause();
    let closed = WorkbenchId::new("closed-page");
    registry
        .accept_next_revision(closed.clone(), &slot, "second := [text=食べる].".into())
        .await;
    run_ready_tasks().await;

    advance(WITHIN_GRACE).await;
    assert_eq!(phase_of(&registry, &closed).await, AttemptPhase::Queued);
    advance(GRACE_ELAPSED - WITHIN_GRACE).await;
    tokio::time::resume();

    assert_eq!(
        phase_of(&registry, &closed).await,
        AttemptPhase::Cancelled(CancelReason::Abandoned)
    );
    assert_eq!(phase_of(&registry, &scanning).await, AttemptPhase::Running);
    drop(held);
    wait_for_completed_snapshot(&registry, &scanning, first).await;
    let view = registry.snapshot(&closed).await.unwrap();
    assert_eq!(
        view.state.current_attempt().unwrap().phase,
        AttemptPhase::Cancelled(CancelReason::Abandoned)
    );
    assert!(view.state.displayed_snapshot().is_none());
}

#[tokio::test]
async fn a_subscriber_that_returns_within_30_seconds_keeps_the_scan() {
    let (_directory, runtime, slot, registry) = multi_batch_registry();
    let held = occupy(&runtime.context().pool());
    let workbench = WorkbenchId::new("reconnected-page");
    let revision = registry
        .accept_next_revision(workbench.clone(), &slot, "verb := [text=食べる].".into())
        .await;
    let page = registry.subscribe(&workbench).await.unwrap();
    held.run_one_queued_job().await;

    tokio::time::pause();
    drop(page);
    run_ready_tasks().await;
    advance(WITHIN_GRACE).await;
    let _page = registry.subscribe(&workbench).await.unwrap();
    advance(GRACE_ELAPSED).await;
    tokio::time::resume();
    drop(held);

    assert_eq!(
        wait_for_phase(&registry, &workbench, is_terminal).await,
        AttemptPhase::Completed
    );
    assert!(
        registry
            .displayed_snapshot(&workbench, revision)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_closed_event_stream_connection_counts_as_no_subscriber() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (_directory, runtime, _slot, _registry) = multi_batch_registry();
    let held = occupy(&runtime.context().pool());
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
    let slot = store.corpora.resolve(None).unwrap();
    let registry = &store.coordinators;
    let scanning = WorkbenchId::new("scanning");
    registry
        .accept_next_revision(scanning.clone(), slot, "first := [text=食べる].".into())
        .await;
    let _page = registry.subscribe(&scanning).await.unwrap();
    held.run_one_queued_job().await;
    let closed = WorkbenchId::new("closed-page");
    registry
        .accept_next_revision(closed.clone(), slot, "second := [text=食べる].".into())
        .await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .merge(lingrep_web::handlers::stream::routes())
        .with_state(store.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut connection = tokio::net::TcpStream::connect(address).await.unwrap();
    connection
        .write_all(b"GET /stream/closed-page HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut status_line = [0_u8; 15];
    connection.read_exact(&mut status_line).await.unwrap();
    assert_eq!(&status_line, b"HTTP/1.1 200 OK");

    tokio::time::pause();
    advance(2 * GRACE_ELAPSED).await;
    assert_eq!(phase_of(registry, &closed).await, AttemptPhase::Queued);

    drop(connection);
    let deadline = std::time::Instant::now() + WAIT;
    while phase_of(registry, &closed).await != AttemptPhase::Cancelled(CancelReason::Abandoned) {
        assert!(
            std::time::Instant::now() < deadline,
            "the server kept the subscriber of a closed connection"
        );
        advance(GRACE_ELAPSED).await;
    }
    tokio::time::resume();
    server.abort();
    drop(held);
}

#[tokio::test]
async fn shutdown_cancels_a_running_scan() {
    let (_directory, runtime, slot, registry) = ready_registry();
    let held = occupy(&runtime.context().pool());
    let workbench = WorkbenchId::new("shutdown");
    registry
        .accept_next_revision(workbench.clone(), &slot, "verb := [upos=VERB].".into())
        .await;
    let mut views = registry.subscribe(&workbench).await.unwrap();
    views
        .wait_for(|view| view.state.current_attempt().unwrap().phase == AttemptPhase::Running)
        .await
        .unwrap();

    let mut shutdown = std::pin::pin!(registry.shutdown());
    assert!(futures::poll!(shutdown.as_mut()).is_pending());
    drop(held);
    shutdown.await;

    assert_eq!(
        phase_of(&registry, &workbench).await,
        AttemptPhase::Cancelled(CancelReason::Shutdown)
    );
    let view = registry.snapshot(&workbench).await.unwrap();
    assert!(
        lingrep_web::revision_status::project_revision_status(Some(&view))
            .message
            .contains("server")
    );
    assert!(
        registry
            .snapshot(&workbench)
            .await
            .unwrap()
            .state
            .displayed_snapshot()
            .is_none()
    );
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
        .accept_revision(
            workbench.clone(),
            harness.slot_a(),
            lingrep_web::coordinator::RevisionRequest {
                source: "p := [].".into(),
                retained: None,
                ..Default::default()
            },
        )
        .await
        .expect("first admission succeeds");

    let conflict = harness
        .registry
        .accept_revision(
            workbench.clone(),
            harness.slot_b(),
            lingrep_web::coordinator::RevisionRequest {
                source: "p := [].".into(),
                retained: None,
                ..Default::default()
            },
        )
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
            .accept_revision(
                workbench.clone(),
                harness.slot_a(),
                lingrep_web::coordinator::RevisionRequest {
                    source: "p := [].".into(),
                    retained: None,
                    ..Default::default()
                },
            )
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
        .accept_revision(
            first.clone(),
            harness.slot_a(),
            lingrep_web::coordinator::RevisionRequest {
                source: "p := [].".into(),
                retained: None,
                ..Default::default()
            },
        )
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
        .accept_revision(
            second,
            harness.slot_b(),
            lingrep_web::coordinator::RevisionRequest {
                source: "p := [].".into(),
                retained: None,
                ..Default::default()
            },
        )
        .await;
    assert!(matches!(result, Err(AdmitError::RegistryFull)));
}

#[tokio::test]
async fn reconnecting_resets_the_abandonment_grace() {
    let (_directory, runtime, slot, registry) = multi_batch_registry();
    let held = occupy(&runtime.context().pool());
    let scanning = WorkbenchId::new("review-scanning");
    let first = registry
        .accept_next_revision(scanning.clone(), &slot, "first := [].".into())
        .await;
    let _scanning_page = registry.subscribe(&scanning).await.unwrap();
    held.run_one_queued_job().await;
    let target = WorkbenchId::new("review-reconnect");
    registry
        .accept_next_revision(target.clone(), &slot, "second := [].".into())
        .await;
    let page = registry.subscribe(&target).await.unwrap();
    run_ready_tasks().await;
    tokio::time::pause();
    drop(page);
    run_ready_tasks().await;
    advance(std::time::Duration::from_secs(10)).await;
    let reconnected = registry.subscribe(&target).await.unwrap();
    advance(std::time::Duration::from_secs(10)).await;
    drop(reconnected);
    run_ready_tasks().await;
    advance(std::time::Duration::from_millis(10_001)).await;
    let actual = phase_of(&registry, &target).await;
    advance(std::time::Duration::from_secs(20)).await;
    let expired = phase_of(&registry, &target).await;
    tokio::time::resume();
    drop(held);
    wait_for_completed_snapshot(&registry, &scanning, first).await;
    assert_eq!(expired, AttemptPhase::Cancelled(CancelReason::Abandoned));
    assert_eq!(
        actual,
        AttemptPhase::Queued,
        "only 10 seconds since the latest disconnect"
    );
}
