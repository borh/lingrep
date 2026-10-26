#![allow(clippy::unwrap_used, clippy::panic, clippy::single_range_in_vec_init)]

use crate::v6_support::RevisionTestExt;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use lingrep_core::{
    corpus::{
        AdmittedCorpusEvent as Event, AdmittedSentence, Division, DivisionId, DocumentId,
        SentenceId, SourceDocument,
    },
    spill::TempBudget,
    token_match::{BoundTokenSentence, CharSpan, TokenInfo, TokenSchema},
};
use lingrep_corpus::{
    AdmissionIdentity, AnalysisInputs, BuildConfig, CorpusArtifact, CorpusBuilder,
    DictionaryFingerprint, ReaderIdentity,
};
use lingrep_web::{
    corpus_runtime::{CorpusIdentity, CorpusRuntime},
    revision::{RevisionId, WorkbenchId},
    state::{Config, DetailLimits, RequestStore},
};
use std::{ops::Range, path::PathBuf, sync::Arc};
use tower::ServiceExt;

struct Fixture {
    _directory: tempfile::TempDir,
    store: RequestStore,
    router: Router,
}

async fn fixture(words: Vec<String>, paragraphs: Vec<Range<usize>>) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reading.lingcorpus");
    let build_path = path.clone();
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let schema = TokenSchema::unidic_morphology();
                let projection = schema
                    .bind_projection(schema.keys().iter().cloned())
                    .unwrap();
                let mut builder = CorpusBuilder::begin(
                    BuildConfig {
                        analysis: AnalysisInputs::new(
                            ReaderIdentity::from_bytes([1; 32]),
                            AdmissionIdentity::from_bytes([2; 32]),
                            &lingrep_corpus::vibrato_fixture_descriptor(),
                        ),
                        analyzer: lingrep_corpus::AnalyzerRecord::vibrato_fixture(),
                        dictionary: DictionaryFingerprint::from_bytes([3; 32]),
                        trust: lingrep_corpus::TrustClass::LocallyExecuted,
                        temp_budget: Arc::new(TempBudget::new(32 * 1024 * 1024)),
                        sentence_metadata_keys: Vec::new(),
                    },
                    &build_path,
                )
                .await
                .unwrap();
                let sources = (0..2)
                    .map(|id| SourceDocument {
                        ordinal: id,
                        logical_name: format!("doc-{id}.md"),
                        path: PathBuf::from(format!("/unused/doc-{id}.md")),
                        target_group_ordinal: id,
                        external_target: false,
                    })
                    .collect::<Vec<_>>();
                let mut ordinal = 0;
                for source in &sources {
                    builder.register_source_document(source).unwrap();
                    let observation = builder.source_observation();
                    observation.begin_source(&source.logical_name).unwrap();
                    observation
                        .update_source_bytes(source.logical_name.as_bytes())
                        .unwrap();
                    observation.end_source().unwrap();
                    let document_id = DocumentId::new(source.ordinal);
                    builder
                        .push_corpus_event(&Event::DocumentStart {
                            id: document_id,
                            source_name: source.logical_name.clone(),
                            metadata: lingrep_core::corpus::metadata_from_json(
                                serde_json::json!({
                                    "title": if source.ordinal == 0 { "<Title & text>" } else { "OTHER_DOCUMENT_METADATA" },
                                    "authors": ["Alice", "Bob"],
                                    "year": 2024,
                                    "published": true
                                }),
                                "reading fixture",
                            ).unwrap(),
                        })
                        .await
                        .unwrap();
                    if source.ordinal == 0 {
                        builder.push_corpus_event(&Event::DivisionStart(Arc::new(Division {
                            id: DivisionId(0), source_id: None, document_id,
                            kind: "section".into(), parent_id: None,
                            metadata: lingrep_core::corpus::metadata_from_json(
                                serde_json::json!({"title": "Rural migration <study>", "level": 1}),
                                "reading section fixture",
                            ).unwrap(),
                        }))).await.unwrap();
                    }
                    let document_words = if source.ordinal == 0 {
                        words.clone()
                    } else {
                        vec!["OTHER_DOCUMENT_SECRET".into()]
                    };
                    for (index, word) in document_words.iter().enumerate() {
                        if source.ordinal == 0 {
                            for (id, range) in paragraphs
                                .iter()
                                .enumerate()
                                .filter(|(_, range)| range.start == index)
                            {
                                let parent_id = paragraphs
                                    .iter()
                                    .enumerate()
                                    .take(id)
                                    .rfind(|(_, other)| {
                                        other.start <= range.start && range.end <= other.end
                                    })
                                    .map(|(parent, _)| DivisionId(parent as u64 + 1))
                                    .or(Some(DivisionId(0)));
                                builder
                                    .push_corpus_event(&Event::DivisionStart(Arc::new(Division {
                                        id: DivisionId(id as u64 + 1),
                                        source_id: None,
                                        document_id,
                                        kind: "paragraph".into(),
                                        parent_id,
                                        metadata: Box::new([]),
                                    })))
                                    .await
                                    .unwrap();
                            }
                        }
                        let text = format!("  {word} \n");
                        let tokens = vec![TokenInfo::default().with_orth(word)];
                        let bound = BoundTokenSentence::project(&projection, &tokens)
                            .unwrap()
                            .with_char_offsets(Some(Arc::new(vec![CharSpan::new(
                                2,
                                2 + word.chars().count() as u32,
                            )])));
                        let location = builder
                            .push_corpus_event(&Event::Sentence(AdmittedSentence {
            annotations: Box::new([]),
                                id: SentenceId::new(ordinal),
                                document_id,
                                document_sentence_ordinal: index as u64,
                                text,
                                words: Some(bound.clone()),
                                enhanced_graph: None,
                                structure: Default::default(),
                                active_labels: if source.ordinal != 0 {
                                    Box::new([])
                                } else if index == 0 { Box::new(["abstract".into(), "introduction".into()]) } else { Box::new(["introduction".into()]) },
                                metadata: Box::new([]),
                            }))
                            .await
                            .unwrap()
                            .unwrap();
                        builder
                            .push_bound_sentence(&location, bound, &projection)
                            .await
                            .unwrap();
                        ordinal += 1;
                        if source.ordinal == 0 {
                            for (id, _) in paragraphs
                                .iter()
                                .enumerate()
                                .rev()
                                .filter(|(_, range)| range.end == index + 1)
                            {
                                builder
                                    .push_corpus_event(&Event::DivisionEnd {
                                        id: DivisionId(id as u64 + 1),
                                    })
                                    .await
                                    .unwrap();
                            }
                        }
                    }
                    if source.ordinal == 0 {
                        builder.push_corpus_event(&Event::DivisionEnd { id: DivisionId(0) }).await.unwrap();
                    }
                    builder
                        .push_corpus_event(&Event::DocumentEnd { id: document_id })
                        .await
                        .unwrap();
                }
                builder
                    .finish(crate::v6_support::discovery_report(&sources))
                    .await
                    .unwrap();
            });
    })
    .join()
    .unwrap();
    let context =
        Arc::new(lingrep_web::server_context::ServerContext::new("/unused/dict").unwrap());
    let runtime = Arc::new(CorpusRuntime::from_ready(
        CorpusIdentity::new("reading-fixture"),
        CorpusArtifact::open(&path).unwrap(),
        context,
    ));
    let (store, router) = bind_runtime(runtime, "hit := HIT.").await;
    Fixture {
        _directory: directory,
        store,
        router,
    }
}

