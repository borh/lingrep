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
        dict_path: Some(dict_path),
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
    let text = encode_form(
        "examples: [\"猫が走る\"] counterexamples: [\"走る\"] v := [upos=VERB].",
        "猫が走る",
    );
    let res = post(&app, text).await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("走る"), "{html}");
    assert!(
        html.contains(r#"data-i18n-args='{"failed": 1, "checked": 2}'"#),
        "the counterexample is tokenized and matched: {html}"
    );
    let dictionary = std::path::PathBuf::from(lingrep_core::testing::dictionary_path());
    let name = dictionary.file_name().unwrap().to_string_lossy();
    assert!(
        html.contains(&format!("<code>vibrato morphology · {name}</code>")),
        "{html}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn configured_adapter_analyzes_text_edits_and_reuses_tokens_for_pattern_edits() {
    use clap::Parser;
    use std::process::Command;

    let artifact = super::v6_support::build_artifact(vec![(
        "sample.txt",
        0,
        false,
        vec![vec![
            lingrep_core::token_match::TokenInfo::default().with_orth("猫"),
        ]],
    )]);
    let bundle = tempfile::tempdir().unwrap();
    let config_path = bundle.path().join("analyzer.toml");
    let batches = bundle.path().join("batches.txt");
    let marker = r#"    elif kind == "batch_begin":"#;
    let adapter = include_str!("../../../lingrep-core/tests/fixtures/external_echo_adapter.py")
        .replace(marker, &format!(
            "{marker}\n        with open({}, 'a') as batch_log:\n            batch_log.write('batch\\n')",
            serde_json::to_string(&batches.to_string_lossy()).unwrap(),
        ))
        .replace(
            "values.append(text if attribute[\"key\"] == \"text\" else None)",
            "values.append(text if attribute[\"key\"] == \"text\" else [\"echo\"])",
        );
    std::fs::write(bundle.path().join("adapter.py"), adapter).unwrap();
    let python = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    assert!(python.status.success());
    let python = std::str::from_utf8(&python.stdout).unwrap().trim();
    std::fs::write(
        &config_path,
        format!(
            "version = 1\nkind = 'protocol'\npython = '{python}'\nentrypoint = 'adapter.py'\nlanguage = 'ja'\nschema = [{{ key = 'text', kind = 'scalar' }}, {{ key = 'echo.markers', kind = 'string_set' }}]\n"
        ),
    )
    .unwrap();
    let args = lingrep_web::cli::Args::try_parse_from([
        std::ffi::OsStr::new("lingrep-web"),
        std::ffi::OsStr::new("--input"),
        artifact.path.as_os_str(),
        std::ffi::OsStr::new("--sample-analyzer-config"),
        config_path.as_os_str(),
    ])
    .expect("server accepts a sample analyzer bundle");
    let mut config = lingrep_web::serve::config(&args).unwrap();
    config.dict_path = None;
    let store = lingrep_web::serve::store(&args, config).await.unwrap();
    let app = lingrep_web::handlers::sample::routes().with_state(store);
    let res = post(
        &app,
        encode_form(
            "examples: [\"猫が走る\"] counterexamples: [\"猫\", \"\"] whole := [text=猫が走る & echo.markers has echo].",
            "猫が走る",
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let html = std::str::from_utf8(&body).unwrap();
    assert!(html.contains("external morphology"), "{html}");
    assert!(html.contains("data-sample-match="), "{html}");
    assert!(html.contains("data-sample-claims=\"hold\""), "{html}");

    let count_batches = || std::fs::read_to_string(&batches).unwrap().lines().count();
    assert_eq!(count_batches(), 2);
    let changed_pattern =
        "examples: [\"猫が走る\"] counterexamples: [\"猫\"] whole := [surface=猫が走る].";
    let res = post(&app, encode_form(changed_pattern, "猫が走る")).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        count_batches(),
        2,
        "pattern edits reuse sample and claim tokens"
    );

    let res = post(&app, encode_form(changed_pattern, "犬が走る")).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(count_batches(), 3, "text edits run the configured analyzer");

    let res = post(
        &app,
        format!("{}&corpus=default", encode_form("word := [text=猫].", "猫")),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);

    let res = post(
        &app,
        format!(
            "{}&corpus=default&analyzer=configured",
            encode_form("word := [text=猫].", "猫")
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let html = std::str::from_utf8(&body).unwrap();
    assert!(html.contains("data-sample-corpus-differs"), "{html}");
}
