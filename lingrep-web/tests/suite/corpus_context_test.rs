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
use axum::body::Body;
use axum::http::{Request, StatusCode};
use lingrep_web::corpus_set::CorpusName;
use lingrep_web::{Config, RequestStore, app_router, state::DetailLimits};
use tower::ServiceExt; // oneshot

/// The display name `declared_corpora` would compute: the path's file stem.
fn display_name_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "corpus".to_string())
}

fn store_with_corpus_path(path: &str) -> RequestStore {
    // A never-discovered path leaves the runtime not-ready (state 4). The pool
    // constructs without touching the path, so this is cheap and deterministic.
    RequestStore::new(Config {
        dict_path: "/nonexistent/dict".into(),
        corpora: vec![(
            CorpusName::default_key(),
            path.into(),
            display_name_of(path),
        )],
        input_format: None,
        markdown: lingrep_core::corpus::MarkdownReaderConfig::new(
            lingrep_core::corpus::SectionPolicy::default(),
        ),
        max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
        detail_limits: DetailLimits { max_kwic: 100 },
    })
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn index_shows_a_preparing_strip_with_the_corpus_stem_and_no_full_path() {
    let app = app_router(store_with_corpus_path("/private/data/secret-corpus.jsonl"));
    let response = app
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_string(response).await;
    assert!(html.contains("class=\"corpus-context\""));
    assert!(
        html.contains(
            r#"<span data-i18n="corpus.label">Corpus:</span> <span>secret-corpus</span>"#
        )
    ); // stem only, no tooltip yet
    assert!(html.contains("Corpus is preparing"));
    // Trust boundary: the full path must never reach the page.
    assert!(!html.contains("/private/data/"));
    assert!(!html.contains("secret-corpus.jsonl"));
}

#[tokio::test]
async fn corpus_context_partial_route_serves_the_strip() {
    let app = app_router(store_with_corpus_path("/x/demo-corpus.jsonl"));
    let response = app
        .oneshot(
            Request::get("/api/corpus-context")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .map(|v| v.to_str().unwrap()),
        Some("no-store"),
        "the changing partial must not be cached"
    );
    let html = body_string(response).await;
    assert!(html.contains("class=\"corpus-context\""));
    assert!(html.contains("demo-corpus"));
    // Preparing → polls, and the poll names the corpus it is about. A keyless
    // request means the default, so that is what the next poll carries.
    assert!(html.contains("hx-get=\"api/corpus-context?k=default\""));
}

/// The poll must observe the corpus the page *selected*. A `k` naming a corpus
/// this server does not declare is a lookup miss, not a validation error, and is
/// answered as an unavailable corpus rather than by silently describing the
/// default — which would tell the reader a corpus is ready when the one they
/// asked for does not exist.
#[tokio::test]
async fn the_poll_answers_for_its_key_and_refuses_an_undeclared_one() {
    let app = app_router(store_with_corpus_path("/x/demo-corpus.jsonl"));
    let response = app
        .oneshot(
            Request::get("/api/corpus-context?k=no-such-corpus")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_string(response).await;
    assert!(html.contains("Corpus unavailable."));
    assert!(!html.contains("demo-corpus"));
    assert!(!html.contains("hx-get")); // terminal: nothing to wait for
}
