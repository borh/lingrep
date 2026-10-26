use crate::v6_support::RevisionTestExt;
use std::sync::Arc;

use axum::{Router, body::Body, http::Request};
use http_body_util::BodyExt;
use lingrep_core::token_match::TokenInfo;
use lingrep_web::revision::WorkbenchId;
use lingrep_web::state::{Config, DetailLimits, RequestStore};
use tower::ServiceExt;

use crate::v6_support::runtime_fixture;

#[tokio::test]
async fn compact_kwic_defers_token_inspection_and_decodes_retained_rows() {
    let (_fixture, runtime) = runtime_fixture(
        "inspection-fixture",
        vec![vec![
            TokenInfo::default()
                .with_orth("食べる")
                .with_lemma("食べる")
                .with_pos("VERB")
                .with_cform("終止形")
                .with_pron_base("タベル")
                .with_kana(""),
        ]],
    );
    let store = RequestStore::with_runtime(
        Config {
            dict_path: String::new(),
            corpora: Vec::new(),
            input_format: None,
            markdown: lingrep_core::corpus::MarkdownReaderConfig::new(
                lingrep_core::corpus::SectionPolicy::default(),
            ),
            max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
            detail_limits: DetailLimits { max_kwic: 10 },
        },
        Arc::clone(&runtime),
    );
    let workbench = WorkbenchId::new("inspection");
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
                .and_then(|view| view.state.displayed_snapshot().cloned())
                .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let cache = runtime.ready().await.unwrap();
    let before = cache.decoded_block_count();
    let app = Router::new()
        .merge(lingrep_web::handlers::pattern_detail::routes())
        .merge(lingrep_web::handlers::inspect::routes())
        .with_state(store);
    let route = format!("/pattern-detail/{}/1/0", workbench.as_str());
    let response = app
        .clone()
        .oneshot(Request::builder().uri(route).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
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
    let after = cache.decoded_block_count();

    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(html.contains("class=\"kwic-row\""));
    assert!(!html.contains("class=\"kwic-token kwic-token-match\""));
    assert!(!html.contains("PronBase"));
    assert!(html.contains("hx-get=\"api/hit-detail/"));
    assert!(!html.contains("hx-get=\"api/inspect/"));
    assert!(after > before, "primary KWIC must decode retained rows");

    let hit_detail = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/hit-detail/inspection/1/0/0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let hit_detail_html = String::from_utf8(
        hit_detail
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(hit_detail_html.contains("hx-get=\"api/inspect/"));

    let inspection = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/inspect/inspection/1/0/0/0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let inspection_html = String::from_utf8(
        inspection
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(inspection_html.contains("Base pronunciation"));
    assert!(
        inspection_html.contains("6 of 32 fields available"),
        "{inspection_html}"
    );
    let repeated = app
        .oneshot(
            Request::builder()
                .uri("/inspect/inspection/1/0/0/0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(repeated.status(), axum::http::StatusCode::OK);
    let repeated_html = repeated.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(repeated_html.as_ref(), inspection_html.as_bytes());
}
