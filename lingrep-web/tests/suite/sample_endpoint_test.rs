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
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
    response::Response,
};
use http_body_util::BodyExt;
use lingrep_web::corpus_set::CorpusName;
use lingrep_web::state::{Config, DetailLimits, RequestStore};
use tower::ServiceExt;

/// Percent-encode `pattern`/`sample` as an `application/x-www-form-urlencoded`
/// body (via the already-present `serde_urlencoded` dev-dep, so Japanese text
/// round-trips correctly without hand-rolled percent-encoding).
fn encode_form(pattern: &str, sample: &str) -> String {
    #[derive(serde::Serialize)]
    struct Form<'a> {
        pattern: &'a str,
        sample: &'a str,
    }
    serde_urlencoded::to_string(Form { pattern, sample }).unwrap()
}

// `sample::routes()` registers the bare `/sample` path; the `/api` prefix is
// added only by the `.nest("/api", ...)` in `main.rs` (same convention as
// `kwic`/`search`/`surface`, whose own handler tests post to the bare path).
async fn post(app: &Router, form: String) -> Response {
    app.clone()
        .oneshot(
            Request::post("/sample")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .unwrap()
}

// 413 on oversize sample. The line count exceeds MAX_SAMPLE_LINES (2000), so
// the size cap rejects this before the (empty, unusable) tokenizer is ever
// consulted — this test needs no dictionary.
#[tokio::test]
async fn oversize_sample_is_rejected_with_413() {
    let app = lingrep_web::handlers::sample::routes().with_state(RequestStore::default());
    let big = "あ\n".repeat(3000);
    let text = encode_form("v := [upos=動詞].", &big);
    let res = post(&app, text).await;
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

// 503 when the dictionary can't load (empty-dict default store) but the
// input is well within the size caps.
#[tokio::test]
async fn missing_dictionary_yields_503_not_panic() {
    let app = lingrep_web::handlers::sample::routes().with_state(RequestStore::default());
    let text = encode_form("v := [upos=VERB].", "猫");
    let res = post(&app, text).await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn real_dictionary_returns_a_rendered_match() {
    let dict_path = lingrep_core::testing::dictionary_path();
    let store = RequestStore::new(Config {
        dict_path,
        corpora: vec![(
            CorpusName::default_key(),
            std::path::PathBuf::new(),
            String::new(),
        )],
        input_format: None,
        markdown: lingrep_core::corpus::MarkdownReaderConfig::new(
            lingrep_core::corpus::SectionPolicy::default(),
        ),
        max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
        detail_limits: DetailLimits { max_kwic: 10 },
    });
    let app = lingrep_web::handlers::sample::routes().with_state(store);
    let text = encode_form("v := [upos=VERB].", "猫が走る");
    let res = post(&app, text).await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("走る"), "{html}");
}
