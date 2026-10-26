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
use std::sync::Arc;

use askama::Template;
use axum::{Router, body::Body, http::Request, http::StatusCode};
use http_body_util::BodyExt;
use lingrep_core::token_match::TokenInfo;
use lingrep_web::corpus_runtime::ReadyCorpus;
use lingrep_web::inspection::InspectionProjector;
use lingrep_web::revision::{RevisionId, WorkbenchId};
use lingrep_web::state::{Config, DetailLimits, RequestStore};
use lingrep_web::templates::TokenInspectionFragment;
use tower::ServiceExt;

use crate::v6_support::runtime_fixture;

struct HandlerFixture {
    _directory: tempfile::TempDir,
    app: Router,
    cache: ReadyCorpus,
}

struct TestResponse {
    status: StatusCode,
    html: String,
}

impl HandlerFixture {
    async fn displaying_revision(revision: u64) -> Self {
        let (fixture, runtime) = runtime_fixture(
            "handler",
            vec![vec![
                TokenInfo::default().with_orth("を"),
                TokenInfo::default()
                    .with_pos1("動詞")
                    .with_pos2("一般")
                    .with_pos3("*")
                    .with_pos4("*")
                    .with_ctype("下一段")
                    .with_cform("終止形")
                    .with_lform("タベル")
                    .with_lemma("食べる")
                    .with_orth("食べる")
                    .with_pron("タベル")
                    .with_orth_base("食べる")
                    .with_pron_base("タベル")
                    .with_goshu("和")
                    .with_kana("タベル")
                    .with_kana_base("タベル")
                    .with_form("タベル")
                    .with_type_("<script>fixture</script>")
                    .with_pos("VERB"),
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
        let workbench = WorkbenchId::new("wb");
        let slot = store
            .corpora
            .resolve(None)
            .expect("a validated configuration declares a default corpus");
        for expected in 1..=revision {
            assert_eq!(
                store
                    .coordinators
                    .accept_next_revision(
                        workbench.clone(),
                        slot,
                        "eating := [text=食べる].".into()
                    )
                    .await,
                RevisionId(expected),
            );
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if store
                    .coordinators
                    .snapshot(&workbench)
                    .await
                    .and_then(|view| view.state.displayed_snapshot().map(|item| item.revision))
                    == Some(RevisionId(revision))
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let cache = runtime.ready().await.unwrap();
        let app = Router::new()
            .merge(lingrep_web::handlers::inspect::routes())
            .with_state(store);
        Self {
            _directory: fixture.directory,
            app,
            cache,
        }
    }

    async fn get(&self, path: &str) -> TestResponse {
        let response = self
            .app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
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
        TestResponse { status, html }
    }
}

#[tokio::test]
async fn stale_revision_never_reinterprets_pattern_or_reference_keys() {
    let fixture = HandlerFixture::displaying_revision(4).await;
    let before = fixture.cache.decoded_block_count();
    let response = fixture.get("/inspect/wb/3/0/0/1").await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    assert!(response.html.contains("Newer results are available"));
    assert_eq!(fixture.cache.decoded_block_count(), before);
}

#[tokio::test]
async fn stale_revision_wins_over_an_out_of_range_snapshot_selector() {
    let fixture = HandlerFixture::displaying_revision(4).await;
    let before = fixture.cache.decoded_block_count();
    let response = fixture
        .get(&format!("/inspect/wb/3/{}/0/1", usize::MAX))
        .await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    assert!(response.html.contains("Newer results are available"));
    assert_eq!(fixture.cache.decoded_block_count(), before);
}

#[tokio::test]
async fn selected_token_returns_grouped_fields_not_a_full_sentence_table() {
    let fixture = HandlerFixture::displaying_revision(4).await;
    let response = fixture.get("/inspect/wb/4/0/0/1").await;
    assert_eq!(response.status, StatusCode::OK);
    assert!(response.html.contains("Token inspection"));
    assert!(
        response.html.contains("18 of 32 fields available"),
        "{}",
        response.html
    );
    assert!(response.html.contains("data-field-key=\"orth\""));
    assert!(response.html.contains("class=\"token-field-group\""));
    assert!(response.html.contains("class=\"token-field-grid\""));
    assert!(response.html.contains("class=\"token-field\""));
    assert!(
        response
            .html
            .contains("<h4><span data-label-en>Surface</span>")
    );
    assert!(
        response
            .html
            .contains("<details class=\"unavailable-fields\">")
    );
    assert!(response.html.contains("Show unavailable fields"));
    assert!(response.html.contains(">Unavailable</dd>"));
    assert!(
        response
            .html
            .contains("class=\"copy-status\" role=\"status\"")
    );
    assert!(response.html.contains(">Orthography</span>"));
    assert!(!response.html.contains("<script>fixture</script>"));
    assert!(
        response
            .html
            .contains("&#60;script&#62;fixture&#60;/script&#62;")
    );
}

#[test]
fn token_fragment_preserves_empty_and_unavailable_value_markers() {
    let inspection = InspectionProjector::project_token(
        &TokenInfo::default().with_orth("食べる").with_lemma(""),
    );
    let html = TokenInspectionFragment::new(inspection).render().unwrap();

    assert!(html.contains(
        "class=\"empty-value\" data-i18n=\"token_inspection.empty_value\">Empty value</span>"
    ));
    assert!(html.contains("<details class=\"unavailable-fields\">"));
    assert!(html.contains(">Unavailable</dd>"));
}
