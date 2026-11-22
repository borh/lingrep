#![allow(clippy::unwrap_used)]

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use lingrep_core::corpus::{MetadataScalar, MetadataValue};
use lingrep_core::token_match::TokenInfo;
use lingrep_web::coordinator::RevisionRequest;
use lingrep_web::corpus_runtime::{CorpusIdentity, CorpusRuntime};
use lingrep_web::corpus_set::CorpusName;
use lingrep_web::revision::{AttemptPhase, WorkbenchId};
use lingrep_web::server_context::ServerContext;
use lingrep_web::state::{Config, DetailLimits, RequestStore};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::v6_support::{ArtifactFixture, build_artifact_with_metadata, open_artifact};

const SOURCE: &str = r#"key: [$item.lemma]
transform: [{op: "substitute", component: "last", attr: "lemma"}, {op: "join"}]
tags: {origin: "fixture"}
pattern phrase := (?<item> [lemma=cat]) [text=is].
pattern copula := [text=is].
"#;

fn config(path: std::path::PathBuf) -> Config {
    Config {
        dict_path: Some("/unused/question-test.dic".into()),
        corpora: vec![(CorpusName::default_key(), path, "fixture".into())],
        input_format: None,
        markdown: lingrep_core::corpus::MarkdownReaderConfig::new(
            lingrep_core::corpus::SectionPolicy::default(),
        ),
        max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
        detail_limits: DetailLimits { max_kwic: 2 },
    }
}

fn fixture(capacity: usize) -> (ArtifactFixture, RequestStore, String) {
    let sentence = vec![
        TokenInfo::default()
            .with_orth("cat")
            .with_lemma("cat")
            .with_pos("NOUN"),
        TokenInfo::default()
            .with_orth("is")
            .with_lemma("be")
            .with_pos("AUX"),
        TokenInfo::default().with_orth("。"),
    ];
    let metadata = |title: &str, author: &str| {
        vec![
            (
                "title".into(),
                MetadataValue::Scalar(MetadataScalar::String(title.into())),
            ),
            (
                "author".into(),
                MetadataValue::Scalar(MetadataScalar::String(author.into())),
            ),
        ]
        .into_boxed_slice()
    };
    let fixture = build_artifact_with_metadata(
        vec![
            ("a.md", 0, false, vec![sentence.clone(); 35]),
            ("b.md", 1, false, vec![sentence; 7]),
        ],
        vec![],
        vec![metadata("A Study", "A"), metadata("B Study", "B")],
    );
    let artifact = open_artifact(&fixture);
    let identity = lingrep_corpus::format_identity(artifact.analysis_identity().as_bytes());
    let context = Arc::new(ServerContext::new("/unused/question-test.dic").unwrap());
    let runtime = Arc::new(CorpusRuntime::from_ready(
        CorpusIdentity::new(&identity),
        artifact,
        context,
    ));
    let store = RequestStore::with_runtime_capacity(
        config(fixture.path.clone()),
        runtime,
        NonZeroUsize::new(capacity).unwrap(),
    );
    (fixture, store, identity)
}