async fn bind_runtime(runtime: Arc<CorpusRuntime>, source: &str) -> (RequestStore, Router) {
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
    complete(&store, source).await;
    let router = Router::new()
        .merge(lingrep_web::handlers::pattern_detail::routes())
        .with_state(store.clone());
    (store, router)
}

async fn complete(store: &RequestStore, source: &str) -> RevisionId {
    let workbench = WorkbenchId::new("reading");
    let revision = store
        .coordinators
        .accept_next_revision(
            workbench.clone(),
            store.corpora.resolve(None).unwrap(),
            source.into(),
        )
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if store
                .coordinators
                .snapshot(&workbench)
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

async fn get(fixture: &Fixture, suffix: &str) -> (StatusCode, String) {
    let response = fixture
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/hit-context/reading/1/0/0/{suffix}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert!(bytes.len() <= 2 * 1024 * 1024);
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

fn ordinals(html: &str) -> Vec<u64> {
    html.split("data-reading-sentence=\"")
        .skip(1)
        .map(|tail| tail.split('"').next().unwrap().parse().unwrap())
        .collect()
}

#[tokio::test]
async fn paragraph_and_document_context_preserve_text_boundaries_and_match_counts() {
    let f = fixture(
        vec![
            "HIT".into(),
            "<script>&文".into(),
            "OUTSIDE_PARAGRAPH".into(),
        ],
        vec![0..2, 2..3],
    )
    .await;
    let (status, paragraph) = get(&f, "paragraph").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ordinals(&paragraph), [0, 1]);
    assert!(paragraph.contains("  <mark>HIT</mark> \n"), "{paragraph}");
    assert!(paragraph.contains("&#60;script&#62;&#38;文"));
    assert!(!paragraph.contains("<script>"));
    assert!(!paragraph.contains("class=\"reading-reconstructed\""));
    assert!(paragraph.contains("<span>abstract</span>"));
    for html in [
        &paragraph,
        &get(&f, "document").await.1,
        &get(&f, "document?start=2").await.1,
    ] {
        assert!(html.contains("data-reading-metadata"));
        assert!(html.contains("<h3>&#60;Title &#38; text&#62;</h3>"));
        assert!(html.contains("Rural migration &#60;study&#62;"));
        assert!(html.contains("data-reading-section"));
        assert!(html.contains("&#60;Title &#38; text&#62;"));
        assert!(html.contains("<li>Alice</li><li>Bob</li>"));
        assert!(html.contains("<li>2024</li>"));
        assert!(html.contains("<li>true</li>"));
        assert!(!html.contains("OTHER_DOCUMENT_METADATA"));
    }
    let (status, document) = get(&f, "document").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ordinals(&document), [0, 1, 2]);
    assert!(!document.contains("OTHER_DOCUMENT_SECRET"));
    assert_eq!(document.matches("<mark>").count(), 1);
    assert!(document.contains("OUTSIDE_PARAGRAPH"));
    assert_eq!(document.matches("data-reading-section").count(), 2);
    assert!(document.contains("<span>introduction</span>"));
    let middle = get(&f, "document?start=2").await.1;
    assert_eq!(middle.matches("data-reading-section").count(), 1);
    assert!(middle.contains("<span>introduction</span>"));
    assert!(!middle.contains("<span>abstract</span>"));
    let backward = get(&f, "document?before=3").await.1;
    assert_eq!(backward.matches("data-reading-section").count(), 2);
    let snapshot = f
        .store
        .coordinators
        .displayed_snapshot(&WorkbenchId::new("reading"), RevisionId(1))
        .await
        .unwrap();
    assert_eq!(snapshot.patterns[0].exact_total, 1);
    for suffix in [
        "document?start=3",
        "document?start=18446744073709551615",
        "document?before=0",
        "document?start=0&before=2",
        "paragraph?paragraph=999",
        "document?paragraph=0",
        "other",
    ] {
        assert_eq!(get(&f, suffix).await.0, StatusCode::BAD_REQUEST, "{suffix}");
    }
}

