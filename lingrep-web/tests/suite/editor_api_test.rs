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
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use lingrep_web::{RequestStore, handlers::pattern_editor};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn post(path: &str, value: Value) -> (StatusCode, Value) {
    let app = pattern_editor::routes().with_state(RequestStore::default());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(value.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn check_is_source_to_result_and_sanitizes_diagnostics() {
    let secret = "EDITOR_SECRET_92C1";
    let (status, body) = post(
        "/check",
        json!({"source": format!("秘密 := [text~/({secret}/].")}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let encoded = body.to_string();
    assert!(!encoded.contains(secret));
    assert!(!encoded.contains("edit_revision"));
    assert_eq!(body["status"], "invalid");
}

#[tokio::test]
async fn check_returns_safe_unicode_byte_ranges_for_every_diagnostic() {
    let source = "猫 := [text=猫] @missing.";
    let (status, body) = post("/check", json!({"source": source})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let diagnostic = &body["diagnostics"][0];
    assert_eq!(diagnostic["code"], "UndefinedReference");
    assert_eq!(diagnostic["location"]["source_name"], "lingrep-web.lx");
    let start = diagnostic["location"]["byte_range"]["start"]
        .as_u64()
        .unwrap() as usize;
    let end = diagnostic["location"]["byte_range"]["end"]
        .as_u64()
        .unwrap() as usize;
    assert_eq!(&source.as_bytes()[start..end], b"@missing");
    assert_eq!(diagnostic["message"], "Pattern references are invalid.");
}

#[tokio::test]
async fn a_valid_pattern_reports_its_compile_stats() {
    let (status, body) = post("/check", json!({"source": "p := [upos=VERB]."})).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "valid");
    assert_eq!(body["stats"]["definitions"], 1);
    assert_eq!(body["stats"]["references"], 0);
    assert_eq!(body["diagnostics"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn an_unreferenced_fragment_is_a_warning_on_an_otherwise_valid_pattern() {
    let source = "fragment orphan := [upos=NOUN].\np := [upos=VERB].";
    let (status, body) = post("/check", json!({"source": source})).await;

    // The program is well formed: a warning must not turn into a rejection.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "valid");
    assert_eq!(body["stats"]["definitions"], 2);

    let diagnostics = body["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["code"], "UnusedFragment");
    assert_eq!(diagnostics[0]["severity"], "warning");
    assert_eq!(diagnostics[0]["category"], "lint");
    // The message is canned, not the compiler's: lingrep-web never echoes submitted
    // pattern text, and the compiler's message embeds the definition name.
    assert_eq!(diagnostics[0]["message"], "A fragment is never referenced.");
    assert_eq!(diagnostics[0]["location"]["line"], 1);
    assert_eq!(diagnostics[0]["location"]["byte_range"]["start"], 0);
    assert_eq!(diagnostics[0]["location"]["byte_range"]["end"], 31);
}

#[tokio::test]
async fn a_rejected_pattern_still_marks_its_diagnostics_as_errors() {
    let (status, body) = post("/check", json!({"source": "x := @missing."})).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["status"], "invalid");
    assert_eq!(body["diagnostics"][0]["severity"], "error");
}

#[tokio::test]
async fn format_returns_canonical_lf_and_located_errors() {
    let (status, body) = post("/format", json!({"source":"x:=[upos=VERB].\r\n"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "formatted");
    assert_eq!(body["source"], "x := [upos=VERB].\n");

    let (status, body) = post("/format", json!({"source":"bad := [upos=]."})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["status"], "invalid");
    assert!(body["diagnostics"][0]["location"]["line"].is_number());
}
