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
/// The identity itself contains the path.
async fn store_with_verbs(sentence_count: usize, max_kwic: usize) -> (RequestStore, TempDir) {
    let sentence = vec![TokenInfo::default().with_orth("食べる").with_pos("VERB")];
    store_with_sentences(vec![sentence; sentence_count], max_kwic).await
}

async fn store_with_sentences(
    sentences: Vec<Vec<TokenInfo>>,
    max_kwic: usize,
) -> (RequestStore, TempDir) {
    let (fixture, runtime) = runtime_fixture("/private/science-corpus.jsonl", sentences);
    let directory = fixture.directory;
    let corpus_path = directory.path().join("science-corpus.jsonl");
    let dict_path = directory.path().join("unidic-cwj-202512.dic");
    let store = RequestStore::with_runtime(
        Config {
            dict_path: Some(dict_path.display().to_string()),
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
    await_displayed(store, workbench, revision).await;
    revision
}

async fn await_displayed(store: &RequestStore, workbench: &WorkbenchId, revision: RevisionId) {
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

    let mut workbook = open(body_bytes(response).await);
    let sheets = workbook.sheet_names().to_vec();
    assert_eq!(sheets.first().map(String::as_str), Some("About"));
    assert!(
        sheets.iter().any(|name| name == "verb"),
        "a pattern sheet is present: {sheets:?}"
    );

    let about = workbook.worksheet_range("About").unwrap();
    let timestamp = about
        .rows()
        .find_map(|row| match row {
            [Data::String(label), Data::String(value), ..] if label == "Exported at" => Some(value),
            _ => None,
        })
        .expect("About sheet contains the export timestamp");
    assert!(timestamp.contains('T') && timestamp.ends_with("+00:00"));
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
async fn export_admits_the_sentences_the_displayed_run_admitted() {
    let small = TokenInfo::default()
        .with_orth("食べる")
        .with_pos("VERB")
        .with_lemma("食べる");
    let oversized = TokenInfo::default().with_lemma("x".repeat(600_000));
    let (store, _dir) = store_with_sentences(
        vec![vec![small.clone(); 2], vec![oversized; 64], vec![small; 2]],
        1_000,
    )
    .await;
    let workbench = WorkbenchId::new("export-oversized");
    let revision = complete(&store, &workbench, "p := ^ [lemma~/.+/]+ $.").await;
    let snapshot = store
        .coordinators
        .displayed_snapshot(&workbench, revision)
        .await
        .unwrap();
    assert_eq!(
        (
            snapshot.patterns[0].exact_total,
            snapshot.oversized_omissions.as_ref().map(|o| o.count),
        ),
        (2, Some(1)),
        "the displayed run skipped the oversized sentence"
    );

    let response = get(&store, &export_uri(&workbench, revision)).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let mut workbook = open(body_bytes(response).await);

    let rows = workbook
        .worksheet_range("p")
        .unwrap()
        .rows()
        .skip(1)
        .map(<[Data]>::to_vec)
        .collect::<Vec<_>>();
    let row = |sentence: f64| {
        vec![
            Data::Float(sentence),
            Data::Empty,
            Data::String("食べる食べる".into()),
            Data::Empty,
            Data::Empty,
        ]
    };
    assert_eq!(rows, vec![row(0.0), row(2.0)]);
    let about = workbook.worksheet_range("About").unwrap();
    let exact = about
        .rows()
        .find(|row| matches!(row.first(), Some(Data::String(k)) if k == "Total matches"));
    assert_eq!(exact.and_then(|row| row.get(1)), Some(&Data::Float(2.0)));
    let value = |key: &str| {
        about
            .rows()
            .find(|row| matches!(row.first(), Some(Data::String(k)) if k == key))
            .and_then(|row| row.get(1))
            .map(ToString::to_string)
    };
    assert_eq!(
        value("Search scope").as_deref(),
        Some("Whole corpus without filters, excluding skipped sentences")
    );
    let skipped = value("Skipped sentences").expect("the About sheet states the omission");
    assert!(
        skipped.starts_with("Skipped 1 oversized sentence (first: sentence 1, "),
        "{skipped}"
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
    let exact = about
        .rows()
        .find(|row| matches!(row.first(), Some(Data::String(k)) if k == "Total matches"));
    assert_eq!(exact.and_then(|row| row.get(1)), Some(&Data::Float(0.0)));
}

async fn body_text(response: axum::response::Response) -> String {
    String::from_utf8(body_bytes(response).await).unwrap()
}

#[tokio::test]
async fn superseded_revision_is_a_conflict_that_points_to_the_newer_results() {
    let (store, _dir) = store_with_verbs(3, 1_000).await;
    let workbench = WorkbenchId::new("export-stale");
    let first = complete(&store, &workbench, "verb := [upos=VERB].").await;
    complete(&store, &workbench, "verb := [upos=VERB] [upos=VERB].").await;

    let response = get(&store, &export_uri(&workbench, first)).await;
    assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
    let body = body_text(response).await;
    assert!(
        body.contains("A newer search replaced these results."),
        "{body}"
    );
}

#[tokio::test]
async fn displayed_results_export_after_a_later_query_fails() {
    let (store, _dir) = store_with_verbs(3, 1_000).await;
    let workbench = WorkbenchId::new("export-after-failure");
    let displayed = complete(&store, &workbench, "verb := [upos=VERB].").await;
    let slot = store.corpora.resolve(None).unwrap();
    let failed = store
        .coordinators
        .accept_next_revision(workbench.clone(), slot, "verb := [".into())
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let view = store.coordinators.snapshot(&workbench).await.unwrap();
            if view.state.is_terminal() {
                assert_eq!(view.state.current_attempt().unwrap().revision, failed);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let response = get(&store, &export_uri(&workbench, displayed)).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(
        get(&store, &export_uri(&workbench, failed)).await.status(),
        axum::http::StatusCode::CONFLICT,
    );
}

#[tokio::test]
async fn a_search_over_displayed_results_disables_their_download_while_it_runs() {
    let (store, _dir) = store_with_verbs(3, 1_000).await;
    let workbench = WorkbenchId::new("export-waiting");
    complete(&store, &workbench, "verb := [upos=VERB].").await;
    let _permit = store.coordinators.acquire_scan_permit().await;

    let response = Router::new()
        .nest("/api", lingrep_web::handlers::search::routes())
        .with_state(store.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search/export-waiting")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("pattern=noun+%3A%3D+%5Bupos%3DNOUN%5D."))
                .unwrap(),
        )
        .await
        .unwrap();
    let html = body_text(response).await;

    assert!(
        html.contains(r#"<div id="export-control" hx-swap-oob="innerHTML">"#),
        "{html}"
    );
    assert!(
        html.contains(
            r#"<button type="button" class="export-download" disabled aria-describedby="export-waiting-reason">"#
        ),
        "{html}"
    );
    assert!(
        html.contains(
            r#"<span id="export-waiting-reason" class="export-waiting-reason" data-i18n="results.download_waiting">Available when the running query finishes.</span>"#
        ),
        "{html}"
    );
    assert!(!html.contains("api/export/"), "{html}");
}

#[tokio::test]
async fn a_workbench_the_server_does_not_hold_is_not_found_with_a_next_step() {
    let (store, _dir) = store_with_verbs(3, 1_000).await;

    let response = get(&store, "/api/export/evicted-or-restarted/1").await;
    assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
    let body = body_text(response).await;
    assert!(
        body.contains("The server no longer holds these results. Run the search again"),
        "{body}"
    );
}

#[tokio::test]
async fn rejections_follow_the_requested_locale_and_default_to_english() {
    let (store, _dir) = store_with_verbs(3, 1_000).await;
    let workbench = WorkbenchId::new("export-rejection-locale");
    let first = complete(&store, &workbench, "verb := [upos=VERB].").await;
    complete(&store, &workbench, "verb := [upos=VERB] [upos=VERB].").await;
    let stale = export_uri(&workbench, first);
    let gone = "/api/export/evicted-or-restarted/1".to_string();
    for (uri, status, english, japanese) in [
        (
            &stale,
            axum::http::StatusCode::CONFLICT,
            "A newer search replaced these results.",
            "この結果は新しい検索の結果に置き換わりました。元のページに表示されている結果をダウンロードしてください。",
        ),
        (
            &gone,
            axum::http::StatusCode::NOT_FOUND,
            "The server no longer holds these results.",
            "この結果はサーバーに残っていません。もう一度検索してから、ダウンロードしてください。",
        ),
    ] {
        for (query, lang, expected) in [
            ("", "en", english),
            ("?locale=en", "en", english),
            ("?locale=ja", "ja", japanese),
        ] {
            let response = get(&store, &format!("{uri}{query}")).await;
            assert_eq!(response.status(), status, "{uri}{query}");
            let body = body_text(response).await;
            assert!(body.contains(&format!("lang=\"{lang}\"")), "{body}");
            assert!(body.contains(expected), "{body}");
        }
    }
}

#[tokio::test]
async fn results_of_a_corpus_that_became_unavailable_are_a_conflict_with_a_next_step() {
    let (fixture, runtime) = runtime_fixture(
        "export-corrupt",
        vec![vec![
            TokenInfo::default().with_orth("食べる").with_pos("VERB"),
        ]],
    );
    let cache_path = fixture.path.join("tokens.v1.zst");
    let store = RequestStore::with_runtime(
        Config {
            dict_path: None,
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
    let workbench = WorkbenchId::new("export-corrupt");
    let revision = complete(&store, &workbench, "verb := [upos=VERB].").await;
    crate::v6_support::corrupt_first_token_block(&cache_path);

    let damaged = get(&store, &export_uri(&workbench, revision)).await;
    assert_eq!(
        damaged.status(),
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    );
    let body = body_text(damaged).await;
    assert!(body.contains("The corpus data is damaged."), "{body}");

    let unavailable = get(&store, &export_uri(&workbench, revision)).await;
    assert_eq!(unavailable.status(), axum::http::StatusCode::CONFLICT);
    let body = body_text(unavailable).await;
    assert!(
        body.contains("The corpus was rebuilt or became unavailable after these results"),
        "{body}"
    );

    let japanese = get(
        &store,
        &format!("{}?locale=ja", export_uri(&workbench, revision)),
    )
    .await;
    assert_eq!(japanese.status(), axum::http::StatusCode::CONFLICT);
    let body = body_text(japanese).await;
    assert!(
        body.contains("この結果を出したあとで、コーパスが再構築されたか、利用できなくなりました。もう一度検索してから、ダウンロードしてください。"),
        "{body}"
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

#[tokio::test]
async fn export_applies_and_states_the_overlap_setting_of_the_displayed_run() {
    use lingrep_core::fsm::policy_program::Overlaps;
    use lingrep_web::coordinator::RevisionRequest;

    const SOURCE: &str = "long := [upos=VERB] [upos=VERB].\n\
         short := [upos=VERB].\n\
         policy\n  resolve all rank: [longest, start, end, pattern].\nend\n";
    let verb = TokenInfo::default().with_orth("食べる").with_pos("VERB");
    let (store, _dir) = store_with_sentences(vec![vec![verb; 2]], 1_000).await;
    let slot = store.corpora.resolve(None).expect("a default corpus");

    for (overlaps, stated, sheets) in [
        (Overlaps::Catalog, "catalog", vec!["About", "long"]),
        (Overlaps::Keep, "keep", vec!["About", "long", "short"]),
    ] {
        let workbench = WorkbenchId::new(format!("export-overlaps-{stated}"));
        let revision = store
            .coordinators
            .accept_revision(
                workbench.clone(),
                slot,
                RevisionRequest {
                    source: SOURCE.into(),
                    overlaps,
                    ..RevisionRequest::default()
                },
            )
            .await
            .expect("an accepted revision");
        await_displayed(&store, &workbench, revision).await;

        let response = get(&store, &export_uri(&workbench, revision)).await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let mut workbook = open(body_bytes(response).await);
        assert_eq!(workbook.sheet_names().to_vec(), sheets, "{stated}");
        let about = workbook.worksheet_range("About").unwrap();
        let row = about
            .rows()
            .find(|row| matches!(row.first(), Some(Data::String(key)) if key == "Overlap policy"));
        assert_eq!(
            row.and_then(|row| row.get(1)),
            Some(&Data::String(stated.into()))
        );
    }
}

#[tokio::test]
async fn export_about_labels_follow_the_requested_locale_and_default_to_english() {
    let (store, _dir) = store_with_verbs(3, 1_000).await;
    let workbench = WorkbenchId::new("export-locale");
    let revision = complete(&store, &workbench, "verb := [upos=VERB].").await;
    for (query, expected) in [
        ("", "Corpus"),
        ("?locale=en", "Corpus"),
        ("?locale=ja", "コーパス"),
    ] {
        let response = get(
            &store,
            &format!("{}{query}", export_uri(&workbench, revision)),
        )
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let mut workbook = open(body_bytes(response).await);
        let sheet = workbook.worksheet_range("About").unwrap();
        assert_eq!(
            sheet.get_value((0, 0)),
            Some(&Data::String(expected.into()))
        );
        assert!(workbook.sheet_names().iter().any(|name| name == "verb"));
    }
    assert_eq!(
        get(
            &store,
            &format!("{}?locale=fr", export_uri(&workbench, revision))
        )
        .await
        .status(),
        axum::http::StatusCode::BAD_REQUEST
    );
}

/// A pattern sheet's KWIC cells hold each token's exact surface, not the
/// folded `text` the pattern matched on.
#[tokio::test]
async fn export_rows_show_the_exact_surface_of_folded_tokens() {
    let word = |surface: &str, text: &str, pos: &str| {
        TokenInfo::default()
            .with_orth(text)
            .with_surface(surface)
            .with_pos(pos)
    };
    let (store, _dir) = store_with_sentences(
        vec![
            vec![
                word("葛\u{E0100}城", "葛城", "PROPN"),
                word("の", "の", "ADP"),
                word("ｶﾞﾗｽ", "ガラス", "NOUN"),
                word("。", "。", "PUNCT"),
            ],
            vec![word("ガラス", "ガラス", "NOUN")],
        ],
        1_000,
    )
    .await;
    let workbench = WorkbenchId::new("export-surface");
    let revision = complete(&store, &workbench, "glass := [text=ガラス].").await;

    let response = get(&store, &export_uri(&workbench, revision)).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let mut workbook = open(body_bytes(response).await);
    let text = |cell: &Data| match cell {
        Data::String(value) => value.clone(),
        Data::Empty => String::new(),
        other => panic!("{other:?}"),
    };
    // Columns: sentence, left, key, right, sections.
    let kwic: Vec<[String; 3]> = workbook
        .worksheet_range("glass")
        .unwrap()
        .rows()
        .skip(1)
        .map(|row| [text(&row[1]), text(&row[2]), text(&row[3])])
        .collect();
    assert_eq!(
        kwic,
        [
            [
                "葛\u{E0100}城の".to_owned(),
                "ｶﾞﾗｽ".to_owned(),
                "。".to_owned()
            ],
            [String::new(), "ガラス".to_owned(), String::new()],
        ]
    );
}
