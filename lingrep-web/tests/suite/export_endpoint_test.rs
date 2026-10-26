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
//! End-to-end coverage for `GET /api/export/{workbench}/{revision}`.
//!
//! This harness is dictionary-free: it mirrors `detail_revision_test.rs`, driving
//! a real coordinator revision to completion over a hand-built `from_ready` cache.
//! That gives deterministic match counts, which the "export is not bounded by
//! display retention" claim (a pattern matching more sentences than `max_kwic`)
//! needs to assert precisely — the sibling `corpus_context_mapping_test.rs` builds
//! a ready corpus but does not drive a revision, so its helpers are not reusable
//! here. No `LINGREP_DICTIONARY_PATH` is required.

use crate::v6_support::RevisionTestExt;
use std::io::Cursor;

use axum::{Router, body::Body, http::Request, http::header};
use calamine::{Data, Reader, Xlsx};
use http_body_util::BodyExt;
use lingrep_core::token_match::TokenInfo;
use lingrep_web::corpus_set::CorpusName;
use lingrep_web::revision::{RevisionId, WorkbenchId};
use lingrep_web::state::{Config, DetailLimits, RequestStore};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::v6_support::runtime_fixture;

const SPREADSHEET_MIME: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

/// Build a store over `sentence_count` one-token VERB sentences, backed by a
/// `from_ready` cache. `corpus_path`/`dict_path` are path-shaped so the trust
/// boundary is exercised: only their stems and the identity digest may surface.
/// The identity itself is deliberately path-bearing.
async fn store_with_verbs(sentence_count: usize, max_kwic: usize) -> (RequestStore, TempDir) {
    let sentence = vec![TokenInfo::default().with_orth("食べる").with_pos("VERB")];
    let (fixture, runtime) = runtime_fixture(
        "/private/science-corpus.jsonl",
        vec![sentence; sentence_count],
    );
    let directory = fixture.directory;
    let corpus_path = directory.path().join("science-corpus.jsonl");
    let dict_path = directory.path().join("unidic-cwj-202512.dic");
    let store = RequestStore::with_runtime(
        Config {
            dict_path: dict_path.display().to_string(),
            corpora: vec![(
                CorpusName::default_key(),
                corpus_path.clone(),
                "science-corpus".into(),
            )],
            input_format: None,
            markdown: lingrep_core::corpus::MarkdownReaderConfig::new(
                lingrep_core::corpus::SectionPolicy::default(),
            ),
            max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
            detail_limits: DetailLimits { max_kwic },
        },
        runtime,
    );
    (store, directory)
}

/// Drive a revision to a displayed (completed) snapshot. Copied from
/// `detail_revision_test.rs`.
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

fn app(store: RequestStore) -> Router {
    Router::new()
        .nest("/api", lingrep_web::handlers::export::routes())
        .with_state(store)
}

