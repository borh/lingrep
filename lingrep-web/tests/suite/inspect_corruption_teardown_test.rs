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
//! Handler-level teardown contract for `inspect` (Thread B follow-up).
//!
//! `cache_reader_test::corruption_is_a_pure_value_and_does_not_flip_readiness`
//! pins the reader half: a corrupt decode is a returned value, not a side
//! effect. This test pins the *other* half through the real HTTP surface: when
//! the `inspect` handler observes that corruption, IT is the owning caller that
//! drives the single at-most-once teardown, flipping global readiness to
//! `Failed { Corruption }` and returning the corruption response.
//!
//! The scan runs against a valid artifact and matches, so a real match reference
//! exists to inspect. The file is corrupted only afterwards; because
//! V7 validates the mapped block checksum on every decode, the later inspect
//! read observes corruption even though the scan already read it cleanly.

use crate::v6_support::RevisionTestExt;
use std::sync::Arc;
use std::time::Duration;

use axum::{Router, body::Body, http::Request, http::StatusCode};
use http_body_util::BodyExt;
use lingrep_core::token_match::TokenInfo;
use lingrep_web::corpus_runtime::{CacheFailureCategory, CacheReadiness, CorpusRuntime};
use lingrep_web::revision::{RevisionId, WorkbenchId};
use lingrep_web::state::{Config, DetailLimits, RequestStore};
use tower::ServiceExt;

use crate::v6_support::runtime_fixture;

/// A corpus of one sentence, `[を, 食べる]`, with a displayed revision whose
/// pattern matched the 食べる token at sentence 0. Returns the runtime (to
/// observe readiness), the wired router, and the on-disk cache path (to corrupt).
struct Fixture {
    _directory: tempfile::TempDir,
    runtime: Arc<CorpusRuntime>,
    app: Router,
    cache_path: std::path::PathBuf,
}

impl Fixture {
    async fn build() -> Self {
        let (fixture, runtime) = runtime_fixture(
            "handler",
            vec![vec![
                TokenInfo::default().with_orth("を"),
                TokenInfo::default().with_orth("食べる").with_pos("VERB"),
            ]],
        );
        let cache_path = fixture.path.join("tokens.v1.zst");
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
        assert_eq!(
            store
                .coordinators
                .accept_next_revision(workbench.clone(), slot, "eating := [text=食べる].".into())
                .await,
            RevisionId(1),
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if store
                    .coordinators
                    .snapshot(&workbench)
                    .await
                    .and_then(|view| view.state.displayed_snapshot().map(|item| item.revision))
                    == Some(RevisionId(1))
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let app = Router::new()
            .merge(lingrep_web::handlers::inspect::routes())
            .with_state(store);
        Self {
            _directory: fixture.directory,
            runtime,
            app,
            cache_path,
        }
    }

    /// Overwrite the first byte of block 0's payload so the next decode of
    /// sentence 0 fails validation (mirrors the reader-level corruption test).
    fn corrupt_cache(&self) {
        crate::v6_support::corrupt_first_token_block(&self.cache_path);
    }

    async fn get(&self, path: &str) -> (StatusCode, String) {
        let response = self
            .app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        (status, body)
    }
}

#[tokio::test]
async fn inspect_handler_drives_failed_corruption_and_returns_500() {
    let fixture = Fixture::build().await;
    let mut readiness = fixture.runtime.readiness();
    assert!(
        matches!(*readiness.borrow_and_update(), CacheReadiness::Ready { .. }),
        "corpus starts ready before corruption"
    );

    fixture.corrupt_cache();

    // Inspect the matched 食べる token; the read re-opens the corrupt block.
    let (status, body) = fixture.get("/inspect/wb/1/0/0/1").await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        body.contains("The corpus cache is damaged and will be rebuilt."),
        "{body}"
    );

    // The handler was the owning caller: it drove the single teardown, so the
    // broadcast readiness flipped to Failed { Corruption } ...
    assert!(readiness.has_changed().unwrap());
    assert_eq!(
        *readiness.borrow_and_update(),
        CacheReadiness::Failed {
            category: CacheFailureCategory::Corruption
        }
    );
    // ... and the lifecycle itself tore down, so no corpus is ready to serve.
    assert!(fixture.runtime.ready().await.is_none());
}
