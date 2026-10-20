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
//! HTTP contract coverage for shareable permalinks.
//!
//! The wire protocol splits two facts that v1 conflated: `k` **selects** a
//! corpus and `c` **asserts** which content produced the results. `c` is
//! therefore optional — a lazily built corpus has no identity to assert yet, and
//! an absent `c` means "no assertion", not "no corpus".

use std::num::NonZeroUsize;
use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt as _;
use indexmap::IndexMap;
use lingrep_core::token_match::TokenInfo;
use lingrep_web::{
    app_router,
    corpus_context::identity_digest,
    corpus_runtime::{CacheReadiness, CorpusIdentity, CorpusRuntime},
    corpus_set::{CorpusName, CorpusSet, CorpusSlot},
    server_context::ServerContext,
    state::{Config, DetailLimits, RequestStore},
};
use tempfile::{TempDir, tempdir};
use tower::ServiceExt as _;

use crate::v6_support::{ArtifactFixture, ready_fixture};

fn config(dict_path: String, corpora: Vec<(CorpusName, std::path::PathBuf, String)>) -> Config {
    Config {
        dict_path,
        corpora,
        input_format: None,
        markdown: lingrep_core::corpus::MarkdownReaderConfig::new(
            lingrep_core::corpus::SectionPolicy::default(),
        ),
        max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
        detail_limits: DetailLimits { max_kwic: 100 },
    }
}

async fn ready_store() -> (RequestStore, TempDir, String) {
    let identity = CorpusIdentity::new("/private/science-corpus.jsonl");
    let digest = identity_digest(&identity);
    let (fixture, runtime) = crate::v6_support::runtime_fixture(
        identity.as_str(),
        vec![vec![
            TokenInfo::default().with_orth("食べる").with_pos("VERB"),
        ]],
    );
    let directory = fixture.directory;
    let corpus_path = directory.path().join("science-corpus.jsonl");
    let dict_path = directory.path().join("unidic-cwj-202512.dic");
    let store = RequestStore::with_runtime(
        config(
            dict_path.display().to_string(),
            vec![(
                CorpusName::default_key(),
                corpus_path.clone(),
                "science-corpus".into(),
            )],
        ),
        runtime,
    );
    (store, directory, digest)
}

fn not_ready_store() -> (RequestStore, TempDir) {
    let directory = tempdir().unwrap();
    let corpus_path = directory.path().join("missing-corpus.jsonl");
    let dict_path = directory.path().join("missing.dic");
    std::fs::write(&corpus_path, b"{}\n").unwrap();
    let context = Arc::new(ServerContext::new(&dict_path).unwrap());
    let runtime = Arc::new(CorpusRuntime::new(context, &corpus_path).unwrap());
    let store = RequestStore::with_runtime(
        config(
            dict_path.display().to_string(),
            vec![(
                CorpusName::default_key(),
                corpus_path.clone(),
                "missing-corpus".into(),
            )],
        ),
        runtime,
    );
    (store, directory)
}

/// A server declaring the default corpus plus `sci`, holding every temporary
/// directory the slots' artifacts live in.
struct MultiCorpusStore {
    store: RequestStore,
    fixtures: Vec<ArtifactFixture>,
    directory: TempDir,
}

impl MultiCorpusStore {
    /// Every filesystem location this server was configured with. No response
    /// may name one: the browser sees `display_name` and `CorpusName` only.
    fn configured_paths(&self) -> Vec<String> {
        let mut paths = vec![self.directory.path().display().to_string()];
        paths.extend(
            self.fixtures
                .iter()
                .map(|fixture| fixture.path.display().to_string()),
        );
        paths
    }
}

