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
//! A request naming a NEVER-STARTED corpus must start it.
//!
//! Only the default slot is started at startup (`main.rs`), and only the landing
//! page starts a selected slot. Every other path that names a corpus — the
//! search POST and the context poll — inherits that as an unstated precondition.
//! A slot neither has reached is `CacheLifecycle::Idle` with readiness
//! `CacheReadiness::Checking`, and nothing will ever send on that watch channel:
//! `wait_for_ready` then awaits a change that cannot arrive, the revision never
//! becomes terminal, and the coordinator entry is never evictable — one of 256
//! sessions burnt for the life of the process. It is reachable without crafting
//! anything: a tab left open on `/?…&k=sci` across a restart posts `corpus=sci`
//! to a process that never started that slot.
//!
//! The cold slot here points at a path that does not exist, so its build fails
//! fast and deterministically without a dictionary. That is deliberate: what is
//! under test is that the slot LEAVES `Idle` at all and that the revision
//! reaches SOME terminal state. A build that hangs and a build that fails are
//! opposite outcomes for availability, and only the second is reachable once the
//! slot is started.
//!
//! Every wait is bounded by `tokio::time::timeout`, so a regression fails in
//! seconds instead of hanging the suite.

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use indexmap::IndexMap;
use lingrep_core::token_match::TokenInfo;
use lingrep_web::corpus_runtime::{CacheReadiness, CorpusRuntime};
use lingrep_web::corpus_set::{CorpusName, CorpusSet, CorpusSlot};
use lingrep_web::revision::WorkbenchId;
use lingrep_web::server_context::ServerContext;
use lingrep_web::state::{Config, DetailLimits, RequestStore};
use tower::ServiceExt;

use crate::v6_support::{ArtifactFixture, ready_fixture};

/// How long a bounded wait may take before the test calls it a hang.
const WAIT: Duration = Duration::from_secs(10);

/// Declared key of the corpus that nothing starts at startup.
const COLD: &str = "cold";

struct ColdServer {
    _fixture: ArtifactFixture,
    _cold_root: tempfile::TempDir,
    store: RequestStore,
}

/// A server declaring two corpora: a ready default, and `cold` — declared,
/// resolvable, and never started by anything.
async fn cold_server() -> ColdServer {
    let context = Arc::new(ServerContext::new("/tmp/dict").unwrap());
    let mut slots = IndexMap::new();
    let mut declared = Vec::new();

    let (fixture, ready) = ready_fixture(
        "/private/corpus-default.lingcorpus",
        vec![vec![TokenInfo::default().with_orth("A").with_pos("NOUN")]],
    );
    let default_name = CorpusName::default_key();
    declared.push((
        default_name.clone(),
        fixture.path.clone(),
        "corpus-default".to_owned(),
    ));
    let default_runtime = Arc::new(CorpusRuntime::from_ready(
        ready.identity,
        Arc::into_inner(ready.artifact).expect("fixture has one artifact owner"),
        Arc::clone(&context),
    ));
    slots.insert(
        default_name.clone(),
        CorpusSlot::new(default_name, default_runtime, "corpus-default".to_owned()),
    );

    // `CorpusRuntime::new` performs no IO, so this slot is constructible and
    // resolvable while its source does not exist — exactly the shape of a
    // declared-but-unbuilt corpus at startup.
    let cold_root = tempfile::tempdir().unwrap();
    let cold_path: PathBuf = cold_root.path().join("never-built.jsonl");
    let cold_name = CorpusName::parse_declared(COLD).unwrap();
    declared.push((
        cold_name.clone(),
        cold_path.clone(),
        "corpus-cold".to_owned(),
    ));
    let cold_runtime = Arc::new(CorpusRuntime::new(Arc::clone(&context), cold_path).unwrap());
    slots.insert(
        cold_name.clone(),
        CorpusSlot::new(cold_name, cold_runtime, "corpus-cold".to_owned()),
    );

    let store = RequestStore::with_corpus_set_capacity(
        Config {
            dict_path: "/unused/unidic-cwj-202512.dic".into(),
            corpora: declared,
            input_format: None,
            markdown: lingrep_core::corpus::MarkdownReaderConfig::new(
                lingrep_core::corpus::SectionPolicy::default(),
            ),
            max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
            detail_limits: DetailLimits { max_kwic: 10 },
        },
        context,
        Arc::new(CorpusSet::new(slots)),
        NonZeroUsize::new(8).unwrap(),
    );

    ColdServer {
        _fixture: fixture,
        _cold_root: cold_root,
        store,
    }
}

impl ColdServer {
    /// The cold slot's current readiness. `Checking` is the never-started state.
    fn cold_readiness(&self) -> CacheReadiness {
        self.store
            .corpora
            .resolve(Some(COLD))
            .expect("declared corpus")
            .runtime
            .readiness()
            .borrow()
            .clone()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_search_naming_a_never_started_corpus_reaches_a_terminal_revision() {
    let server = cold_server().await;
    assert!(
        matches!(server.cold_readiness(), CacheReadiness::Checking),
        "precondition: nothing has started the cold slot"
    );

    let workbench = WorkbenchId::new("wb-cold");
    let body = serde_urlencoded::to_string([("pattern", "[upos=NOUN]"), ("corpus", COLD)]).unwrap();
    let response = tokio::time::timeout(
        WAIT,
        lingrep_web::app_router(server.store.clone()).oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search/wb-cold")
                .header(
                    axum::http::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .body(Body::from(body))
                .unwrap(),
        ),
    )
    .await
    .expect("the search POST itself must not hang")
    .unwrap();
    let rendered = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(
        !rendered.contains("corpus.unavailable"),
        "the cold corpus is declared and must resolve; got {rendered}"
    );

    // The defect: the spawned revision awaits a readiness change that nothing
    // will ever send, so it never becomes terminal and its entry never becomes
    // evictable.
    tokio::time::timeout(WAIT, async {
        loop {
            if server
                .store
                .coordinators
                .snapshot(&workbench)
                .await
                .is_some_and(|view| view.state.is_terminal())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a revision against a declared corpus must reach a terminal state");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_context_poll_naming_a_never_started_corpus_starts_it() {
    let server = cold_server().await;
    assert!(
        matches!(server.cold_readiness(), CacheReadiness::Checking),
        "precondition: nothing has started the cold slot"
    );

    let response = tokio::time::timeout(
        WAIT,
        lingrep_web::app_router(server.store.clone()).oneshot(
            Request::builder()
                .uri("/api/corpus-context?k=cold")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .expect("the context poll itself must not hang")
    .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);

    // `start_or_observe` sends `Building` before it returns, and the handler
    // awaits it — so this is decided by the time the response is, with no race.
    // Without it the poll renders "preparing", promising progress that nothing
    // will ever make, and the browser polls forever.
    assert!(
        !matches!(server.cold_readiness(), CacheReadiness::Checking),
        "polling a declared corpus must leave it started, not Idle/Checking"
    );
}
