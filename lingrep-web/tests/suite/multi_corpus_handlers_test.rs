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
//! Workbench-scoped handlers must answer from the workbench's OWN corpus.
//!
//! Two corpora are declared: the default `a` and the declared `b`. Each holds
//! one sentence of two tokens — the one its pattern matches (`A` / `B`), plus a
//! marker token unique to that corpus (see [`marker`]). Every assertion here is
//! about a workbench admitted against `b`, i.e. the NON-default slot — which is
//! exactly what a process-wide runtime lookup gets wrong.
//!
//! Assertions are on rendered CONTENT, not just status. A wrong-corpus answer
//! is a `200` whenever the two corpora's identities happen to agree, so a status
//! assertion alone would not discriminate.
//!
//! Corruption is reused from `inspect_corruption_teardown_test`: overwrite the
//! first byte of block 0's payload in `tokens.v1.zst`, which V1 detects on the
//! next decode because it validates the mapped block checksum every time.

use crate::v6_support::RevisionTestExt;
use std::collections::HashMap;
use std::io::Cursor;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::{body::Body, http::Request, http::StatusCode, response::Response};
use calamine::{Reader, Xlsx};
use http_body_util::BodyExt;
use indexmap::IndexMap;
use lingrep_core::token_match::TokenInfo;
use lingrep_web::corpus_runtime::{CacheReadiness, CorpusRuntime};
use lingrep_web::corpus_set::{CorpusName, CorpusSet, CorpusSlot};
use lingrep_web::revision::{RevisionId, WorkbenchId};
use lingrep_web::server_context::ServerContext;
use lingrep_web::state::{Config, DetailLimits, RequestStore};
use tower::ServiceExt;

use crate::v6_support::{ArtifactFixture, ready_fixture};

/// Byte offset of block 0's payload in `tokens.v1.zst`, per
/// `inspect_corruption_teardown_test`.
/// The token that appears in corpus `key`'s only sentence and in no other
/// corpus. Deliberately not `A`/`B`: a single ASCII letter occurs all over
/// rendered HTML, so it could not distinguish a body.
fn marker(key: &str) -> &'static str {
    match key {
        "a" => "甲",
        _ => "乙",
    }
}

struct TwoCorpusServer {
    _fixtures: Vec<ArtifactFixture>,
    store: RequestStore,
    cache_paths: HashMap<&'static str, PathBuf>,
}

/// A server declaring two corpora with distinguishable content: the default
/// slot `a` matches `[text=A]` and the declared slot `b` matches `[text=B]`.
async fn two_corpus_server() -> TwoCorpusServer {
    let context = Arc::new(ServerContext::new("/tmp/dict").unwrap());
    let mut fixtures = Vec::new();
    let mut cache_paths = HashMap::new();
    let mut slots = IndexMap::new();
    let mut declared = Vec::new();

    for (key, orth) in [("a", "A"), ("b", "B")] {
        // `a` occupies the reserved `default` key (it is the `--input` corpus);
        // `b` is an operator-declared `--corpus b=PATH`.
        let name = if key == "a" {
            CorpusName::default_key()
        } else {
            CorpusName::parse_declared(key).unwrap()
        };
        // Two tokens: the matched one, plus a marker that appears ONLY in this
        // corpus. The marker is what a rendered body is asserted on — it makes
        // "answered from corpus b" observable, where a status code alone would
        // pass just as well if the wrong corpus happened to return 200.
        let (fixture, ready) = ready_fixture(
            &format!("/private/corpus-{key}.lingcorpus"),
            vec![vec![
                TokenInfo::default().with_orth(marker(key)).with_pos("NOUN"),
                TokenInfo::default().with_orth(orth).with_pos("NOUN"),
            ]],
        );
        cache_paths.insert(key, fixture.path.join("tokens.v1.zst"));
        declared.push((name.clone(), fixture.path.clone(), format!("corpus-{key}")));
        let runtime = Arc::new(CorpusRuntime::from_ready(
            ready.identity,
            Arc::into_inner(ready.artifact).expect("fixture has one artifact owner"),
            Arc::clone(&context),
        ));
        slots.insert(
            name.clone(),
            CorpusSlot::new(name, runtime, format!("corpus-{key}")),
        );
        fixtures.push(fixture);
    }

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

    TwoCorpusServer {
        _fixtures: fixtures,
        store,
        cache_paths,
    }
}

/// Corpus `a` is the `--input` corpus and so carries the reserved `default`
/// protocol key; every other corpus is keyed by its declared name.
fn protocol_key(key: &str) -> &str {
    if key == "a" { CorpusName::DEFAULT } else { key }
}

