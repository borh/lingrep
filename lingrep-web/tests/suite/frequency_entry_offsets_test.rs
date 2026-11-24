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
use clap::Parser;
use http_body_util::BodyExt;
use lingrep_core::execution::CancellationFlag;
use lingrep_core::token_match::TokenInfo;
use lingrep_corpus::{CountTableKey, CountTableWriter, DocumentTotals, GroupKind, GroupSchema};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::v6_support as support;

const CATALOG: &str = "pattern word := [].\npattern pair := [] [].\n";

struct Served {
    app: axum::Router,
    _artifact: support::ArtifactFixture,
    _dictionary: lingrep_core::testing::MiniDictionary,
}

/// Serves one sentence, `NHK news`, whose tokens carry no character offsets,
/// with a table that counts `word` under each token. `pair_entry` adds a
/// two-token `pair` entry, which `ling summarize --write-table` cannot write:
/// it fails with `missing_surface_offsets` and leaves no table.
async fn serve(pair_entry: bool) -> Served {
    let artifact = support::build_artifact(vec![(
        "doc.md",
        0,
        false,
        vec![vec![
            TokenInfo::default().with_orth("NHK"),
            TokenInfo::default().with_orth("news"),
        ]],
    )]);
    let root = artifact.directory.path().join("patterns");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("catalog.lx"), CATALOG).unwrap();
    let table = artifact.directory.path().join("fixture.lingtable");
    let key = CountTableKey {
        analysis_identity: lingrep_corpus::format_identity(
            support::open_artifact(&artifact)
                .analysis_identity()
                .as_bytes(),
        ),
        catalog_fingerprint: lingrep_core::fsm::catalog_fingerprint(CATALOG),
        matcher_revision: lingrep_core::fsm::MATCHER_SEMANTICS_IDENTITY.to_owned(),
    };
    let mut writer = CountTableWriter::create(
        &table,
        key,
        vec!["word".into(), "pair".into()],
        vec![GroupSchema {
            kind: GroupKind::Surface,
            names: Vec::new(),
        }],
        &["NHK", "NHKnews", "news"],
        &[DocumentTotals {
            document_id: 0,
            sentences: 1,
            tokens: 2,
        }],
    )
    .unwrap();
    for (pattern, value) in [(0, 0), (0, 2), (1, 1)] {
        if pattern == 1 && !pair_entry {
            continue;
        }
        writer.begin_entry(pattern, 0, &[value]).unwrap();
        writer.add_sentence(0, 0, 1).unwrap();
    }
    writer
        .finish(&CancellationFlag::new())
        .unwrap()
        .publish()
        .unwrap();

    let dictionary = lingrep_core::testing::MiniDictionary::new();
    let args = lingrep_web::Args::try_parse_from([
        "lingrep-web".as_ref(),
        "--dict-path".as_ref(),
        dictionary.path().as_os_str(),
        "--input".as_ref(),
        artifact.path.as_os_str(),
        "--pattern-root".as_ref(),
        root.as_os_str(),
        "--count-table".as_ref(),
        table.as_os_str(),
    ])
    .unwrap();
    let config = lingrep_web::serve::config(&args).unwrap();
    let store = lingrep_web::serve::store(&args, config).await.unwrap();
    Served {
        app: lingrep_web::app_router(store),
        _artifact: artifact,
        _dictionary: dictionary,
    }
}

async fn get(served: &Served, uri: &str) -> (StatusCode, Value) {
    let response = served
        .app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn lines(served: &Served, pattern: &str, surface: &str) -> (StatusCode, Value) {
    let (status, meta) = get(served, "/api/frequency/meta").await;
    assert_eq!(status, StatusCode::OK, "{meta}");
    let query = serde_urlencoded::to_string([
        ("corpus", meta["pairs"][0]["corpus"].as_str().unwrap()),
        ("catalog", meta["pairs"][0]["catalog"].as_str().unwrap()),
        ("pattern", pattern),
        (
            "entry",
            &json!({"kind": "surface", "value": surface}).to_string(),
        ),
    ])
    .unwrap();
    get(served, &format!("/api/frequency/lines?{query}")).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_single_token_entry_lists_its_line_in_a_sentence_without_character_offsets() {
    let served = serve(false).await;
    let (status, body) = lines(&served, "word", "news").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "ok", "{body}");
    assert_eq!(body["total"], 1, "{body}");
    assert_eq!(
        (&body["lines"][0]["left"], &body["lines"][0]["node"]),
        (&json!("NHK "), &json!("news")),
        "{body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_multi_token_surface_entry_without_character_offsets_returns_500_without_the_source_name()
{
    let served = serve(true).await;
    let (status, body) = lines(&served, "pair", "NHKnews").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["code"], "server_error", "{body}");
    assert!(!body.to_string().contains("doc.md"), "{body}");
}