/// The default slot is always ready. `sci_ready` decides whether the second slot
/// has an identity yet — a cold `sci` is exactly the case a selector must be
/// expressible for, and keeping the default ready makes a handler that answered
/// from the process default instead of the target slot fail visibly.
fn multi_corpus_store(sci_ready: bool) -> MultiCorpusStore {
    let directory = tempdir().unwrap();
    let dict_path = directory.path().join("unidic-cwj-202512.dic");
    let context = Arc::new(ServerContext::new(&dict_path).unwrap());
    let mut fixtures = Vec::new();
    let mut slots = IndexMap::new();
    let mut declared = Vec::new();

    let (fixture, ready) = ready_fixture(
        "/private/default-corpus.lingcorpus",
        vec![vec![
            TokenInfo::default().with_orth("食べる").with_pos("VERB"),
        ]],
    );
    let default_name = CorpusName::default_key();
    declared.push((
        default_name.clone(),
        fixture.path.clone(),
        "default-corpus".into(),
    ));
    slots.insert(
        default_name.clone(),
        CorpusSlot::new(
            default_name,
            Arc::new(CorpusRuntime::from_ready(
                ready.identity,
                Arc::into_inner(ready.artifact).expect("fixture has one artifact owner"),
                Arc::clone(&context),
            )),
            "default-corpus".into(),
        ),
    );
    fixtures.push(fixture);

    let sci_name = CorpusName::parse_declared("sci").unwrap();
    let sci_runtime = if sci_ready {
        let (fixture, ready) = ready_fixture(
            "/private/sci-corpus.lingcorpus",
            vec![vec![
                TokenInfo::default().with_orth("観察").with_pos("NOUN"),
            ]],
        );
        declared.push((sci_name.clone(), fixture.path.clone(), "sci-corpus".into()));
        let runtime = Arc::new(CorpusRuntime::from_ready(
            ready.identity,
            Arc::into_inner(ready.artifact).expect("fixture has one artifact owner"),
            Arc::clone(&context),
        ));
        fixtures.push(fixture);
        runtime
    } else {
        // Never started, so the slot is Idle and `ready()` is `None`.
        let sci_path = directory.path().join("sci-corpus.jsonl");
        std::fs::write(&sci_path, b"{}\n").unwrap();
        declared.push((sci_name.clone(), sci_path.clone(), "sci-corpus".into()));
        Arc::new(CorpusRuntime::new(Arc::clone(&context), &sci_path).unwrap())
    };
    slots.insert(
        sci_name.clone(),
        CorpusSlot::new(sci_name, sci_runtime, "sci-corpus".into()),
    );

    let store = RequestStore::with_corpus_set_capacity(
        config(dict_path.display().to_string(), declared),
        context,
        Arc::new(CorpusSet::new(slots)),
        NonZeroUsize::new(8).unwrap(),
    );
    MultiCorpusStore {
        store,
        fixtures,
        directory,
    }
}

async fn two_corpus_store() -> MultiCorpusStore {
    multi_corpus_store(true)
}

async fn cold_corpus_store() -> MultiCorpusStore {
    multi_corpus_store(false)
}