impl TwoCorpusServer {
    fn runtime(&self, key: &str) -> Arc<CorpusRuntime> {
        Arc::clone(
            &self
                .store
                .corpora
                .resolve(Some(protocol_key(key)))
                .expect("declared corpus")
                .runtime,
        )
    }

    /// Admit a workbench against corpus `key` and drive one revision of
    /// `pattern` to a displayed snapshot. Returns the workbench id.
    async fn run_query(&self, key: &str, pattern: &str) -> String {
        let workbench = WorkbenchId::new(format!("wb-{key}"));
        let slot = self
            .store
            .corpora
            .resolve(Some(protocol_key(key)))
            .expect("declared corpus");
        let revision = self
            .store
            .coordinators
            .accept_next_revision(workbench.clone(), slot, pattern.into())
            .await;
        assert_eq!(revision, RevisionId(1));
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if self
                    .store
                    .coordinators
                    .snapshot(&workbench)
                    .await
                    .and_then(|view| {
                        view.state
                            .displayed_snapshot()
                            .map(|snapshot| snapshot.revision)
                    })
                    == Some(revision)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        workbench.as_str().to_owned()
    }

    async fn get(&self, path: &str) -> Response {
        lingrep_web::app_router(self.store.clone())
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    /// POST the search form for `workbench`, selecting corpus `key`.
    ///
    /// `key` is written straight onto the wire without validation: the hidden
    /// field is client-supplied, so an undeclared or malformed value is exactly
    /// what this must be able to send.
    async fn post_search(&self, workbench: &str, key: &str, pattern: &str) -> String {
        let body = serde_urlencoded::to_string([("pattern", pattern), ("corpus", key)]).unwrap();
        let response = lingrep_web::app_router(self.store.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/search/{workbench}"))
                    .header(
                        axum::http::header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded",
                    )
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    async fn get_body(&self, path: &str) -> (StatusCode, Vec<u8>) {
        let response = self.get(path).await;
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, bytes)
    }

    /// The rendered pattern-detail fragment for corpus `key`'s workbench.
    async fn detail_html(&self, key: &str) -> (StatusCode, String) {
        let (status, bytes) = self
            .get_body(&format!("/api/pattern-detail/wb-{key}/1/0"))
            .await;
        (status, String::from_utf8(bytes).unwrap())
    }

    /// The export workbook's About sheet, flattened to its cell strings.
    async fn export_about(&self, key: &str) -> (StatusCode, Vec<String>) {
        let (status, bytes) = self.get_body(&format!("/api/export/wb-{key}/1")).await;
        let mut workbook = Xlsx::new(Cursor::new(bytes)).unwrap();
        let about = workbook.worksheet_range("About").unwrap();
        let cells = about
            .rows()
            .flat_map(|row| row.iter())
            .map(ToString::to_string)
            .collect();
        (status, cells)
    }

    /// Overwrite the first byte of block 0's payload so the next decode of
    /// sentence 0 in THIS corpus fails validation.
    async fn corrupt_corpus(&self, key: &str) {
        crate::v6_support::corrupt_first_token_block(&self.cache_paths[key]);
    }

    /// Drive a decode of corpus `key` through the pattern-detail handler, which
    /// is the caller that owns the at-most-once corruption teardown.
    async fn trigger_decode(&self, key: &str) -> StatusCode {
        self.get(&format!("/api/pattern-detail/wb-{key}/1/0"))
            .await
            .status()
    }

    async fn is_failed(&self, key: &str) -> bool {
        let runtime = self.runtime(key);
        let failed = matches!(*runtime.readiness().borrow(), CacheReadiness::Failed { .. });
        failed && runtime.ready().await.is_none()
    }

    async fn is_ready(&self, key: &str) -> bool {
        self.runtime(key).ready().await.is_some()
    }
}

/// The regression a process-wide runtime lookup produces: every detail request
/// for a non-default corpus 409s, because SnapshotInspector::bind compares the
/// snapshot's identity against the reader it was handed.
///
/// A status assertion alone would not be discriminating — it would pass just as
/// well if the handler resolved the wrong corpus and happened to answer `200`.
/// So the rendered KWIC is asserted to carry corpus B's marker token and NOT
/// corpus A's.
#[tokio::test]
async fn detail_and_export_resolve_the_workbench_s_own_corpus() {
    let harness = two_corpus_server().await;
    harness.run_query("b", "b := [text=B].").await;

    let (status, html) = harness.detail_html("b").await;
    assert_eq!(status, StatusCode::OK, "non-default corpus must not 409");
    assert!(
        html.contains(marker("b")),
        "the detail is projected from corpus b's sentence: {html}"
    );
    assert!(
        !html.contains(marker("a")),
        "no token of the default corpus may appear: {html}"
    );

    let (status, about) = harness.export_about("b").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        about.iter().any(|cell| cell == "corpus-b"),
        "the About sheet names the workbench's own corpus: {about:?}"
    );
    assert!(
        !about.iter().any(|cell| cell == "corpus-a"),
        "the default corpus's display name must not be published: {about:?}"
    );
}

/// The search form's corpus key comes from the client, so it can name a corpus
/// this server does not declare. `CorpusSet::resolve` is two-valued and returns
/// `None` for both a malformed and an undeclared key, so the handler must answer
/// the miss rather than unwrap it.
///
/// Both halves matter. The alert alone would pass even if the handler had
/// admitted against the DEFAULT corpus and then failed for an unrelated reason,
/// so the workbench is asserted to be absent: admission is where a client's key
/// becomes durable state, and a silent fallback there would bind the workbench to
/// the wrong corpus for the rest of its life.
///
/// This is the test B4 could not write: `resolve(None)` is total for a validated
/// configuration, so the `None` branch first became reachable when the search
/// form started carrying `k`.
#[tokio::test]
async fn an_undeclared_corpus_key_is_refused_before_any_workbench_is_admitted() {
    let harness = two_corpus_server().await;
    let workbench = WorkbenchId::new("wb-undeclared");

    let html = harness
        .post_search(workbench.as_str(), "no-such-corpus", "p := [text=A].")
        .await;

    assert!(
        html.contains("data-i18n=\"corpus.unavailable\""),
        "an undeclared key is answered as an unavailable corpus: {html}"
    );
    assert!(
        !html.contains("Server busy"),
        "a lookup miss is not capacity exhaustion: {html}"
    );
    assert!(
        harness
            .store
            .coordinators
            .snapshot(&workbench)
            .await
            .is_none(),
        "an unresolvable key must not admit a workbench against the default corpus"
    );
}

/// A workbench's corpus is fixed at admission (B3), so a request naming another
/// corpus is refused. Before the form carried a key that refusal was unreachable
/// and every `Err` was rendered as capacity exhaustion; now it is reachable, and
/// "Server busy: too many active sessions" would be a wrong diagnosis of a
/// mismatch the user can actually fix.
#[tokio::test]
async fn a_bound_workbench_reports_a_corpus_conflict_rather_than_capacity() {
    let harness = two_corpus_server().await;
    harness.run_query("b", "b := [text=B].").await;

    let html = harness
        .post_search("wb-b", CorpusName::DEFAULT, "a := [text=A].")
        .await;

    assert!(
        html.contains("data-i18n=\"corpus.conflict\""),
        "the mismatch is reported as a corpus conflict: {html}"
    );
    assert!(
        !html.contains("Server busy"),
        "a corpus conflict is not capacity exhaustion: {html}"
    );
}

/// Corruption is per corpus. decode_refs currently invalidates the process
/// runtime with no identity check, which would flip an unrelated corpus.
#[tokio::test]
async fn corrupting_one_corpus_leaves_the_others_ready() {
    let harness = two_corpus_server().await;
    harness.run_query("b", "b := [text=B].").await;

    harness.corrupt_corpus("b").await;
    assert_eq!(
        harness.trigger_decode("b").await,
        StatusCode::INTERNAL_SERVER_ERROR,
        "the corrupt decode is reported as a damaged cache"
    );

    assert!(harness.is_failed("b").await, "corpus b must be torn down");
    assert!(harness.is_ready("a").await, "corpus a must be unaffected");
}

/// The corpus-context poll swaps the WHOLE section, selector included, so the
/// partial must carry the options too — otherwise the first poll after landing
/// would swap the selector away, and a page whose corpus is still preparing is
/// exactly the page a researcher most wants to switch from.
#[tokio::test]
async fn the_polled_context_partial_keeps_the_selector_marked_for_the_polled_corpus() {
    let harness = two_corpus_server().await;

    let (status, bytes) = harness.get_body("/api/corpus-context?k=b").await;
    assert_eq!(status, StatusCode::OK);
    let html = String::from_utf8(bytes).unwrap();

    assert!(html.contains("data-corpus-select"), "{html}");
    assert!(html.contains(r#"value="default""#), "{html}");
    assert!(
        html.contains(r#"value="b" selected"#),
        "the polled corpus is the selected one: {html}"
    );
    assert!(
        !html.contains("/private/corpus-"),
        "the corpus path must never reach the browser: {html}"
    );
    // The other half of the swap contract: the switch's live region is NOT in
    // this payload, so a poll cannot erase a message it just reported. A region
    // inside here would be wiped every few seconds while a corpus prepares —
    // exactly when a researcher is most likely to try switching away.
    assert!(
        !html.contains("data-corpus-select-status"),
        "the live region must live outside the swapped section: {html}"
    );
}
