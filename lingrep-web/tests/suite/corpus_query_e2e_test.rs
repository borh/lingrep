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
use std::fs;
use std::sync::Arc;
use std::time::Duration;

use axum::{Router, body::Body, http::Request};
use http_body_util::BodyExt;
use lingrep_core::testing::MiniDictionary;
use lingrep_web::coordinator::CoordinatorRegistry;
use lingrep_web::corpus_runtime::CorpusRuntime;
use lingrep_web::corpus_set::{CorpusName, CorpusSlot};
use lingrep_web::revision::{AttemptPhase, WorkbenchId};
use lingrep_web::state::{Config, DetailLimits, RequestStore};
use tempfile::tempdir;
use tower::ServiceExt;

async fn ready_runtime(corpus: &std::path::Path) -> (MiniDictionary, Arc<CorpusRuntime>) {
    let dictionary = MiniDictionary::new();
    let context =
        Arc::new(lingrep_web::server_context::ServerContext::new(dictionary.path()).unwrap());
    let runtime = Arc::new(CorpusRuntime::new(context, corpus).unwrap());
    runtime.start_or_observe().await;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if runtime.ready().await.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    (dictionary, runtime)
}

#[tokio::test]
async fn real_artifact_regions_render_broadest_earliest_first() {
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("markdown-corpus");
    fs::create_dir(&corpus).unwrap();
    fs::write(
        corpus.join("paper.md"),
        ":::: {.z-outer}\n一。\n::: {.a,b}\n二。\n:::\n三。\n::::\n",
    )
    .unwrap();
    let (_dictionary, runtime) = ready_runtime(&corpus).await;
    let ready = runtime.ready().await.unwrap();
    let provenance = ready.artifact.provenance_view().await.unwrap();
    let sentence = ready
        .artifact
        .sentence_provenance(&provenance, 1)
        .await
        .unwrap();

    assert_eq!(
        &*lingrep_web::provenance_view::HitProvenance::from_sentence(&sentence).sections,
        &["z-outer", "a,b"]
    );
    assert_eq!(
        provenance.section_coverage(8).await.unwrap(),
        [
            lingrep_corpus::SectionCoverage {
                label: "z-outer".into(),
                coverage: 3,
            },
            lingrep_corpus::SectionCoverage {
                label: "a,b".into(),
                coverage: 1,
            },
        ]
    );
}