async fn post_permalink(
    store: &RequestStore,
    pattern: &str,
    corpus: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut fields = vec![("pattern", pattern)];
    if let Some(corpus) = corpus {
        fields.push(("corpus", corpus));
    }
    let body = serde_urlencoded::to_string(fields).unwrap();
    let request = Request::builder()
        .method("POST")
        .uri("/api/permalink")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let response = app_router(store.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn get_index(store: RequestStore, target: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method("GET")
        .uri(target)
        .body(Body::empty())
        .unwrap();
    let response = app_router(store).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

fn noisy_pattern() -> String {
    let mut state = 0x9e37_79b9_u32;
    (0..40_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            char::from(32 + (state % 95) as u8)
        })
        .collect()
}

fn query_of(target: &str) -> &str {
    target
        .split('&')
        .find_map(|pair| pair.strip_prefix("q="))
        .unwrap()
}

#[tokio::test]
async fn encode_ok_roundtrips_and_stamps_live_digest() {
    let (store, _directory, digest) = ready_store().await;
    let source = "verb := [upos=VERB].";
    let (status, body) = post_permalink(&store, source, None).await;
    assert_eq!(status, StatusCode::OK);
    let path = body["path"].as_str().unwrap();
    assert!(path.starts_with("?v=1&"));
    assert!(path.contains("&k=default"));
    assert!(path.contains(&format!("&c={digest}")));
    assert_eq!(
        lingrep_web::permalink::decode(query_of(path)).unwrap(),
        source
    );
}

#[tokio::test]
async fn encode_too_large_target_yields_413() {
    let (store, _directory, _digest) = ready_store().await;
    let (status, body) = post_permalink(&store, &noisy_pattern(), None).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    // Whole-body equality, not just the field: an error response quietly
    // gaining a field is worth catching.
    assert_eq!(body, serde_json::json!({ "error": "too_large" }));
}

#[tokio::test]
async fn a_ready_corpus_yields_both_selector_and_digest() {
    let (status, body) =
        post_permalink(&two_corpus_store().await.store, "p := [].", Some("sci")).await;
    assert_eq!(status, StatusCode::OK);
    let path = body["path"].as_str().unwrap();
    assert!(path.contains("&k=sci"));
    assert!(path.contains("&c="));
}

/// The case that makes cold selection expressible: no digest exists yet, and
/// that is a success rather than the old 503.
#[tokio::test]
async fn a_cold_corpus_yields_a_selector_with_no_digest() {
    let (status, body) =
        post_permalink(&cold_corpus_store().await.store, "p := [].", Some("sci")).await;
    assert_eq!(status, StatusCode::OK);
    let path = body["path"].as_str().unwrap();
    assert!(path.contains("&k=sci"));
    assert!(!path.contains("&c="));
}

#[tokio::test]
async fn an_absent_corpus_field_selects_the_default() {
    let (status, body) = post_permalink(&two_corpus_store().await.store, "p := [].", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["path"].as_str().unwrap().contains("&k=default"));
}

#[tokio::test]
async fn an_undeclared_corpus_is_not_found() {
    let (status, body) =
        post_permalink(&two_corpus_store().await.store, "p := [].", Some("absent")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, serde_json::json!({ "error": "unknown_corpus" }));
}

/// A malformed key is a lookup miss, not a validation error: `resolve` is
/// deliberately two-valued so startup owns validation.
#[tokio::test]
async fn a_malformed_corpus_key_is_not_found() {
    let (status, _) = post_permalink(
        &two_corpus_store().await.store,
        "p := [].",
        Some("has space"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// No response may name where a corpus lives, whichever way the lookup went: a
/// permalink carries `display_name`-free protocol keys and nothing else.
#[tokio::test]
async fn no_permalink_response_carries_a_filesystem_path() {
    let harness = cold_corpus_store().await;
    for corpus in [None, Some("sci"), Some("absent")] {
        let (_, body) = post_permalink(&harness.store, "p := [].", corpus).await;
        let rendered = body.to_string();
        for path in harness.configured_paths() {
            assert!(!rendered.contains(&path), "{corpus:?}: {rendered}");
        }
        assert!(!rendered.contains("lingcorpus"), "{corpus:?}: {rendered}");
        assert!(!rendered.contains(".jsonl"), "{corpus:?}: {rendered}");
    }
}

/// The default-corpus round trip, which the reserved-key split exists for: this
/// target must be readable by the landing parser.
#[tokio::test]
async fn a_default_corpus_target_round_trips_through_the_parser() {
    let (_, body) = post_permalink(&two_corpus_store().await.store, "p := [].", None).await;
    let path = body["path"].as_str().unwrap();
    let params = lingrep_web::handlers::index::parse_wire_for_test(path.strip_prefix('?').unwrap())
        .expect("a default-corpus v2 target must be readable");
    assert_eq!(params.corpus_key, "default");
}

const SEEDED: &str = "shared_permalink_probe := [upos=VERB].";

#[tokio::test]
async fn landing_matching_digest_seeds_and_autoruns() {
    let (store, _directory, digest) = ready_store().await;
    let target = lingrep_web::permalink::encode(SEEDED, "default", Some(&digest)).unwrap();
    let (status, body) = get_index(store, &target).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(SEEDED));
    assert!(body.contains("data-autorun"));
    // Ready runs now; there is nothing to queue, and the two markers are
    // mutually exclusive.
    assert!(!body.contains("data-run-when-ready"));
}

/// A link that asserts nothing about which content it saw still runs against a
/// ready corpus: an absent `c` means "no assertion", not "no corpus".
#[tokio::test]
async fn landing_without_a_digest_assertion_seeds_and_autoruns() {
    let (store, _directory, _digest) = ready_store().await;
    let target = lingrep_web::permalink::encode(SEEDED, "default", None).unwrap();
    let (status, body) = get_index(store, &target).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(SEEDED));
    assert!(body.contains("data-autorun"));
}

/// A retired selector-less target cannot seed or run a query.
#[tokio::test]
async fn a_selector_less_target_is_unreadable() {
    let (store, _directory, digest) = ready_store().await;
    let current = lingrep_web::permalink::encode(SEEDED, "default", Some(&digest)).unwrap();
    let retired = format!("/?v=1&q={}&c={digest}", query_of(&current));
    let (status, body) = get_index(store, &retired).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains(SEEDED));
    assert!(!body.contains("data-autorun"));
    assert!(body.contains("permalink-notice"));
}

/// Cold selection: the landing page describes and starts the **selected** slot
/// instead of refusing the link or answering for the default.
#[tokio::test]
async fn landing_on_a_cold_selection_prepares_without_autorun() {
    let harness = cold_corpus_store().await;
    let target = lingrep_web::permalink::encode(SEEDED, "sci", None).unwrap();
    let (status, body) = get_index(harness.store.clone(), &target).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(SEEDED));
    assert!(!body.contains("data-autorun"));
    // No identity yet, so the panel is in a not-ready state and the link's
    // pattern is seeded but not run.
    assert!(!body.contains("data-corpus-digest"));
    assert!(body.contains("permalink-notice"));
    // The PANEL describes the selected slot, never the (ready) default. B6 adds
    // a selector that lists every declared corpus by display name, so "names the
    // default at all" is no longer the discriminator — "is pointed at it" is,
    // and that is carried by the panel line, the hidden field and `selected`.
    assert!(body.contains("sci-corpus"));
    assert!(body.contains(r#"<option value="sci" selected>sci-corpus</option>"#));
    assert!(body.contains(r#"<option value="default">default-corpus</option>"#));
    assert!(!body.contains(r#"value="default" selected"#));
    // B5b: preparing is transient, so the seeded query is queued rather than
    // abandoned — the marker is what the context poll turns into one submit.
    assert!(body.contains("data-run-when-ready"));
    // Every submit from this page names the SELECTED corpus, so an in-place
    // re-run cannot resolve the default and then be refused as a conflict.
    assert!(body.contains(r#"<input type="hidden" name="corpus" value="sci">"#));
    assert!(body.contains("hx-get=\"api/corpus-context?k=sci\""));
    // The step B5 exists for: a never-selected slot must leave `Checking`, or it
    // would stay cold forever however long the client polls.
    let readiness = harness
        .store
        .corpora
        .resolve(Some("sci"))
        .expect("declared corpus")
        .runtime
        .readiness();
    assert!(
        !matches!(*readiness.borrow(), CacheReadiness::Checking),
        "the landing page must start the selected slot"
    );
}

/// A key naming a corpus this server does not declare keeps the query, points
/// at no slot, and reveals no filesystem path.
///
/// B5 discarded the decoded pattern here and reported the link as unreadable.
/// B6 supersedes that: only the *corpus* is missing, and the selector is now the
/// affordance that retargets a perfectly good query, so throwing it away would
/// be the defect. The panel still resolves no slot — the hidden field names no
/// corpus and no option is `selected` — and the offered display names are the
/// same list every ordinary landing already publishes.
#[tokio::test]
async fn landing_on_an_undeclared_corpus_keeps_the_query_without_pointing_anywhere() {
    let harness = two_corpus_store().await;
    let target = lingrep_web::permalink::encode(SEEDED, "absent", None).unwrap();
    let (status, body) = get_index(harness.store.clone(), &target).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(SEEDED));
    assert!(body.contains("data-i18n=\"notice.unknown_corpus\""));
    assert!(!body.contains("data-autorun"));
    assert!(body.contains("permalink-notice"));
    // Offered, but no declared corpus is selected: this page is pointed at none,
    // and the placeholder holds the control's position so that choosing the
    // first corpus is still a change, and so still navigates.
    assert!(body.contains(r#"<option value="sci">sci-corpus</option>"#));
    assert!(body.contains(r#"<option value="default">default-corpus</option>"#));
    assert!(body.contains(r#"<option value="" disabled selected"#));
    assert!(!body.contains(r#"value="sci" selected"#));
    assert!(!body.contains(r#"value="default" selected"#));
    // No slot resolved, so the form names no corpus rather than the default: a
    // submit from this page is refused, not answered from somewhere else.
    assert!(body.contains(r#"<input type="hidden" name="corpus" value="">"#));
    assert!(!body.contains("data-run-when-ready"));
    for path in harness.configured_paths() {
        assert!(!body.contains(&path), "{path}");
    }
}

#[tokio::test]
async fn landing_mismatched_digest_seeds_without_autorun_and_warns() {
    let (store, _directory, _digest) = ready_store().await;
    let target = lingrep_web::permalink::encode(SEEDED, "default", Some("deadbeefdead")).unwrap();
    let (status, body) = get_index(store, &target).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(SEEDED));
    assert!(!body.contains("data-autorun"));
    assert!(body.contains("permalink-banner"));
    assert!(body.contains("deadbeefdead"));
}

#[tokio::test]
async fn landing_ready_link_while_corpus_loading_seeds_without_autorun() {
    let (store, _directory) = not_ready_store();
    let target = lingrep_web::permalink::encode(SEEDED, "default", Some("abcdef012345")).unwrap();
    let (status, body) = get_index(store, &target).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(SEEDED));
    assert!(!body.contains("data-autorun"));
    assert!(body.contains("permalink-notice"));
    assert!(body.contains("still loading"));
}

/// A terminally failed corpus must not be described as "still loading". The
/// panel renders the failure and the notice states its consequence for the
/// link; the two must agree.
///
/// The failure is driven to completion **before** the request, because the
/// build a landing request starts is still `Building` when that same request
/// renders — so this state is only reachable on a later request, which is
/// exactly the one a user makes when they reload or follow the link again.
#[tokio::test]
async fn landing_on_a_failed_corpus_says_unavailable_not_loading() {
    let (store, _directory) = not_ready_store();
    let runtime = Arc::clone(
        &store
            .corpora
            .resolve(None)
            .expect("the default corpus is always declared")
            .runtime,
    );
    runtime.start_or_observe().await;
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !matches!(*runtime.readiness().borrow(), CacheReadiness::Failed { .. }) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a corpus with no dictionary must reach the failed state");

    let target = lingrep_web::permalink::encode(SEEDED, "default", Some("abcdef012345")).unwrap();
    let (status, body) = get_index(store, &target).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(SEEDED));
    assert!(!body.contains("data-autorun"));
    // The panel's failure state and the notice agree.
    assert!(body.contains("corpus.unavailable"));
    assert!(body.contains("notice.corpus_unavailable"));
    assert!(!body.contains("still loading"));
    assert!(!body.contains("corpus.preparing"));
    // Terminal: a failed corpus never gains a digest, so queueing the query
    // would leave the marker set forever.
    assert!(!body.contains("data-run-when-ready"));
}

#[tokio::test]
async fn landing_malformed_query_variants_fall_back_without_400() {
    let (store, _directory, digest) = ready_store().await;
    let valid = lingrep_web::permalink::encode(SEEDED, "default", Some(&digest)).unwrap();
    let encoded = query_of(&valid);
    let cases = [
        "/?v=1&q=not*base64&k=default&c=abcdef012345".to_string(),
        // A version this build cannot read is unreadable, never reinterpreted
        // under the current rules.
        format!("/?v=3&q={encoded}&k=default&c={digest}"),
        // The corpus selector is mandatory.
        format!("/?v=1&q={encoded}"),
        // A retired version is refused.
        format!("/?v=2&q={encoded}&c={digest}&k=default"),
        format!("/?v=1&q={encoded}&k=default&c="),
        format!("/?v=1&q={encoded}&k="),
        // A key outside the name charset is refused before any lookup.
        format!("/?v=1&q={encoded}&k=has%20space"),
        format!("/?v=1&q={encoded}&q={encoded}&k=default&c={digest}"),
        format!("/?v=1&q={encoded}&k=default&c={digest}&extra=x"),
        format!("/?v=1&q={encoded}&k=default&c={digest}&bad=%"),
        format!("/?v=1&q={encoded}&k=default&c=ABCDEF012345"),
    ];
    for target in cases {
        let (status, body) = get_index(store.clone(), &target).await;
        assert_eq!(status, StatusCode::OK, "target: {target}");
        assert!(!body.contains(SEEDED), "target: {target}");
        assert!(!body.contains("data-autorun"), "target: {target}");
        assert!(body.contains("permalink-notice"), "target: {target}");
    }
}

#[tokio::test]
async fn landing_over_length_target_falls_back_without_decoding() {
    let (store, _directory, digest) = ready_store().await;
    let target = lingrep_web::permalink::encode(SEEDED, "default", Some(&digest)).unwrap();
    let oversized = format!(
        "{target}&{}",
        "x".repeat(2 * lingrep_web::permalink::MAX_REQUEST_TARGET_LEN)
    );
    let (status, body) = get_index(store, &oversized).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains(SEEDED));
    assert!(body.contains("permalink-notice"));
}

#[tokio::test]
async fn landing_bare_root_uses_default_without_permalink_ui() {
    let (store, _directory, _digest) = ready_store().await;
    let (status, body) = get_index(store, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains(SEEDED));
    assert!(!body.contains("data-autorun"));
    assert!(!body.contains("permalink-notice"));
    assert!(!body.contains("permalink-banner"));
}
