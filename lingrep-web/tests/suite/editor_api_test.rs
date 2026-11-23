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
use http_body_util::BodyExt;
use lingrep_web::{RequestStore, handlers::pattern_editor};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn post(path: &str, value: Value) -> (StatusCode, Value) {
    let app = pattern_editor::routes().with_state(RequestStore::default());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(value.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn check_is_source_to_result_and_sanitizes_diagnostics() {
    let secret = "EDITOR_SECRET_92C1";
    let (status, body) = post(
        "/check",
        json!({"source": format!("秘密 := [text~/({secret}/].")}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let encoded = body.to_string();
    assert!(!encoded.contains(secret));
    assert!(!encoded.contains("edit_revision"));
    assert_eq!(body["status"], "invalid");
}

#[tokio::test]
async fn check_returns_safe_unicode_byte_ranges_for_every_diagnostic() {
    let source = "猫 := [text=猫] @missing.";
    let (status, body) = post("/check", json!({"source": source})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let diagnostic = &body["diagnostics"][0];
    assert_eq!(diagnostic["code"], "UndefinedReference");
    assert_eq!(diagnostic["location"]["source_name"], "lingrep-web.lx");
    let start = diagnostic["location"]["byte_range"]["start"]
        .as_u64()
        .unwrap() as usize;
    let end = diagnostic["location"]["byte_range"]["end"]
        .as_u64()
        .unwrap() as usize;
    assert_eq!(&source.as_bytes()[start..end], b"@missing");
    assert_eq!(diagnostic["message"], "Pattern references are invalid.");
    assert_eq!(diagnostic["message_key"], "editor.diag_reference");
}

#[tokio::test]
async fn a_valid_pattern_reports_its_compile_stats() {
    let (status, body) = post("/check", json!({"source": "p := [upos=VERB]."})).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "valid");
    assert_eq!(body["stats"]["definitions"], 1);
    assert_eq!(body["stats"]["references"], 0);
    assert_eq!(body["diagnostics"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn an_unreferenced_fragment_is_a_warning_on_an_otherwise_valid_pattern() {
    let source = "fragment orphan := [upos=NOUN].\np := [upos=VERB].";
    let (status, body) = post("/check", json!({"source": source})).await;

    // The program is well formed: a warning must not turn into a rejection.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "valid");
    assert_eq!(body["stats"]["definitions"], 2);

    let diagnostics = body["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["code"], "UnusedFragment");
    assert_eq!(diagnostics[0]["severity"], "warning");
    assert_eq!(diagnostics[0]["category"], "lint");
    // The message is canned, not the compiler's: lingrep-web never echoes submitted
    // pattern text, and the compiler's message embeds the definition name.
    assert_eq!(diagnostics[0]["message"], "A fragment is never referenced.");
    assert_eq!(diagnostics[0]["location"]["line"], 1);
    assert_eq!(diagnostics[0]["location"]["byte_range"]["start"], 0);
    assert_eq!(diagnostics[0]["location"]["byte_range"]["end"], 31);
}

#[tokio::test]
async fn a_rejected_pattern_still_marks_its_diagnostics_as_errors() {
    let (status, body) = post("/check", json!({"source": "x := @missing."})).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["status"], "invalid");
    assert_eq!(body["diagnostics"][0]["severity"], "error");
}

#[tokio::test]
async fn format_returns_canonical_lf_and_located_errors() {
    let (status, body) = post("/format", json!({"source":"x:=[upos=VERB].\r\n"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "formatted");
    assert_eq!(body["source"], "x := [upos=VERB].\n");

    let (status, body) = post("/format", json!({"source":"bad := [upos=]."})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["status"], "invalid");
    assert!(body["diagnostics"][0]["location"]["line"].is_number());
}

#[tokio::test]
async fn capture_key_edits_preserve_unicode_source_and_exclude_unsupported_keys() {
    let source =
        "description: \"猫😀\"\r\nkey: [$word.lemma]\r\npattern p := (?<word> [text=猫]).\r\n";
    let (status, body) = post("/check", json!({"source": source})).await;
    assert_eq!(status, StatusCode::OK);
    let edits = body["capture_key_edits"]
        .as_array()
        .expect("capture key edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0]["capture"], "word");
    let start = edits[0]["byte_range"]["start"].as_u64().unwrap() as usize;
    let end = edits[0]["byte_range"]["end"].as_u64().unwrap() as usize;
    assert_eq!(&source[start..end], "lemma");
    for source in [
        "defaults\nkey: [$word.lemma]\nend\np := (?<word> []).",
        "key: [$word.span.lemma]\np := (?<word> []+).",
        "key: [$word.node.id]\np := (?<n> []) where (?<word> []) -[obj]-> $n.",
    ] {
        let (status, body) = post("/check", json!({"source": source})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["capture_key_edits"], json!([]));
    }
}

#[tokio::test]
async fn capture_key_edits_cover_each_field_of_a_compound_graph_key() {
    let source = "# 名詞😀\nset ROLE := {nsubj, obj, obl}.\n\
key: [$n.lemma, $p.lemma, $v.lemma]\n\
npv := (?<n> [dep.rel in @ROLE]) (?<p> [dep.rel=case])\n\
       where (?<v> [upos=VERB]) -[@ROLE]-> $n, $n -[case]-> $p.\n";
    let (status, body) = post("/check", json!({"source": source})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let at = |needle: &str| source.find(needle).unwrap() + 3;
    let edit = |field: usize, capture: &str| {
        json!({
            "definition": "npv", "published_name": "npv", "field": field,
            "capture": capture, "attribute": "lemma",
            "attributes": ["text", "lemma", "norm"], "source_name": "lingrep-web.lx",
            "byte_range": {"start": at(&format!("${capture}.lemma")), "end": at(&format!("${capture}.lemma")) + 5},
        })
    };
    assert_eq!(
        body["capture_key_edits"],
        json!([edit(0, "n"), edit(1, "p"), edit(2, "v")])
    );
}

fn morphology_store() -> (crate::v6_support::ArtifactFixture, RequestStore) {
    let (fixture, runtime) = crate::v6_support::runtime_fixture(
        "editor-check",
        vec![vec![
            lingrep_core::token_match::TokenInfo::default()
                .with_orth("猫")
                .with_pos("NOUN"),
        ]],
    );
    let store = RequestStore::with_runtime(
        lingrep_web::state::Config {
            dict_path: None,
            corpora: Vec::new(),
            input_format: None,
            markdown: lingrep_core::corpus::MarkdownReaderConfig::new(
                lingrep_core::corpus::SectionPolicy::default(),
            ),
            max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
            detail_limits: lingrep_web::state::DetailLimits { max_kwic: 10 },
        },
        runtime,
    );
    (fixture, store)
}

async fn post_to(store: RequestStore, value: Value) -> (StatusCode, Value) {
    let response = pattern_editor::routes()
        .with_state(store)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/check")
                .header("content-type", "application/json")
                .body(Body::from(value.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn check_with_a_corpus_warns_of_attributes_its_schema_lacks_and_without_one_does_not() {
    let (_fixture, store) = morphology_store();
    let source = "noun := [unidic.pos1=名詞].\nsubject := [dep.rel=nsubj].\n";
    let corpus = lingrep_web::corpus_set::CorpusName::default_key();
    let (status, body) = post_to(
        store.clone(),
        json!({"source": source, "corpus": corpus.as_str()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "valid");
    let warnings: Vec<&Value> = body["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|diagnostic| diagnostic["code"] == "attribute_unavailable")
        .collect();
    assert_eq!(warnings.len(), 1, "{body}");
    let warning = warnings[0];
    assert_eq!(warning["severity"], "warning");
    assert_eq!(warning["message_key"], "editor.diag_attribute_unavailable");
    assert_eq!(warning["args"]["attribute"], "dep.rel");
    assert_eq!(warning["args"]["definitions"], "subject");
    assert_eq!(warning["args"]["schema"], "vibrato morphology");
    let start = warning["location"]["byte_range"]["start"].as_u64().unwrap() as usize;
    let end = warning["location"]["byte_range"]["end"].as_u64().unwrap() as usize;
    assert_eq!(&source[start..end], "dep.rel");
    assert_eq!(warning["location"]["line"], 2);
    assert!(
        body["token_keys"]
            .as_array()
            .unwrap()
            .iter()
            .any(|key| key == "unidic.pos1")
    );

    let (status, body) = post_to(store.clone(), json!({"source": source})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == "attribute_unavailable"),
        "{body}"
    );
    assert!(body.get("token_keys").is_none());

    let (status, body) = post_to(store, json!({"source": source, "corpus": "undeclared"})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("token_keys").is_none(), "{body}");
}

#[tokio::test]
async fn check_with_a_corpus_refuses_a_dependency_where_its_schema_cannot_answer() {
    let (_fixture, store) = morphology_store();
    let corpus = lingrep_web::corpus_set::CorpusName::default_key();
    let source = "pattern edge := (?<d> []) where (?<h> []) -[*]-> $d.\n";
    let (status, body) = post_to(store.clone(), json!({"source": source})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = post_to(store, json!({"source": source, "corpus": corpus.as_str()})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["diagnostics"][0]["code"], "SchemaBind");
    assert_eq!(body["diagnostics"][0]["severity"], "error");
    assert_eq!(
        body["diagnostics"][0]["args"]["schema"],
        "vibrato morphology"
    );
}

#[tokio::test]
async fn a_corpus_sentence_is_shown_with_its_stored_tokens_until_the_form_omits_it() {
    let (_fixture, store) = morphology_store();
    let page = lingrep_web::handlers::sample::page_routes()
        .with_state(store.clone())
        .oneshot(
            Request::builder()
                .uri("/sample?sentence=0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let page = String::from_utf8(
        page.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(page.contains(r#"name="corpus_sentence" value="0""#));
    assert!(page.contains(r#"placeholder="猫が走る。">猫</textarea>"#));

    let post = |fields: Vec<(&'static str, &'static str)>| {
        let store = store.clone();
        async move {
            let response = lingrep_web::handlers::sample::routes()
                .with_state(store)
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/sample")
                        .header("content-type", "application/x-www-form-urlencoded")
                        .body(Body::from(serde_urlencoded::to_string(fields).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let body = response.into_body().collect().await.unwrap().to_bytes();
            (status, String::from_utf8(body.to_vec()).unwrap())
        }
    };
    let pattern = "noun := [upos=NOUN].";

    let (status, html) = post(vec![
        ("pattern", pattern),
        ("sample", "ignored"),
        ("corpus_sentence", "0"),
    ])
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("data-sample-stored"), "{html}");
    assert!(html.contains("data-sample-bar=\"0\""), "{html}");

    let (status, _) = post(vec![
        ("pattern", pattern),
        ("sample", "ignored"),
        ("corpus_sentence", "7"),
    ])
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