#[tokio::test]
async fn missing_paragraphs_fall_back_to_document_and_nested_paragraphs_require_a_choice() {
    let missing = fixture(vec!["HIT".into()], vec![]).await;
    let html = get(&missing, "paragraph").await.1;
    assert!(html.contains("data-reading-unavailable"));
    assert!(ordinals(&html).is_empty());
    assert_eq!(ordinals(&get(&missing, "document").await.1), [0]);
    let multiple = fixture(
        vec!["HIT".into(), "SECOND".into(), "THIRD".into()],
        vec![0..3, 0..2],
    )
    .await;
    let html = get(&multiple, "paragraph").await.1;
    assert!(html.contains("several authored paragraphs"));
    assert!(ordinals(&html).is_empty());
    assert_eq!(
        ordinals(&get(&multiple, "paragraph?paragraph=2").await.1),
        [0, 1]
    );
    assert_eq!(
        ordinals(&get(&multiple, "paragraph?paragraph=1").await.1),
        [0, 1, 2]
    );
}

#[tokio::test]
async fn byte_limited_pages_cover_the_document_in_both_directions_without_gaps() {
    let mut words = vec!["HIT".into()];
    words.extend((0..24).map(|_| "&".repeat(90 * 1024)));
    words.push("z".repeat(140 * 1024));
    let f = fixture(words, vec![0..26]).await;
    let mut visited = Vec::new();
    let mut start = 0;
    while start < 26 {
        let (status, html) = get(&f, &format!("document?start={start}")).await;
        assert_eq!(status, StatusCode::OK);
        let ids = ordinals(&html);
        assert!(!ids.is_empty());
        assert!(ids.len() <= 20);
        start = ids.last().unwrap() + 1;
        visited.extend(ids);
    }
    assert_eq!(visited, (0..26).collect::<Vec<_>>());
    assert!(
        get(&f, "document?start=25")
            .await
            .1
            .contains("Sentence text is unavailable within the reading limits")
    );
    let mut before = 26;
    let mut reverse = Vec::new();
    while before > 0 {
        let (status, html) = get(&f, &format!("document?before={before}")).await;
        assert_eq!(status, StatusCode::OK);
        let ids = ordinals(&html);
        assert!(!ids.is_empty());
        before = ids[0];
        reverse.extend(ids.into_iter().rev());
    }
    reverse.reverse();
    assert_eq!(reverse, visited);
}

