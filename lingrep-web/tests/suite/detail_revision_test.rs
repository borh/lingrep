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
use axum::{Router, body::Body, http::Request};
use http_body_util::BodyExt;
use lingrep_core::token_match::TokenInfo;
use lingrep_web::revision::{RevisionId, WorkbenchId};
use lingrep_web::state::{Config, DetailLimits, RequestStore};
use tower::ServiceExt;

use crate::v6_support::runtime_fixture;

async fn body_string(response: axum::response::Response) -> String {
    String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}

async fn complete(store: &RequestStore, workbench: &WorkbenchId, source: &str) -> RevisionId {
    let slot = store
        .corpora
        .resolve(None)
        .expect("a validated configuration declares a default corpus");
    let revision = store
        .coordinators
        .accept_next_revision(workbench.clone(), slot, source.into())
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if store
                .coordinators
                .snapshot(workbench)
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
    revision
}

#[tokio::test]
async fn stale_detail_revision_never_reinterprets_pattern_key() {
    let (_fixture, runtime) = runtime_fixture(
        "detail-revision",
        vec![vec![
            TokenInfo::default().with_orth("食べる").with_pos("VERB"),
        ]],
    );
    let ready = runtime.ready().await.unwrap();
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
        runtime,
    );
    let workbench = WorkbenchId::new("detail-revision");
    let first = complete(&store, &workbench, "old := [text=食べる].").await;
    complete(&store, &workbench, "new := [upos=VERB].").await;
    let before = ready.decoded_block_count();
    let app = Router::new()
        .merge(lingrep_web::handlers::pattern_detail::routes())
        .with_state(store);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/pattern-detail/{}/{}/0",
                    workbench.as_str(),
                    first.0
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
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
    assert!(html.contains("Newer results are available"), "{html}");
    assert_eq!(ready.decoded_block_count(), before);
}

/// Builds a store backed by a corpus with three one-token sentences whose
/// orths differ but which all carry `pos: Some("VERB")`, so a pattern like
/// `[upos=VERB]` yields three distinct surface forms. The returned `TempDir`
/// must be kept alive by the caller because the artifact is memory mapped.
async fn store_with_three_distinct_verbs(max_kwic: usize) -> (RequestStore, tempfile::TempDir) {
    let (fixture, runtime) = runtime_fixture(
        "surface-cap",
        vec![
            vec![TokenInfo::default().with_orth("食べる").with_pos("VERB")],
            vec![TokenInfo::default().with_orth("飲む").with_pos("VERB")],
            vec![TokenInfo::default().with_orth("歩く").with_pos("VERB")],
        ],
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
            detail_limits: DetailLimits { max_kwic },
        },
        runtime,
    );
    (store, fixture.directory)
}

#[tokio::test]
async fn pattern_detail_projects_one_ordered_sample_for_distribution_and_kwic() {
    let (store, _directory) = store_with_three_distinct_verbs(1000).await;
    let workbench = WorkbenchId::new("pattern-detail");
    let revision = complete(&store, &workbench, "v := [upos=VERB].").await;
    let app = lingrep_web::handlers::pattern_detail::routes().with_state(store);
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/pattern-detail/{}/{}/0",
                    workbench.as_str(),
                    revision.0
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = body_string(response).await;
    assert!(body.contains("3 retained hits"));
    assert_eq!(body.matches("class=\"kwic-row\"").count(), 3);
    // The wired path cross-tabulates, so the variation list reports the corpus
    // rather than the retained sample.
    assert!(body.contains("Surface variations \u{2014} corpus"));
    assert!(!body.contains("Surface variations in retained sample"));
    assert!(
        body.contains("role=\"columnheader\" scope=\"col\" data-i18n=\"detail.col_index\">#</th>")
    );
    assert!(body.contains("scope=\"col\" data-i18n=\"detail.col_sentence\">Sentence</th>"));
    assert_eq!(body.matches("class=\"kwic-sentence-cell\"").count(), 3);
    assert_eq!(body.matches("class=\"kwic-sentence\"").count(), 3);
    assert!(body.contains("data-expandable-text"));
    assert!(body.contains("data-expand-sentence"));
    assert!(body.contains("hx-trigger=\"hit-select\""));
    assert!(body.contains("hx-target=\"#inspector-body\""));
    assert!(body.contains("hx-sync=\"#inspector-body:replace\""));
    assert!(!body.contains("<th>Left context</th>"));
    assert!(!body.contains("<th>Right context</th>"));
    assert!(body.contains("data-hit-grid data-kwic-pagination"));
    // One pager, living in the sticky pane header above the KWIC table.
    assert_eq!(body.matches("data-kwic-pager").count(), 1);
    assert_eq!(body.matches("data-page-previous").count(), 1);
    assert_eq!(body.matches("data-page-next").count(), 1);
    assert_eq!(body.matches("data-page-size").count(), 1);
    assert!(body.contains("class=\"kwic-pane-header\""));
    // The concordance is its own view, and the default one: the charts used
    // to stack above it and push the first hit off the fold.
    assert!(body.contains("data-detail-view=\"concordance\""));
    assert!(!body.contains("distribution-band"));
    assert!(body.contains("data-pattern-id=\""));
    for size in [50, 100, 250, 500, 1000] {
        assert_eq!(body.matches(&format!(r#"value="{size}""#)).count(), 1);
    }
}

#[tokio::test]
async fn row_ordinal_round_trips_to_the_same_retained_reference() {
    let (store, _directory) = store_with_three_distinct_verbs(1000).await;
    let workbench = WorkbenchId::new("ordinal-round-trip");
    let revision = complete(&store, &workbench, "v := [upos=VERB].").await;
    let app = Router::new()
        .nest("/api", lingrep_web::handlers::pattern_detail::routes())
        .with_state(store);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/hit-detail/{}/{}/0/0",
                    workbench.as_str(),
                    revision.0
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert!(body_string(response).await.contains("食べる"));
}