async fn get(store: &RequestStore, uri: &str) -> axum::response::Response {
    app(store.clone())
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn body_bytes(response: axum::response::Response) -> Vec<u8> {
    response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

fn open(bytes: Vec<u8>) -> Xlsx<Cursor<Vec<u8>>> {
    Xlsx::new(Cursor::new(bytes)).unwrap()
}

fn export_uri(workbench: &WorkbenchId, revision: RevisionId) -> String {
    format!("/api/export/{}/{}", workbench.as_str(), revision.0)
}

#[tokio::test]
async fn export_returns_a_well_formed_xlsx_with_headers() {
    let (store, _dir) = store_with_verbs(3, 1_000).await;
    let workbench = WorkbenchId::new("export-headers");
    let revision = complete(&store, &workbench, "verb := [upos=VERB].").await;

    let response = get(&store, &export_uri(&workbench, revision)).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);

    let headers = response.headers();
    assert_eq!(headers.get(header::CONTENT_TYPE).unwrap(), SPREADSHEET_MIME);
    assert!(
        headers
            .get(header::CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains(".xlsx")
    );
    assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");

    let workbook = open(body_bytes(response).await);
    let sheets = workbook.sheet_names().to_vec();
    assert_eq!(sheets.first().map(String::as_str), Some("About"));
    assert!(
        sheets.iter().any(|name| name == "verb"),
        "a pattern sheet is present: {sheets:?}"
    );
}

#[tokio::test]
async fn export_exceeds_display_retention_but_counts_every_match() {
    // Display retention (max_kwic) is 2, yet five sentences match: the export scan
    // is independent of display retention, so the sheet carries all five rows and
    // the About count is the true total.
    let (store, _dir) = store_with_verbs(5, 2).await;
    let workbench = WorkbenchId::new("export-retention");
    let revision = complete(&store, &workbench, "verb := [upos=VERB].").await;

    // The displayed snapshot really is capped at the retention limit.
    let snapshot = store
        .coordinators
        .displayed_snapshot(&workbench, revision)
        .await
        .unwrap();
    assert_eq!(
        snapshot.match_refs.len(),
        2,
        "display retention caps at max_kwic"
    );

    let response = get(&store, &export_uri(&workbench, revision)).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let mut workbook = open(body_bytes(response).await);

    let verb = workbook.worksheet_range("verb").unwrap();
    let data_rows = verb.rows().count() - 1; // minus the header row
    assert_eq!(
        data_rows, 5,
        "the export carries every matching sentence, not just the retained two"
    );

    // About exact_matches is the true corpus total, not the retention cap.
    let about = workbook.worksheet_range("About").unwrap();
    assert!(
        about.rows().any(|row| row.contains(&Data::Float(5.0))),
        "About records the true match count of 5"
    );
}

#[tokio::test]
async fn zero_matches_yield_about_sheet_only() {
    let (store, _dir) = store_with_verbs(3, 1_000).await;
    let workbench = WorkbenchId::new("export-empty");
    // A pattern that matches nothing in a verb-only corpus.
    let revision = complete(&store, &workbench, "noun := [upos=NOUN].").await;

    let response = get(&store, &export_uri(&workbench, revision)).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let mut workbook = open(body_bytes(response).await);

    assert_eq!(workbook.sheet_names().to_vec(), vec!["About".to_string()]);
    let about = workbook.worksheet_range("About").unwrap();
    // exact_matches row is present and zero.
    let exact = about
        .rows()
        .find(|row| matches!(row.first(), Some(Data::String(k)) if k == "exact_matches"));
    assert_eq!(exact.and_then(|row| row.get(1)), Some(&Data::Float(0.0)));
}

#[tokio::test]
async fn superseded_revision_is_a_conflict() {
    let (store, _dir) = store_with_verbs(3, 1_000).await;
    let workbench = WorkbenchId::new("export-stale");
    let first = complete(&store, &workbench, "verb := [upos=VERB].").await;
    // A newer revision supersedes the first.
    complete(&store, &workbench, "verb := [upos=VERB] [upos=VERB].").await;

    let response = get(&store, &export_uri(&workbench, first)).await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::CONFLICT,
        "an older, superseded revision gets the same stale response as the display path"
    );
}

#[tokio::test]
async fn no_workbook_cell_leaks_a_filesystem_path() {
    let (store, dir) = store_with_verbs(3, 1_000).await;
    let workbench = WorkbenchId::new("export-trust");
    let revision = complete(&store, &workbench, "verb := [upos=VERB].").await;

    let response = get(&store, &export_uri(&workbench, revision)).await;
    let mut workbook = open(body_bytes(response).await);

    let prefix = dir.path().to_str().unwrap();
    for name in workbook.sheet_names().to_vec() {
        let range = workbook.worksheet_range(&name).unwrap();
        for row in range.rows() {
            for cell in row {
                if let Data::String(text) = cell {
                    assert!(
                        !text.contains(prefix),
                        "cell leaks the corpus/dictionary temp path: {text:?}"
                    );
                }
            }
        }
    }
}