#[tokio::test]
async fn a_context_request_queued_behind_a_scan_rechecks_the_bound_artifact() {
    let f = fixture(vec!["HIT".into()], vec![0..1]).await;
    let permit = f.store.coordinators.acquire_scan_permit().await;
    let mut request = Box::pin(get(&f, "document"));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), &mut request)
            .await
            .is_err()
    );
    let runtime = f
        .store
        .coordinators
        .bound_corpus(&WorkbenchId::new("reading"))
        .await
        .unwrap();
    runtime.invalidator().invalidate_corrupt().await;
    drop(permit);
    assert_eq!(request.await.0, StatusCode::CONFLICT);
}

#[tokio::test]
async fn reading_rejects_a_replaced_result_revision() {
    let f = fixture(vec!["HIT".into()], vec![0..1]).await;
    complete(&f.store, "new := HIT.").await;
    assert_eq!(get(&f, "document").await.0, StatusCode::CONFLICT);
}

#[tokio::test]
async fn missing_written_forms_are_omitted_without_invalidating_the_artifact() {
    let (artifact, runtime) = crate::v6_support::runtime_fixture(
        "missing-written-form",
        vec![vec![TokenInfo::default().with_lemma("HIT")]],
    );
    let (store, router) = bind_runtime(runtime.clone(), "hit := [lemma=HIT].").await;
    let f = Fixture {
        _directory: artifact.directory,
        store,
        router,
    };
    let (status, html) = get(&f, "document").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Sentence text is unavailable within the reading limits"));
    assert!(runtime.ready().await.is_some());
}