#[tokio::test]
async fn real_artifact_numeric_and_opaque_metadata_reaches_web_display() {
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("markdown-corpus");
    fs::create_dir(&corpus).unwrap();
    fs::write(
        corpus.join("paper.md"),
        "---\ninteger: 2016\nfloat: 2.75\nopaque:\n  x: 1\n---\n文。\n",
    )
    .unwrap();
    let (_dictionary, runtime) = ready_runtime(&corpus).await;
    let ready = runtime.ready().await.unwrap();
    let provenance = ready.artifact.provenance_view().await.unwrap();
    let sentence = ready
        .artifact
        .sentence_provenance(&provenance, 0)
        .await
        .unwrap();
    let raw: serde_json::Value =
        serde_json::from_str(sentence.rich.metadata_json.as_deref().unwrap()).unwrap();
    let display = lingrep_web::provenance_view::HitProvenance::from_sentence(&sentence);

    assert_eq!(raw["integer"], serde_json::json!(2016));
    assert_eq!(raw["float"], serde_json::json!(2.75));
    assert_eq!(display.document[0].values[0], "2.75");
    assert_eq!(display.document[1].values[0], "2016");
    assert_eq!(display.document[2].values[0], r#"{"x":1}"#);
}

#[tokio::test]
async fn jsonl_sentence_arrays_reach_a_completed_verb_query() {
    let dictionary = MiniDictionary::new();
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    fs::write(
        &corpus,
        "{\"title\":\"ignored\",\"sentences\":[\"私は食べる。\",\"彼は走る。\"]}\n",
    )
    .unwrap();
    let context =
        Arc::new(lingrep_web::server_context::ServerContext::new(dictionary.path()).unwrap());
    let runtime = Arc::new(CorpusRuntime::new(context, &corpus).unwrap());
    runtime.start_or_observe().await;
    let slot = CorpusSlot::new(
        CorpusName::default_key(),
        Arc::clone(&runtime),
        "e2e".into(),
    );
    let registry = CoordinatorRegistry::new(10);
    let workbench = WorkbenchId::new("jsonl-e2e");
    registry
        .accept_next_revision(workbench.clone(), &slot, "verb := [upos=VERB].".into())
        .await;

    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let view = registry.snapshot(&workbench).await.unwrap();
            if view.state.current_attempt().unwrap().phase == AttemptPhase::Completed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let snapshot = registry.snapshot(&workbench).await.unwrap();
    assert_eq!(
        snapshot.state.displayed_snapshot().unwrap().patterns[0].exact_total,
        2
    );
    let ready = runtime.ready().await.unwrap();
    assert_eq!(ready.sentence_count().unwrap(), 2);
    let provenance = ready.artifact.provenance_view().await.unwrap();
    assert_eq!(provenance.sentence_count(), 2);
    let sentence = ready
        .artifact
        .sentence_provenance(&provenance, 0)
        .await
        .unwrap();
    assert_eq!(
        lingrep_web::provenance_view::HitProvenance::from_sentence(&sentence).document[0].values[0],
        "ignored"
    );
}

#[tokio::test]
async fn markdown_directory_reaches_a_completed_verb_query() {
    let dictionary = MiniDictionary::new();
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("markdown-corpus");
    fs::create_dir(&corpus).unwrap();
    fs::write(
        corpus.join("01.md"),
        "---\ntitle: First paper\nyear: 2013\n---\n# First\n私は食べる。\n::: {.references}\n彼は走る。\n:::\n",
    )
    .unwrap();
    fs::write(corpus.join("02.md"), "<sup>7\n彼は走る。\n").unwrap();

    let context =
        Arc::new(lingrep_web::server_context::ServerContext::new(dictionary.path()).unwrap());
    let runtime = Arc::new(CorpusRuntime::new(context, &corpus).unwrap());
    runtime.start_or_observe().await;
    let slot = CorpusSlot::new(
        CorpusName::default_key(),
        Arc::clone(&runtime),
        "e2e".into(),
    );
    let registry = CoordinatorRegistry::new(10);
    let workbench = WorkbenchId::new("markdown-e2e");
    registry
        .accept_next_revision(workbench.clone(), &slot, "verb := [upos=VERB].".into())
        .await;

    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let view = registry.snapshot(&workbench).await.unwrap();
            if view.state.current_attempt().unwrap().phase == AttemptPhase::Completed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let snapshot = registry.snapshot(&workbench).await.unwrap();
    assert_eq!(
        snapshot.state.displayed_snapshot().unwrap().patterns[0].exact_total,
        2
    );
    let ready = runtime.ready().await.unwrap();
    assert_eq!(ready.sentence_count().unwrap(), 3);
    let provenance = ready.artifact.provenance_view().await.unwrap();
    assert_eq!(provenance.sentence_count(), 3);
    let sentence = ready
        .artifact
        .sentence_provenance(&provenance, 0)
        .await
        .unwrap();
    let metadata = lingrep_web::provenance_view::HitProvenance::from_sentence(&sentence);
    assert!(
        metadata
            .document
            .iter()
            .any(|entry| entry.key == "year" && entry.values[0] == "2013")
    );
}

#[tokio::test]
async fn kwic_views_surface_section_and_document_provenance() {
    let dictionary = MiniDictionary::new();
    let directory = tempdir().unwrap();
    let corpus = directory.path().join("markdown-corpus");
    fs::create_dir(&corpus).unwrap();
    fs::write(
        corpus.join("01.md"),
        "---\ntitle: First paper\nyear: 2013\n---\n# First\n私は食べる。\n::: {.methods}\n彼は食べる。\n:::\n",
    )
    .unwrap();

    let context =
        Arc::new(lingrep_web::server_context::ServerContext::new(dictionary.path()).unwrap());
    let runtime = Arc::new(CorpusRuntime::new(context, &corpus).unwrap());
    runtime.start_or_observe().await;
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
    let workbench = WorkbenchId::new("prov-e2e");
    let slot = store
        .corpora
        .resolve(None)
        .expect("a validated configuration declares a default corpus");
    store
        .coordinators
        .accept_next_revision(workbench.clone(), slot, "verb := [upos=VERB].".into())
        .await;
    tokio::time::timeout(Duration::from_secs(30), async {
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

    async fn get(store: &RequestStore, uri: &str) -> String {
        let response = Router::new()
            .merge(lingrep_web::handlers::pattern_detail::routes())
            .with_state(store.clone())
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(response.status().is_success());
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

    // Section chips attach to the correct row: sentence 0 (私は食べる, outside
    // any div) has none; sentence 1 (彼は食べる, inside `.methods`) has one.
    let list = get(&store, "/pattern-detail/prov-e2e/1/0").await;
    let row = |ordinal: usize| -> String {
        let segment = list
            .split("<tr class=\"kwic-row\"")
            .find(|segment| segment.contains(&format!("data-hit-ordinal=\"{ordinal}\"")))
            .unwrap_or_else(|| panic!("no KWIC row for hit ordinal {ordinal}"));
        // Bound the segment to this single row so a later row or trailing markup
        // can never make an assertion about this row pass vacuously.
        segment
            .split_once("</tr>")
            .map_or(segment, |(row, _)| row)
            .to_string()
    };
    assert!(
        !row(0).contains("methods"),
        "the sentence outside the div must carry no methods chip"
    );
    assert!(
        row(1).contains("kwic-section-label") && row(1).contains("methods"),
        "the .methods sentence's row must carry its section label"
    );

    // The section distribution surfaces the `.methods` section with its count,
    // and the rows carry machine-readable membership ids.
    assert!(
        list.contains("class=\"detail-view section-distribution\""),
        "the section distribution block is present"
    );
    assert!(
        list.contains("class=\"section-label\">methods<"),
        "the methods section is listed"
    );
    // The methods button's count reflects its one hit (sentence 1 only). Scope
    // the count assertion to that button's own segment.
    let methods_button = list
        .split("class=\"section-filter\"")
        .find(|segment| segment.contains("class=\"section-label\">methods<"))
        .expect("methods section button present");
    assert!(
        methods_button.contains(">1</span>"),
        "the methods section count is 1"
    );
    // sentence 1 (彼は食べる, inside `.methods`) is the hit that carries an id;
    // sentence 0 (私は食べる, outside any div) carries an empty membership.
    assert!(
        row(1).contains("data-section-ids=\"0 \""),
        "the .methods row carries the section id"
    );
    assert!(
        row(0).contains("data-section-ids=\"\""),
        "the out-of-section row carries an empty membership"
    );

    // The hit-detail panel proves the same association per hit, and shows the
    // document metadata + source basename.
    let detail0 = get(&store, "/hit-detail/prov-e2e/1/0/0").await;
    assert!(
        !detail0.contains("methods"),
        "the hit outside the div must show no section"
    );
    let detail1 = get(&store, "/hit-detail/prov-e2e/1/0/1").await;
    assert!(detail1.contains("provenance-panel"));
    assert!(detail1.contains("methods"));
    assert!(detail1.contains("01.md")); // source basename, not a path
    assert!(detail1.contains("year"));
    assert!(detail1.contains("2013"));
}
