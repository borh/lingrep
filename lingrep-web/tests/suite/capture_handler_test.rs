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
use askama::Template;
use axum::{Router, body::Body, http::Request};
use http_body_util::BodyExt;
use lingrep_core::token_match::TokenInfo;
use lingrep_web::inspection::{CaptureInspection, CaptureOrdinal};
use lingrep_web::revision::WorkbenchId;
use lingrep_web::state::{Config, DetailLimits, RequestStore};
use lingrep_web::templates::{
    CaptureInspectionFragment, GraphBindingAttributeData, GraphBindingData, HitDetailFragment,
    KwicCaptureData, KwicCaptureLaneData, TrustedFragmentHtml,
};
use proptest::prelude::*;
use tower::ServiceExt;

use crate::render_security_support::{
    HTML_SECURITY_DELIMITERS, expected_html_text, safe_affix, security_payload,
};
use crate::v6_support::runtime_fixture;

async fn kwic_html(source: &str) -> String {
    let (_fixture, runtime) = runtime_fixture(
        "capture-handler",
        vec![vec![
            TokenInfo::default()
                .with_orth("食べる")
                .with_lemma("食べる"),
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
        runtime,
    );
    let workbench = WorkbenchId::new("capture-handler");
    let slot = store
        .corpora
        .resolve(None)
        .expect("a validated configuration declares a default corpus");
    store
        .coordinators
        .accept_next_revision(workbench.clone(), slot, source.into())
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
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
    let response = Router::new()
        .merge(lingrep_web::handlers::pattern_detail::routes())
        .with_state(store)
        .oneshot(
            Request::builder()
                .uri("/hit-detail/capture-handler/1/0/0")
                .body(Body::empty())
                .unwrap(),
        )
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

#[tokio::test]
async fn kwic_renders_capture_name_path_range_and_text_equivalent() {
    let html = kwic_html("outer := (?<unit> [text=食べる]).").await;
    // Hit details render inside the dock, with no disclosure wrapper or
    // per-hit close control — the dock owns close semantics.
    assert!(html.contains("class=\"hit-detail\""));
    assert!(!html.contains("class=\"hit-inspector-disclosure\""));
    assert!(!html.contains("data-clear-hit"));
    assert!(!html.contains("kwic-inspector-row"));
    assert!(!html.contains(">Close</button>"));
    assert!(html.contains("class=\"hit-capture-list\""));
    assert!(html.contains("capture-selector hit-capture-item"));
    assert!(html.contains("data-capture-target="));
    assert!(html.contains("aria-controls="));
    assert!(html.contains("unit"));
    assert!(html.contains("outer/unit"));
    assert!(html.contains("tokens 0..1"));
    assert!(html.contains("食べる"));
    assert!(html.contains("<button"));
    assert!(!html.contains("class=\"hit-detail-scroll\""));
    assert!(!html.contains("class=\"hit-capture-lane\""));
}

#[derive(Clone, Copy, Debug)]
enum CaptureSite {
    Name,
    Path,
    Text,
}

const CAPTURE_SITES: [CaptureSite; 3] = [CaptureSite::Name, CaptureSite::Path, CaptureSite::Text];

fn capture_with(site: CaptureSite, payload: &str) -> CaptureInspection {
    let mut capture = CaptureInspection {
        ordinal: CaptureOrdinal::new(0),
        name: "capture-name".into(),
        path: "pattern/capture-name".into(),
        span: 0..1,
        text: "captured-text".into(),
    };
    match site {
        CaptureSite::Name => capture.name = payload.into(),
        CaptureSite::Path => capture.path = payload.into(),
        CaptureSite::Text => capture.text = payload.into(),
    }
    capture
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn every_capture_field_escapes_every_html_delimiter(
        prefix in safe_affix(),
        suffix in safe_affix(),
    ) {
        for site in CAPTURE_SITES {
            for delimiter in HTML_SECURITY_DELIMITERS {
                let payload = security_payload(&prefix, delimiter, &suffix);
                let expected = expected_html_text(&payload);
                let capture = capture_with(site, &payload);
                let html = CaptureInspectionFragment { capture: &capture }
                    .render()
                    .unwrap();

                prop_assert!(
                    !html.contains(&payload),
                    "raw payload survived at {site:?} for {delimiter:?}",
                );
                prop_assert_eq!(
                    html.matches(&expected).count(),
                    1,
                    "escaped payload count at {:?} for {:?}",
                    site,
                    delimiter,
                );
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum OuterCaptureSite {
    Name,
    Text,
}

const OUTER_CAPTURE_SITES: [OuterCaptureSite; 2] = [OuterCaptureSite::Name, OuterCaptureSite::Text];

fn outer_capture_fragment(site: OuterCaptureSite, payload: &str) -> String {
    let mut capture = KwicCaptureData {
        ordinal: 0,
        name: "capture-name".into(),
        text: "captured-text".into(),
        start: 0,
        end: 1,
        grid_start: 1,
        grid_span: 1,
        detail_html: TrustedFragmentHtml::from_rendered(String::new()),
    };
    match site {
        OuterCaptureSite::Name => capture.name = payload.into(),
        OuterCaptureSite::Text => capture.text = payload.into(),
    }
    HitDetailFragment {
        reference_ordinal: 0,
        workbench_id: "wb",
        revision: 1,
        pattern_key: 0,
        id_prefix: "hit-capture-0-0".into(),
        tokens: Vec::new(),
        capture_lanes: vec![KwicCaptureLaneData {
            captures: vec![capture],
        }],
        captures: Vec::new(),
        graph_bindings: Vec::new(),
        provenance: None,
    }
    .render()
    .unwrap()
}

#[test]
fn hit_detail_renders_typed_graph_bindings() {
    let html = HitDetailFragment {
        reference_ordinal: 0,
        workbench_id: "wb",
        revision: 1,
        pattern_key: 0,
        id_prefix: "hit-capture-0-0".into(),
        tokens: Vec::new(),
        capture_lanes: Vec::new(),
        captures: Vec::new(),
        graph_bindings: vec![
            GraphBindingData {
                name: "predicate".into(),
                is_node: true,
                identity: "14.1".into(),
                relation: None,
                attributes: vec![GraphBindingAttributeData {
                    name: "lemma".into(),
                    value: "have".into(),
                }],
            },
            GraphBindingData {
                name: "role".into(),
                is_node: false,
                identity: "14.1 → 8".into(),
                relation: Some("obl".into()),
                attributes: Vec::new(),
            },
        ],
        provenance: None,
    }
    .render()
    .unwrap();

    assert!(html.contains("class=\"hit-graph-bindings\""));
    assert!(html.contains("predicate"));
    assert!(html.contains("14.1"));
    assert!(html.contains("lemma"));
    assert!(html.contains("have"));
    assert!(html.contains("role"));
    assert!(html.contains("obl"));
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn outer_capture_name_and_text_escape_every_html_delimiter(
        prefix in safe_affix(),
        suffix in safe_affix(),
    ) {
        for site in OUTER_CAPTURE_SITES {
            for delimiter in HTML_SECURITY_DELIMITERS {
                let payload = security_payload(&prefix, delimiter, &suffix);
                let expected = expected_html_text(&payload);
                let html = outer_capture_fragment(site, &payload);

                prop_assert!(
                    !html.contains(&payload),
                    "raw outer capture payload survived at {site:?} for {delimiter:?}",
                );
                prop_assert_eq!(
                    html.matches(&expected).count(),
                    1,
                    "outer escaped payload count at {:?} for {:?}",
                    site,
                    delimiter,
                );
                let expected_site = match site {
                    OuterCaptureSite::Name => {
                        format!("<span>{expected}</span><span lang=\"ja\">")
                    }
                    OuterCaptureSite::Text => {
                        format!("<span lang=\"ja\">{expected}</span><span>tokens")
                    }
                };
                prop_assert!(
                    html.contains(&expected_site),
                    "outer escaped payload missing from {site:?}",
                );
            }
        }
    }
}

#[tokio::test]
async fn capture_free_kwic_omits_capture_controls_and_empty_lanes() {
    let html = kwic_html("plain := [text=食べる].").await;
    assert!(!html.contains("class=\"hit-capture-list\""));
    assert!(!html.contains("class=\"capture-inspection\""));
}

#[tokio::test]
async fn capture_nesting_increases_rendered_kwic_payload() {
    let capture_free = kwic_html("plain := [text=食べる].").await;
    let one_capture = kwic_html("outer := (?<unit> [text=食べる]).").await;
    let nested = kwic_html("outer := (?<whole> (?<unit> [text=食べる])).").await;
    assert!(capture_free.len() < one_capture.len());
    assert!(one_capture.len() < nested.len());
}