async fn request(
    app: &Router,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> axum::response::Response {
    let mut request = Request::builder().method(method).uri(path);
    let body = if let Some(body) = body {
        request = request.header("Content-Type", "application/json");
        Body::from(body.to_string())
    } else {
        Body::empty()
    };
    app.clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap()
}

async fn json_response(response: axum::response::Response, expected: StatusCode) -> Value {
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(status, expected, "{}", String::from_utf8_lossy(&bytes));
    serde_json::from_slice(&bytes).unwrap()
}

async fn submit(app: &Router, source: &str, condition: &str, identity: Option<&str>) -> String {
    let response = request(app, Method::POST, "/api/questions/runs", Some(json!({"source":source,"dataset":"default","where":condition,"expected_analysis_identity":identity}))).await;
    json_response(response, StatusCode::OK).await["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn updates(app: &Router, id: &str) -> Vec<Value> {
    let response = request(
        app,
        Method::GET,
        &format!("/api/questions/runs/{id}/events"),
        None,
    )
    .await;
    collect_updates(response).await
}

async fn collect_updates(response: axum::response::Response) -> Vec<Value> {
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["Content-Type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let bytes = tokio::time::timeout(Duration::from_secs(5), response.into_body().collect())
        .await
        .expect("question SSE must terminate")
        .unwrap()
        .to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    text.split("\n\n")
        .filter_map(|frame| {
            let event = frame
                .lines()
                .find_map(|line| line.strip_prefix("event:"))
                .map(str::trim)?;
            let data = frame
                .lines()
                .find_map(|line| line.strip_prefix("data:"))
                .unwrap()
                .trim();
            assert_eq!(event, "update", "{data}");
            Some(serde_json::from_str(data).unwrap())
        })
        .collect()
}

fn expected_match(sequence: u64) -> Value {
    let sentence = sequence / 2;
    let phrase = sequence.is_multiple_of(2);
    let (document, local, source, title, author) = if sentence < 35 {
        (0, sentence, "a.md", "A Study", "A")
    } else {
        (1, sentence - 35, "b.md", "B Study", "B")
    };
    let cat = json!({"index":0,"attributes":{"text":"cat","lemma":"cat"}});
    // The fixture's tokens carry no source spans, so the space between the
    // two Latin words is the one guessed from their script.
    let copula = json!({"index":1,"space_before":true,"attributes":{"text":"is","lemma":"be"}});
    let punctuation = json!({"index":2,"attributes":{"text":"。"}});
    let mut row = json!({
        "sequence":sequence,"pattern":if phrase{"phrase"}else{"copula"},
        "span":{"start":u64::from(!phrase),"end":2},
        "captures":if phrase{vec![json!({"name":"item","hygienic_path":"phrase/item","span":{"start":0,"end":1}})]}else{vec![]},
        "corpus_sentence_ordinal":sentence,"document_ordinal":document,"document_sentence_ordinal":local,
        "source_name":source,"source_metadata":{"document":{"title":title,"author":author}},
        "surface":if phrase{"cat is"}else{"is"},
        "tokens":if phrase{vec![cat.clone(),copula]}else{vec![copula]},
        "left_context":if phrase{vec![]}else{vec![cat]},"right_context":[punctuation]
    });
    if phrase {
        row["unit"] = json!("catbe");
        row["key"] = json!([{"kind":"scalar","name":"$item.lemma","value":"cat"}]);
        row["tags"] = json!([{"name":"origin","value":"fixture"}]);
    }
    row
}

#[tokio::test]
async fn submitted_runs_stream_bounded_previews_and_page_export_every_complete_match_once() {
    let (_fixture, store, identity) = fixture(8);
    let app = lingrep_web::app_router(store.clone());
    let id = submit(&app, SOURCE, "", Some(&identity)).await;
    let frames = updates(&app, &id).await;
    assert!(!frames.is_empty());
    assert_eq!(frames.last().unwrap()["patch"]["phase"], "complete");
    assert_eq!(frames[0]["reset"], true);
    let mut streamed = std::collections::HashSet::new();
    for frame in frames {
        assert_eq!(frame["id"], id);
        for rows in frame["append"].as_object().unwrap().values() {
            for row in rows.as_array().unwrap() {
                assert!(
                    streamed.insert(row["sequence"].as_u64().unwrap()),
                    "SSE appended the same occurrence twice"
                );
            }
        }
    }
    let snapshot = json_response(
        request(
            &app,
            Method::GET,
            &format!("/api/questions/runs/{id}"),
            None,
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(snapshot["counts"], json!({"phrase":42,"copula":42}));
    assert_eq!(snapshot["received"], 84);
    assert_eq!(snapshot["rows"]["phrase"].as_array().unwrap().len(), 30);
    assert_eq!(snapshot["rows"]["copula"].as_array().unwrap().len(), 30);
    for (pattern, offset) in [("phrase", 0), ("copula", 1)] {
        for (index, row) in snapshot["rows"][pattern]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            assert_eq!(*row, expected_match(index as u64 * 2 + offset));
        }
    }
    assert_eq!(streamed.len(), 60);
    let large_page = json_response(
        request(
            &app,
            Method::GET,
            &format!("/api/questions/runs/{id}/documents?pattern=phrase&continuous=1&limit=100"),
            None,
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(large_page["rows"].as_array().unwrap().len(), 42);
    assert!(large_page["next"].is_null());
    let mut paged = Vec::new();
    let mut after = -1;
    loop {
        let page = json_response(request(&app, Method::GET, &format!("/api/questions/runs/{id}/documents?pattern=phrase&continuous=1&limit=13&after={after}"), None).await, StatusCode::OK).await;
        assert_eq!(page["matches_total"], 42);
        assert_eq!(page["start"], paged.len() as u64);
        assert_eq!(page["documents_total"], 2);
        assert_eq!(
            page["documents"][0],
            json!({"ordinal":0,"title":"A Study","author":"A","matches":35})
        );
        paged.extend(page["rows"].as_array().unwrap().iter().cloned());
        if let Some(next) = page["next"].as_i64() {
            after = next;
        } else {
            break;
        }
    }
    assert_eq!(paged.len(), 42);
    for (index, row) in paged.iter().enumerate() {
        assert_eq!(*row, expected_match(index as u64 * 2));
    }
    let scoped = json_response(
        request(
            &app,
            Method::GET,
            &format!("/api/questions/runs/{id}/documents?pattern=phrase&document=1&limit=30"),
            None,
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(scoped["matches_total"], 7);
    assert_eq!(
        scoped["rows"],
        json!((35..42).map(|i| expected_match(i * 2)).collect::<Vec<_>>())
    );
    let response = request(
        &app,
        Method::GET,
        &format!("/api/questions/runs/{id}/export"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = tokio::time::timeout(Duration::from_secs(5), response.into_body().collect())
        .await
        .unwrap()
        .unwrap()
        .to_bytes();
    let records = String::from_utf8(bytes.to_vec())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 86);
    assert!(
        records
            .iter()
            .all(|record| record["schema"] == "lingrep.question-results/v1")
    );
    assert_eq!(records.first().unwrap()["type"], "run");
    assert_eq!(records.last().unwrap()["type"], "complete");
    assert_eq!(
        records.last().unwrap()["data"]["counters"]["matches_found"],
        84
    );
    assert_eq!(records.iter().filter(|r| r["type"] == "run").count(), 1);
    assert_eq!(
        records.iter().filter(|r| r["type"] == "complete").count(),
        1
    );
    let exported = records
        .iter()
        .filter(|r| r["type"] == "match")
        .collect::<Vec<_>>();
    assert_eq!(exported.len(), 84);
    for (sequence, record) in exported.into_iter().enumerate() {
        assert_eq!(record["data"], expected_match(sequence as u64));
    }
}

#[tokio::test]
async fn pending_scope_survives_completion_and_saved_identity_mismatch_publishes_no_evidence() {
    let (_fixture, store, identity) = fixture(8);
    let app = lingrep_web::app_router(store.clone());
    let permit = store.coordinators.acquire_scan_permit().await;
    let condition = "@document.author = \"B\"";
    let id = submit(&app, SOURCE, condition, Some(&identity)).await;
    let pending = json_response(
        request(
            &app,
            Method::GET,
            &format!("/api/questions/runs/{id}"),
            None,
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(pending["phase"], "running");
    assert_eq!(pending["where"], condition);
    let response = request(
        &app,
        Method::GET,
        &format!("/api/questions/runs/{id}/export"),
        None,
    )
    .await;
    json_response(response, StatusCode::CONFLICT).await;
    drop(permit);
    let frames = updates(&app, &id).await;
    assert_eq!(
        frames.last().unwrap()["patch"]["where"]
            .as_str()
            .unwrap_or(condition),
        condition
    );
    let completed = json_response(
        request(
            &app,
            Method::GET,
            &format!("/api/questions/runs/{id}"),
            None,
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(completed["where"], condition);
    assert_eq!(completed["counts"], json!({"phrase":7,"copula":7}));
    assert_eq!(completed["counters"]["counters"]["documents_admitted"], 1);
    assert!(
        completed["rows"]["phrase"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["document_ordinal"] == 1)
    );
    let mismatch = submit(&app, SOURCE, "", Some("different-analysis")).await;
    let frames = updates(&app, &mismatch).await;
    let failure = frames.last().unwrap();
    assert_eq!(failure["patch"]["phase"], "failed");
    assert_eq!(failure["patch"]["error_code"], "analysis_changed");
    assert!(
        failure["patch"]["error"]
            .as_str()
            .unwrap()
            .contains("corpus saved with this question")
    );
    assert!(failure["append"].as_object().unwrap().is_empty());
    json_response(
        request(
            &app,
            Method::GET,
            &format!("/api/questions/runs/{mismatch}/documents?pattern=phrase"),
            None,
        )
        .await,
        StatusCode::CONFLICT,
    )
    .await;
}

#[tokio::test]
async fn evicted_and_absent_runs_return_gone_for_all_result_operations() {
    let (_fixture, store, _) = fixture(1);
    let app = lingrep_web::app_router(store);
    let evicted = submit(&app, SOURCE, "", None).await;
    updates(&app, &evicted).await;
    let replacement = submit(&app, SOURCE, "", None).await;
    updates(&app, &replacement).await;
    for id in [evicted, uuid::Uuid::new_v4().simple().to_string()] {
        for suffix in [
            "",
            "/events",
            "/documents?pattern=phrase",
            "/export",
            "/cancel",
        ] {
            let method = if suffix == "/cancel" {
                Method::POST
            } else {
                Method::GET
            };
            let error = json_response(
                request(
                    &app,
                    method,
                    &format!("/api/questions/runs/{id}{suffix}"),
                    None,
                )
                .await,
                StatusCode::GONE,
            )
            .await;
            assert!(error["error"].as_str().unwrap().contains("expired"));
        }
    }
}

#[tokio::test]
async fn submission_starts_an_unstarted_corpus_and_reports_preparation_failure() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing.jsonl");
    let context = Arc::new(ServerContext::new("/unused/question-test.dic").unwrap());
    let runtime = Arc::new(CorpusRuntime::new(context, &path).unwrap());
    let store = RequestStore::with_runtime(config(path), runtime);
    let app = lingrep_web::app_router(store.clone());
    let id = submit(&app, "p := [].", "", None).await;
    let frames = updates(&app, &id).await;
    assert_eq!(frames.last().unwrap()["patch"]["phase"], "failed");
    assert!(
        frames.last().unwrap()["patch"]["error"]
            .as_str()
            .unwrap()
            .contains("corpus")
    );
    store.coordinators.shutdown().await;
}

#[tokio::test]
async fn a_replaced_revision_terminates_an_open_stream_with_a_failed_update() {
    let (_fixture, store, _) = fixture(8);
    let app = lingrep_web::app_router(store.clone());
    let permit = store.coordinators.acquire_scan_permit().await;
    let id = submit(&app, SOURCE, "", None).await;
    let response = request(
        &app,
        Method::GET,
        &format!("/api/questions/runs/{id}/events"),
        None,
    )
    .await;
    let workbench = WorkbenchId::new(&id);
    let slot = store.corpora.resolve(Some("default")).unwrap();
    let revision = store
        .coordinators
        .accept_revision(
            workbench.clone(),
            slot,
            RevisionRequest {
                source: SOURCE.into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let frames = collect_updates(response).await;
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["id"], id);
    assert_eq!(frames[0]["patch"]["phase"], "failed");
    assert!(
        frames[0]["patch"]["error"]
            .as_str()
            .unwrap()
            .contains("replaced")
    );
    assert_ne!(frames[0]["patch"]["search_complete"], true);
    assert!(frames[0]["append"].as_object().unwrap().is_empty());
    assert!(
        store
            .coordinators
            .cancel(&workbench, revision)
            .await
            .unwrap()
    );
    drop(permit);
}

fn assert_surface_projection(row: &Value) {
    for side in ["tokens", "left_context", "right_context"] {
        for token in row[side].as_array().unwrap() {
            assert_eq!(
                token["attributes"],
                json!({"text":token["attributes"]["text"]}),
                "{side} included an attribute outside the accepted query projection"
            );
        }
    }
}

#[tokio::test]
async fn results_keep_the_available_projection_while_inspection_keeps_all_attributes() {
    let (_fixture, store, identity) = fixture(8);
    let app = lingrep_web::app_router(store);
    let id = submit(
        &app,
        "p := [text=cat & !(dep.rel=nsubj)].",
        "",
        Some(&identity),
    )
    .await;
    for frame in updates(&app, &id).await {
        for rows in frame["append"].as_object().unwrap().values() {
            for row in rows.as_array().unwrap() {
                assert_surface_projection(row);
            }
        }
    }
    let snapshot = json_response(
        request(
            &app,
            Method::GET,
            &format!("/api/questions/runs/{id}"),
            None,
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(snapshot["phase"], "complete", "{snapshot}");
    for row in snapshot["rows"]["p"].as_array().unwrap() {
        assert_surface_projection(row);
    }
    let page = json_response(
        request(
            &app,
            Method::GET,
            &format!("/api/questions/runs/{id}/documents?pattern=p&continuous=1"),
            None,
        )
        .await,
        StatusCode::OK,
    )
    .await;
    for row in page["rows"].as_array().unwrap() {
        assert_surface_projection(row);
    }
    let response = request(
        &app,
        Method::GET,
        &format!("/api/questions/runs/{id}/export"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = tokio::time::timeout(Duration::from_secs(5), response.into_body().collect())
        .await
        .unwrap()
        .unwrap()
        .to_bytes();
    let mut matches = 0;
    for line in String::from_utf8(bytes.to_vec()).unwrap().lines() {
        let record: Value = serde_json::from_str(line).unwrap();
        if record["type"] == "match" {
            assert_surface_projection(&record["data"]);
            matches += 1;
        }
    }
    assert_eq!(matches, 42);
    let inspected = json_response(
        request(
            &app,
            Method::GET,
            "/api/questions/data/default/tokens?sentence=0",
            None,
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        inspected["tokens"][0]["attributes"],
        json!({"text":"cat","lemma":"cat","upos":"NOUN"})
    );
}

#[tokio::test]
async fn oversized_aggregate_preview_terminates_with_a_failed_update_but_keeps_paging_and_export() {
    let text = "猫".repeat(14 * 1024);
    let sentence = vec![TokenInfo::default().with_orth(&text).with_lemma("cat")];
    let (fixture, runtime) =
        crate::v6_support::runtime_fixture("large-question-preview", vec![sentence; 30]);
    let store = RequestStore::with_runtime(config(fixture.path.clone()), runtime);
    let app = lingrep_web::app_router(store.clone());
    let id = submit(&app, "first := [].\nsecond := [].", "", None).await;
    let frames = updates(&app, &id).await;
    let failure = frames.last().unwrap();
    assert_eq!(failure["patch"]["phase"], "failed");
    let message = "The result preview is too large to display. Search fewer patterns.";
    assert_eq!(failure["patch"]["error"], message);
    assert_eq!(
        failure["patch"]["search_complete"], true,
        "the final frame must say that the search completed although its preview failed: {failure}"
    );
    assert_eq!(
        failure["patch"]["exportable"], true,
        "export does not apply the aggregate preview budget: {failure}"
    );
    assert_eq!(failure["patch"]["counts"], json!({"first":30,"second":30}));
    assert_eq!(failure["patch"]["received"], 60);
    assert!(
        frames[..frames.len() - 1]
            .iter()
            .any(|frame| frame["patch"].get("phase").is_none()
                && frame["patch"]["counts"] == json!({"first":30,"second":30})),
        "counts must keep arriving while the preview cannot be displayed: {frames:?}"
    );
    assert!(failure["append"].as_object().unwrap().is_empty());
    let snapshot = json_response(
        request(
            &app,
            Method::GET,
            &format!("/api/questions/runs/{id}"),
            None,
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (
            &snapshot["phase"],
            &snapshot["search_complete"],
            &snapshot["exportable"],
            &snapshot["error"],
            &snapshot["counts"],
            &snapshot["received"],
            &snapshot["rows"],
            &snapshot["definitions"],
        ),
        (
            &json!("failed"),
            &json!(true),
            &json!(true),
            &json!(message),
            &json!({"first":30,"second":30}),
            &json!(60),
            &json!({}),
            &json!(["first", "second"]),
        ),
        "a reload reads the same terminal state the event stream ended with: {snapshot}"
    );
    let inspected = json_response(
        request(
            &app,
            Method::GET,
            "/api/questions/data/default/tokens?sentence=0",
            None,
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(inspected["tokens"][0]["attributes"]["text"], text);
    let page = json_response(
        request(
            &app,
            Method::GET,
            &format!("/api/questions/runs/{id}/documents?pattern=first&continuous=1&limit=30"),
            None,
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(page["rows"].as_array().unwrap().len(), 30);
    let group_bytes = serde_json::to_vec(&page["rows"]).unwrap().len();
    assert!(group_bytes < 4 * 1024 * 1024);
    assert!(group_bytes * 2 > 4 * 1024 * 1024);
    let response = request(
        &app,
        Method::GET,
        &format!("/api/questions/runs/{id}/export"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = tokio::time::timeout(Duration::from_secs(5), response.into_body().collect())
        .await
        .unwrap()
        .unwrap()
        .to_bytes();
    let records: Vec<Value> = std::str::from_utf8(&bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        records
            .iter()
            .filter(|record| record["type"] == "match")
            .count(),
        60
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record["type"] == "complete")
            .count(),
        1
    );
    store.coordinators.shutdown().await;
}

#[tokio::test]
async fn unreferenced_large_attributes_do_not_block_projected_pages_or_complete_export() {
    let ordinary = vec![TokenInfo::default().with_orth("cat").with_lemma("cat")];
    let mut sentences = vec![ordinary; 16];
    sentences.push(vec![
        TokenInfo::default()
            .with_orth("cat")
            .with_lemma("x".repeat(1024 * 1024 + 1)),
    ]);
    let (fixture, runtime) = crate::v6_support::runtime_fixture("fat-unused-attribute", sentences);
    let store = RequestStore::with_runtime(config(fixture.path.clone()), runtime);
    let app = lingrep_web::app_router(store.clone());
    let id = submit(&app, "p := [text=cat].", "", None).await;
    let frames = updates(&app, &id).await;
    assert_eq!(frames.last().unwrap()["patch"]["phase"], "complete");
    let page = json_response(
        request(
            &app,
            Method::GET,
            &format!("/api/questions/runs/{id}/documents?pattern=p&continuous=1&after=15&limit=1"),
            None,
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(page["rows"][0]["corpus_sentence_ordinal"], 16);
    assert_eq!(
        page["rows"][0]["tokens"][0]["attributes"],
        json!({"text":"cat"})
    );
    let inspection = request(
        &app,
        Method::GET,
        "/api/questions/data/default/tokens?sentence=16",
        None,
    )
    .await;
    assert_eq!(inspection.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let response = request(
        &app,
        Method::GET,
        &format!("/api/questions/runs/{id}/export"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let records: Vec<Value> = std::str::from_utf8(&bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        records
            .iter()
            .filter(|record| record["type"] == "match")
            .count(),
        17
    );
    assert_eq!(records.last().unwrap()["type"], "complete");
    for record in records.iter().filter(|record| record["type"] == "match") {
        assert_surface_projection(&record["data"]);
    }
    let selected = submit(&app, "p := [lemma~x].", "", None).await;
    let frames = updates(&app, &selected).await;
    assert_eq!(frames.last().unwrap()["patch"]["phase"], "failed");
    assert_eq!(
        frames.last().unwrap()["patch"]["error"],
        "An example in these results is too large to display."
    );
    assert_eq!(frames.last().unwrap()["patch"]["exportable"], true);
    let records = export_records(&app, &selected).await;
    let types = records
        .iter()
        .map(|record| record["type"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(types, ["run", "match", "complete"]);
    assert_eq!(
        records[1]["data"]["tokens"][0]["attributes"],
        json!({"text":"cat","lemma":"x".repeat(1024 * 1024 + 1)}),
        "export writes the oversized example in full"
    );
    store.coordinators.shutdown().await;
}

async fn export_records(app: &Router, id: &str) -> Vec<Value> {
    let response = request(
        app,
        Method::GET,
        &format!("/api/questions/runs/{id}/export"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    std::str::from_utf8(&bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test]
async fn a_complete_run_exports_an_oversized_example_outside_its_preview() {
    let mut sentences = vec![vec![TokenInfo::default().with_orth("cat").with_lemma("x")]; 30];
    sentences.push(vec![
        TokenInfo::default()
            .with_orth("cat")
            .with_lemma("x".repeat(3 * 1024 * 1024)),
    ]);
    let (fixture, runtime) = crate::v6_support::runtime_fixture("late-oversized", sentences);
    let store = RequestStore::with_runtime(config(fixture.path.clone()), runtime);
    let app = lingrep_web::app_router(store.clone());
    let id = submit(&app, "p := [lemma~x].", "", None).await;
    let frames = updates(&app, &id).await;
    assert_eq!(frames.last().unwrap()["patch"]["phase"], "complete");
    let records = export_records(&app, &id).await;
    let matches = records
        .iter()
        .filter(|record| record["type"] == "match")
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 31);
    assert_eq!(matches[30]["data"]["corpus_sentence_ordinal"], 30);
    assert_eq!(
        matches[30]["data"]["tokens"][0]["attributes"]["lemma"]
            .as_str()
            .unwrap()
            .len(),
        3 * 1024 * 1024
    );
    assert_eq!(records.last().unwrap()["type"], "complete");
    store.coordinators.shutdown().await;
}

#[tokio::test]
async fn a_preview_that_cannot_be_projected_ends_the_stream_only_after_the_search_completes() {
    let mut sentences = vec![vec![
        TokenInfo::default()
            .with_orth("cat")
            .with_lemma("x".repeat(1024 * 1024 + 1)),
    ]];
    sentences.extend(vec![
        vec![
            TokenInfo::default().with_orth("cat").with_lemma("cat")
        ];
        20_000
    ]);
    let (fixture, runtime) = crate::v6_support::runtime_fixture("late-completion", sentences);
    let store = RequestStore::with_runtime(config(fixture.path.clone()), runtime);
    let app = lingrep_web::app_router(store.clone());
    let id = submit(&app, "p := [lemma~x].", "", None).await;
    let frames = updates(&app, &id).await;
    let (last, earlier) = frames.split_last().unwrap();
    assert_eq!(last["patch"]["phase"], "failed");
    assert_eq!(
        last["patch"]["error"],
        "An example in these results is too large to display."
    );
    assert_eq!(last["patch"]["search_complete"], true, "{last}");
    assert_eq!(
        last["patch"]["exportable"], true,
        "export does not apply the per-example display limit: {last}"
    );
    assert_eq!(last["patch"]["counts"], json!({"p":1}));
    for frame in earlier {
        assert!(
            frame["patch"]
                .get("phase")
                .is_none_or(|phase| phase == "running"),
            "{frame}"
        );
        assert!(frame["patch"]["error"].is_null(), "{frame}");
        assert!(frame["append"].as_object().unwrap().is_empty(), "{frame}");
    }
    let view = store
        .coordinators
        .snapshot(&WorkbenchId::new(&id))
        .await
        .unwrap();
    assert_eq!(
        view.state.current_attempt().unwrap().phase,
        AttemptPhase::Completed,
        "the stream ended before the search did"
    );
    let snapshot = json_response(
        request(
            &app,
            Method::GET,
            &format!("/api/questions/runs/{id}"),
            None,
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (
            &snapshot["phase"],
            &snapshot["search_complete"],
            &snapshot["exportable"],
            &snapshot["error"],
            &snapshot["counts"],
        ),
        (
            &json!("failed"),
            &json!(true),
            &json!(true),
            &json!("An example in these results is too large to display."),
            &json!({"p":1}),
        ),
        "{snapshot}"
    );
    let records = export_records(&app, &id).await;
    assert_eq!(
        records
            .iter()
            .filter(|record| record["type"] == "match")
            .count(),
        1
    );
    assert_eq!(records.last().unwrap()["type"], "complete");
    store.coordinators.shutdown().await;
}

#[tokio::test]
async fn a_frequency_occurrence_is_located_by_sentence_and_roles_only_under_the_same_identity() {
    let (_fixture, store, identity) = fixture(8);
    let app = lingrep_web::app_router(store.clone());
    let id = submit(&app, SOURCE, "", Some(&identity)).await;
    assert_eq!(
        updates(&app, &id).await.last().unwrap()["patch"]["phase"],
        "complete"
    );
    let fingerprint = lingrep_web::patterns::web_catalog_fingerprint(SOURCE);
    let item = json!([{"name":"item","start":0,"end":1}]).to_string();
    let locate = |sentence: u64, roles: &str, analysis: &str, fingerprint: &str| {
        let app = app.clone();
        let query = serde_urlencoded::to_string([
            ("pattern", "phrase"),
            ("sentence", &sentence.to_string()),
            ("roles", roles),
            ("analysis", analysis),
            ("fingerprint", fingerprint),
        ])
        .unwrap();
        let path = format!("/api/questions/runs/{id}/occurrence?{query}");
        async move { request(&app, Method::GET, &path, None).await }
    };
    let found = json_response(
        locate(36, &item, &identity, &fingerprint).await,
        StatusCode::OK,
    )
    .await;
    let row = &json_response(
        request(
            &app,
            Method::GET,
            &format!(
                "/api/questions/runs/{id}/documents?pattern=phrase&document={}&after={}&limit=1",
                found["document"],
                found["sequence"].as_u64().unwrap() - 1
            ),
            None,
        )
        .await,
        StatusCode::OK,
    )
    .await["rows"][0];
    assert_eq!(row["sequence"], found["sequence"]);
    assert_eq!(row["corpus_sentence_ordinal"], 36);
    assert_eq!(row["document_ordinal"], 1);
    assert_eq!(row["captures"][0]["name"], "item");
    assert_eq!(row["captures"][0]["span"], json!({"start":0,"end":1}));

    let other = json!([{"name":"item","start":1,"end":2}]).to_string();
    json_response(
        locate(36, &other, &identity, &fingerprint).await,
        StatusCode::NOT_FOUND,
    )
    .await;
    json_response(
        locate(36, &item, &"0".repeat(64), &fingerprint).await,
        StatusCode::CONFLICT,
    )
    .await;
    json_response(
        locate(36, &item, &identity, &"0".repeat(64)).await,
        StatusCode::CONFLICT,
    )
    .await;
    json_response(
        locate(36, "[{", &identity, &fingerprint).await,
        StatusCode::BAD_REQUEST,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_that_names_a_count_only_definition_searches_it() {
    let (_fixture, store, identity) = fixture(8);
    let app = lingrep_web::app_router(store.clone());
    let source = "key: [upos]\nevery := [].\nphrase := [lemma=cat] [text=is].\n";
    assert_eq!(
        lingrep_web::patterns::default_question_definition(source).as_deref(),
        Some("phrase")
    );
    let response = request(&app, Method::POST, "/api/questions/runs", Some(json!({"source":source,"dataset":"default","where":"","pattern":"every","expected_analysis_identity":identity}))).await;
    let id = json_response(response, StatusCode::OK).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let last = updates(&app, &id).await.pop().unwrap();
    assert_eq!(last["patch"]["phase"], "complete", "{last}");
    let snapshot = json_response(
        request(
            &app,
            Method::GET,
            &format!("/api/questions/runs/{id}"),
            None,
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(snapshot["definitions"], json!(["every"]));
    assert_eq!(snapshot["counts"], json!({"every": 126}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_endpoints_complete_while_export_streams_are_paused() {
    let (_fixture, store, identity) = fixture(8);
    let app = lingrep_web::app_router(store.clone());
    let id = submit(&app, SOURCE, "", Some(&identity)).await;
    let frames = updates(&app, &id).await;
    assert_eq!(frames.last().unwrap()["patch"]["phase"], "complete");

    // Open two export streams (equal to READING_CONCURRENCY) and pause them mid-stream
    // after reading the first chunk to simulate slow HTTP clients.
    let mut paused_exports = Vec::new();
    for _ in 0..2 {
        let response = request(
            &app,
            Method::GET,
            &format!("/api/questions/runs/{id}/export"),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body();
        let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert!(!first.is_empty());
        paused_exports.push((first, body));
    }

    // While both export streams are paused between chunks, read endpoints must
    // still acquire reading permits without starving.
    let snapshot_response = tokio::time::timeout(
        Duration::from_secs(2),
        request(
            &app,
            Method::GET,
            &format!("/api/questions/runs/{id}"),
            None,
        ),
    )
    .await
    .expect("paused export streams must not starve concurrent read endpoints");
    let snapshot = json_response(snapshot_response, StatusCode::OK).await;
    assert_eq!(snapshot["counts"], json!({"phrase": 42, "copula": 42}));

    // Draining the paused exports afterward still yields the complete NDJSON stream.
    for (first, body) in paused_exports {
        let mut bytes = first.to_vec();
        let rest = tokio::time::timeout(Duration::from_secs(5), body.collect())
            .await
            .unwrap()
            .unwrap()
            .to_bytes();
        bytes.extend_from_slice(&rest);
        let lines: Vec<&str> = std::str::from_utf8(&bytes).unwrap().lines().collect();
        assert_eq!(lines.len(), 86);
    }
}
