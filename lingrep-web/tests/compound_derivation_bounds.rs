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
//! A Workbench search deriving compounds stops at the per-sentence chain-byte
//! limit and at the per-search compound limit at 1×, 10× and 100× one
//! fixture.
//!
//! Its own binary: the lowered limits are process-wide.

#[allow(dead_code)]
#[path = "suite/v6_support.rs"]
mod v6_support;

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use indexmap::IndexMap;
use lingrep_core::test_scale::ScaleKnob;
use lingrep_core::token_match::TokenInfo;
use lingrep_web::corpus_runtime::CorpusRuntime;
use lingrep_web::corpus_set::{CorpusName, CorpusSet, CorpusSlot};
use lingrep_web::revision::{AttemptPhase, ErrorCategory, QueryAttempt, WorkbenchId};
use lingrep_web::server_context::ServerContext;
use lingrep_web::snapshot::PatternKey;
use lingrep_web::state::{Config, DetailLimits, RequestStore};
use tower::ServiceExt;

use crate::v6_support::{ArtifactFixture, ready_fixture};

const WAIT: Duration = Duration::from_secs(30);

const SCALES: [usize; 3] = [1, 10, 100];

/// Two definitions that match every token, so the contiguous chains double
/// with each token of a sentence.
const CATALOG: &str = "pos := [text=語].\ntags := [text=語].\n";

const CHAIN_BYTES: usize = 16 * 1024;

const PATTERNS: usize = 8;

fn store(sentences: Vec<Vec<TokenInfo>>) -> (ArtifactFixture, RequestStore) {
    let context = Arc::new(ServerContext::new("/tmp/dict").unwrap());
    let (fixture, ready) = ready_fixture("/private/compounds.lingcorpus", sentences);
    let name = CorpusName::default_key();
    let runtime = Arc::new(CorpusRuntime::from_ready(
        ready.identity,
        Arc::into_inner(ready.artifact).expect("fixture has one artifact owner"),
        Arc::clone(&context),
    ));
    let mut slots = IndexMap::new();
    slots.insert(
        name.clone(),
        CorpusSlot::new(name.clone(), runtime, "compounds".to_owned()),
    );
    let store = RequestStore::with_corpus_set_capacity(
        Config {
            dict_path: Some("/unused/unidic-cwj-202512.dic".into()),
            corpora: vec![(name, fixture.path.clone(), "compounds".to_owned())],
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
    (fixture, store)
}

/// Posts the Workbench search form with 複合項目を生成 checked and waits for
/// the revision to end.
async fn derive_compounds(catalog: &str, sentences: Vec<Vec<TokenInfo>>) -> QueryAttempt {
    let (_fixture, store) = store(sentences);
    let workbench = WorkbenchId::new("wb-compounds");
    let body = serde_urlencoded::to_string([
        ("pattern", catalog),
        ("corpus", CorpusName::DEFAULT),
        ("derive_compounds", "true"),
    ])
    .unwrap();
    let response = lingrep_web::app_router(store.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/search/{}", workbench.as_str()))
                .header(
                    axum::http::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    response.into_body().collect().await.unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            if let Some(view) = store.coordinators.snapshot(&workbench).await
                && view.state.is_terminal()
            {
                return view.state.current_attempt().unwrap().clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the search ends")
}

fn word() -> TokenInfo {
    TokenInfo::default().with_orth("語").with_pos("NOUN")
}

/// Ten definitions `w0` to `w9`, each matching only the token `語0` to `語9`.
fn numbered_catalog() -> String {
    (0..10)
        .map(|n| format!("w{n} := [text=語{n}].\n"))
        .collect()
}

/// Sentence `k` is `語k 語k+1 語k+2`, digits modulo 10, so it derives three
/// compounds, two of them absent from sentence `k - 1`.
fn numbered_sentence(k: usize) -> Vec<TokenInfo> {
    (k..k + 3)
        .map(|n| {
            TokenInfo::default()
                .with_orth(format!("語{}", n % 10))
                .with_pos("NOUN")
        })
        .collect()
}

fn assert_result_limit(attempt: &QueryAttempt, row: &str) {
    let AttemptPhase::Failed(failure) = &attempt.phase else {
        panic!("{row}: expected Failed, got {attempt:?}");
    };
    assert_eq!(
        failure.category,
        ErrorCategory::ResultLimit,
        "{row}: {failure:?}"
    );
    assert!(
        failure.message.contains("Derive compounds"),
        "{row}: {failure:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compound_derivation_stops_at_its_limits_at_every_scale() {
    // One sentence of 8 × scale tokens: its chains outgrow the per-sentence
    // byte limit, while the chains held stay within it.
    let chain_bytes = ScaleKnob::CompoundChainBytes.set(CHAIN_BYTES);
    for scale in SCALES {
        lingrep_web::held_chains::take_peak();
        let attempt = derive_compounds(CATALOG, vec![vec![word(); 8 * scale]]).await;
        let row = format!("{scale}× sentence length");
        assert_result_limit(&attempt, &row);
        let held = lingrep_web::held_chains::take_peak();
        assert!(held > 0, "{row}: no chain was derived");
        let held_bytes = held * size_of::<(Vec<PatternKey>, usize, usize)>();
        assert!(
            held_bytes <= CHAIN_BYTES,
            "{row}: {held} chains held in one sentence, at least {held_bytes} bytes, \
             over the {CHAIN_BYTES}-byte limit"
        );
    }
    drop(chain_bytes);

    // 5 × scale sentences that each derive 3 compounds, fewer than the
    // per-search limit, and together more than it from the fourth sentence on.
    let _patterns = ScaleKnob::CompoundPatterns.set(PATTERNS);
    let catalog = numbered_catalog();
    for scale in SCALES {
        let sentences = (0..5 * scale).map(numbered_sentence).collect();
        let attempt = derive_compounds(&catalog, sentences).await;
        assert_result_limit(&attempt, &format!("{scale}× sentence count"));
    }
}
