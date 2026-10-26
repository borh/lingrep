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
use lingrep_web::provenance_view::{HitProvenance, ProvenanceDivision, ProvenanceField};
use lingrep_web::templates::HitDetailFragment;
use proptest::prelude::*;

use crate::render_security_support::{
    HTML_SECURITY_DELIMITERS, expected_html_text, safe_affix, security_payload,
};

fn fragment(provenance: Option<HitProvenance>) -> String {
    HitDetailFragment {
        overlaps: Vec::new(),
        reference_ordinal: 0,
        workbench_id: "wb",
        revision: 1,
        pattern_key: 0,
        id_prefix: "hit-capture-0-0".into(),
        tokens: Vec::new(),
        capture_lanes: Vec::new(),
        captures: Vec::new(),
        graph_bindings: Vec::new(),
        provenance,
    }
    .render()
    .unwrap()
}

#[test]
fn panel_shows_source_sections_and_metadata() {
    let html = fragment(Some(HitProvenance {
        source: "00042.md".into(),
        document: Box::new([ProvenanceField {
            key: "year".into(),
            values: Box::new(["2013".to_string()]),
        }]),
        divisions: Box::new([]),
        sentence: Box::new([]),
        sections: Box::new(["methods".to_string()]),
    }));
    assert!(html.contains("provenance-panel"));
    assert!(html.contains("00042.md"));
    assert!(html.contains("methods"));
    assert!(html.contains("year"));
    assert!(html.contains("2013"));
}

#[test]
fn metadata_values_carry_no_hardcoded_language() {
    // Metadata is open-vocabulary (e.g. a numeric year, a Latin-script name), so
    // the panel must not assert `lang="ja"` on values it cannot classify; the
    // value `<dd>` inherits the document language instead.
    let html = fragment(Some(HitProvenance {
        source: "x.md".into(),
        document: Box::new([ProvenanceField {
            key: "year".into(),
            values: Box::new(["2013".to_string()]),
        }]),
        divisions: Box::new([]),
        sentence: Box::new([]),
        sections: Box::new([]),
    }));
    assert!(html.contains("<li>2013</li>"));
    assert!(!html.contains("lang=\"ja\""));
}

#[derive(Clone, Copy, Debug)]
enum ProvenanceSite {
    Source,
    Section,
    DocumentKey,
    DocumentValue,
    DivisionKind,
    DivisionSourceId,
    DivisionKey,
    DivisionValue,
    SentenceKey,
    SentenceValue,
}

const PROVENANCE_SITES: [ProvenanceSite; 10] = [
    ProvenanceSite::Source,
    ProvenanceSite::Section,
    ProvenanceSite::DocumentKey,
    ProvenanceSite::DocumentValue,
    ProvenanceSite::DivisionKind,
    ProvenanceSite::DivisionSourceId,
    ProvenanceSite::DivisionKey,
    ProvenanceSite::DivisionValue,
    ProvenanceSite::SentenceKey,
    ProvenanceSite::SentenceValue,
];

fn provenance_with(site: ProvenanceSite, payload: &str) -> HitProvenance {
    let mut provenance = HitProvenance {
        source: "source.md".into(),
        document: Box::new([]),
        divisions: Box::new([]),
        sentence: Box::new([]),
        sections: Box::new([]),
    };
    match site {
        ProvenanceSite::Source => provenance.source = payload.into(),
        ProvenanceSite::Section => provenance.sections = Box::new([payload.into()]),
        ProvenanceSite::DocumentKey => {
            provenance.document = Box::new([ProvenanceField {
                key: payload.into(),
                values: Box::new(["document-value".into()]),
            }]);
        }
        ProvenanceSite::DocumentValue => {
            provenance.document = Box::new([ProvenanceField {
                key: "document-key".into(),
                values: Box::new([payload.into()]),
            }]);
        }
        ProvenanceSite::DivisionKind | ProvenanceSite::DivisionSourceId => {
            provenance.divisions = Box::new([ProvenanceDivision {
                source_id: if matches!(site, ProvenanceSite::DivisionSourceId) {
                    Some(payload.to_owned())
                } else {
                    None
                },
                id: 1,
                kind: if matches!(site, ProvenanceSite::DivisionKind) {
                    payload.to_owned()
                } else {
                    "paragraph".into()
                },
                parent_id: Some(0),
                fields: Box::new([]),
            }]);
        }
        ProvenanceSite::DivisionKey => {
            provenance.divisions = Box::new([ProvenanceDivision {
                source_id: None,
                id: 0,
                kind: "paragraph".into(),
                parent_id: None,
                fields: Box::new([ProvenanceField {
                    key: payload.into(),
                    values: Box::new(["paragraph-value".into()]),
                }]),
            }]);
        }
        ProvenanceSite::DivisionValue => {
            provenance.divisions = Box::new([ProvenanceDivision {
                source_id: None,
                id: 0,
                kind: "paragraph".into(),
                parent_id: None,
                fields: Box::new([ProvenanceField {
                    key: "paragraph-key".into(),
                    values: Box::new([payload.into()]),
                }]),
            }]);
        }
        ProvenanceSite::SentenceKey => {
            provenance.sentence = Box::new([ProvenanceField {
                key: payload.into(),
                values: Box::new(["sentence-value".into()]),
            }]);
        }
        ProvenanceSite::SentenceValue => {
            provenance.sentence = Box::new([ProvenanceField {
                key: "sentence-key".into(),
                values: Box::new([payload.into()]),
            }]);
        }
    }
    provenance
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn every_provenance_field_escapes_every_html_delimiter(
        prefix in safe_affix(),
        suffix in safe_affix(),
    ) {
        for site in PROVENANCE_SITES {
            for delimiter in HTML_SECURITY_DELIMITERS {
                let payload = security_payload(&prefix, delimiter, &suffix);
                let expected = expected_html_text(&payload);
                let html = fragment(Some(provenance_with(site, &payload)));

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

#[test]
fn no_panel_when_provenance_absent() {
    let html = fragment(None);
    assert!(!html.contains("provenance-panel"));
}