#[tokio::test]
async fn search_scope_includes_before_matching_and_keeps_revision_totals_consistent() {
    let f = fixture(vec!["HIT".into(), "HIT".into(), "HIT".into()], vec![0..3]).await;
    let router = lingrep_web::app_router(f.store.clone());
    for (offset, scope, scanned, hits, excluded) in [
        (
            0,
            serde_json::json!({"included_sections":["abstract"], "include_unlabelled":true}),
            2,
            1,
            2,
        ),
        (
            1,
            serde_json::json!({"included_sections":["abstract"], "include_unlabelled":false}),
            1,
            1,
            3,
        ),
        (
            2,
            serde_json::json!({"included_sections":null, "include_unlabelled":true}),
            4,
            3,
            0,
        ),
        (
            3,
            serde_json::json!({"included_sections":[], "include_unlabelled":false}),
            0,
            0,
            4,
        ),
        (
            4,
            serde_json::json!({"included_sections":["introduction"], "include_unlabelled":false}),
            3,
            3,
            1,
        ),
    ] {
        let body = serde_urlencoded::to_string([
            ("pattern", "hit := HIT.".to_owned()),
            ("search_scope", scope.to_string()),
            ("document_reach", "true".to_owned()),
        ])
        .unwrap();
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/search/reading")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let revision = RevisionId(2 + offset);
        let snapshot = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(snapshot) = f
                    .store
                    .coordinators
                    .displayed_snapshot(&WorkbenchId::new("reading"), revision)
                    .await
                {
                    break snapshot;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(snapshot.sentences_matched, scanned);
        assert_eq!(snapshot.observed_sentences, scanned);
        assert_eq!(snapshot.patterns[0].exact_total, hits);
        assert_eq!(snapshot.excluded_sentences, excluded);
        assert_eq!(
            snapshot.search_scope.included_sections,
            serde_json::from_value::<Option<Vec<String>>>(scope["included_sections"].clone())
                .unwrap()
        );
        let detail = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/pattern-detail/reading/{}/0", revision.0))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail.status(), StatusCode::OK);
        let detail = String::from_utf8(
            detail
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        if let Some(selected) = &snapshot.search_scope.included_sections {
            for label in ["abstract", "introduction"] {
                assert_eq!(
                    detail.contains(&format!("data-search-value=\"{label}\"")),
                    selected.iter().any(|name| name == label),
                    "section distribution must follow the applied scope: {label}"
                );
            }
        } else {
            assert!(detail.contains("data-search-value=\"abstract\""));
            assert!(detail.contains("data-search-value=\"introduction\""));
        }
        if offset == 3 {
            assert!(
                snapshot
                    .match_refs
                    .iter()
                    .all(|hit| hit.sentence_index != 0)
            );
        }
        if offset == 0 {
            let sections = match &snapshot.sections {
                lingrep_web::section_tally::SectionAvailability::Available(sections) => sections,
                _ => panic!("section tally unavailable"),
            };
            let abstract_slot = sections.slots().iter().position(|slot| matches!(slot, lingrep_web::section_tally::SectionSlot::Label(label) if label == "abstract")).unwrap();
            assert_eq!(sections.universe(abstract_slot), 1);
            assert_eq!(snapshot.documents.as_ref().unwrap().documents_scanned, 2);
            let link_body = serde_urlencoded::to_string([
                ("pattern", "hit := HIT.".to_owned()),
                ("search_scope", scope.to_string()),
            ])
            .unwrap();
            let link = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/permalink")
                        .header("content-type", "application/x-www-form-urlencoded")
                        .body(Body::from(link_body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(link.status(), StatusCode::OK);
            let link: serde_json::Value =
                serde_json::from_slice(&link.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            let target = link["path"].as_str().unwrap();
            assert!(target.contains("&s="));
            let landing = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/{target}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(landing.status(), StatusCode::OK);
            let landing = String::from_utf8(
                landing
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .to_vec(),
            )
            .unwrap();
            assert!(landing.contains("value=\"{&#34;included_sections&#34;:[&#34;abstract&#34;],&#34;include_unlabelled&#34;:true}\""));
            let context_request = |query: &str| {
                Request::builder()
                    .uri(format!(
                        "/api/hit-context/reading/2/0/0/document?start=0{query}"
                    ))
                    .body(Body::empty())
                    .unwrap()
            };
            let hidden = router.clone().oneshot(context_request("")).await.unwrap();
            assert_eq!(hidden.status(), StatusCode::OK);
            let hidden = String::from_utf8(
                hidden
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .to_vec(),
            )
            .unwrap();
            assert!(hidden.contains("data-reading-excluded"));
            assert!(!hidden.contains("data-reading-sentence=\"1\""));
            assert!(hidden.contains("data-reading-sentence=\"0\""));
            let revealed = router
                .clone()
                .oneshot(context_request("&show_excluded=true"))
                .await
                .unwrap();
            assert_eq!(revealed.status(), StatusCode::OK);
            let revealed = String::from_utf8(
                revealed
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .to_vec(),
            )
            .unwrap();
            assert!(revealed.contains("data-reading-sentence=\"0\""));
            assert!(revealed.contains("reading-outside-scope"));
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/api/export/reading/2")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            use calamine::Reader;
            let mut workbook = calamine::Xlsx::new(std::io::Cursor::new(
                response
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .to_vec(),
            ))
            .unwrap();
            let about = workbook.worksheet_range("About").unwrap();
            assert!(
                about
                    .rows()
                    .any(|row| row.iter().any(|cell| cell.to_string().contains("abstract")))
            );
            let pattern = workbook.worksheet_range("hit").unwrap();
            assert_eq!(pattern.rows().count(), 2);
            assert_eq!(
                f.store
                    .coordinators
                    .displayed_snapshot(&WorkbenchId::new("reading"), revision)
                    .await
                    .unwrap()
                    .patterns[0]
                    .exact_total,
                hits
            );
        }
    }
}
