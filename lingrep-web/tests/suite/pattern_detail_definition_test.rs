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
//! The definition band: what the selected pattern *is*, beside what it matched.
//!
//! The Definition tab groups the authored pattern, its resolved references,
//! and its searchable surface variations.

use askama::Template;
use lingrep_web::{
    pattern_detail::{PatternDetailProjection, RetainedHitText},
    patterns::{expand_compound_pattern, expand_web_pattern},
    templates::{PatternDefinitionView, PatternDetailFragment},
};

const CATALOG: &str = concat!(
    "fragment KOTO := [lemma in {こと, 事}].\n",
    "fragment RERU := [lemma=れる].\n",
    "pattern ことが知られている := @KOTO [text=が] [lemma=知る] @RERU.\n",
    "pattern 単独 := [upos=VERB].\n",
);

fn render(definition: Option<PatternDefinitionView>) -> String {
    let hits = vec![RetainedHitText {
        reference_ordinal: 0,
        left: "左".into(),
        key: "鍵".into(),
        right: "右".into(),
        section_labels: Box::from([]),
        in_chain: false,
    }];
    let sections = lingrep_web::pattern_detail::sample_sections(&hits);
    let projection = PatternDetailProjection::new(
        hits,
        &sections,
        lingrep_web::snapshot::PatternKey::new(0).unwrap(),
    );
    PatternDetailFragment {
        definition,
        chains: None,
        chained: None,
        documents: None,
        pattern_name: "ことが知られている",
        exact_total_label: "1".into(),
        exact_total: 1,
        retained_count: 1,
        projection: &projection,
        workbench_id: "wb",
        revision: 1,
        pattern_key: 0,
    }
    .render()
    .unwrap()
}

fn view(name: &str) -> PatternDefinitionView {
    let expansion = expand_web_pattern(CATALOG, name)
        .expect("the fixture catalog expands")
        .expect("the definition is present");
    PatternDefinitionView {
        declaration: expansion.declaration,
        expanded: expansion.expanded,
        references: expansion.references.into_vec(),
        diagram: expansion
            .diagram
            .map(lingrep_web::templates::TrustedFragmentHtml::from_rendered),
    }
}

#[test]
fn the_band_shows_the_definition_as_written_and_fully_expanded() {
    let html = render(Some(view("ことが知られている")));

    assert!(html.contains("data-definition-band"), "{html}");
    assert!(
        html.contains("pattern ことが知られている := @KOTO [text=が] [lemma=知る] @RERU."),
        "the authored definition must appear verbatim: {html}"
    );
    assert!(
        html.contains(
            "pattern ことが知られている := [lemma in {こと, 事}] [text=が] [lemma=知る] [lemma=れる]."
        ),
        "the expansion must appear with every reference resolved: {html}"
    );
    // The names resolved, so a reader can go back to the catalog for any one.
    assert!(html.contains("@KOTO"), "{html}");
    assert!(html.contains("@RERU"), "{html}");
    assert!(
        html.contains("data-copy-definition=\"declaration\""),
        "{html}"
    );
    assert!(html.contains("data-copy-definition=\"expanded\""), "{html}");
    assert!(html.contains("data-jump-definition=\"KOTO\""), "{html}");
    assert!(
        html.contains("data-definition-diagram") && html.contains("pattern-fsm-svg"),
        "the expanded definition renders its state-machine SVG diagram: {html}"
    );
}

/// A pattern standing on no fragment says so, rather than showing the same
/// text twice under two headings.
#[test]
fn a_definition_with_nothing_to_resolve_says_so_instead_of_repeating_itself() {
    let html = render(Some(view("単独")));

    assert!(html.contains("data-definition-declaration"), "{html}");
    assert!(
        !html.contains("data-definition-expanded"),
        "there is nothing to expand: {html}"
    );
    assert!(html.contains("definition-standalone"), "{html}");
}

/// Surface variations remain available when definition expansion is absent.
#[test]
fn an_absent_definition_keeps_surface_variations_and_the_rest_of_the_pane() {
    let html = render(None);

    assert!(html.contains("data-definition-band"), "{html}");
    assert!(html.contains("data-ranked-list=\"surface\""), "{html}");
    assert!(!html.contains("data-definition-declaration"), "{html}");
    assert!(html.contains("kwic-concordance"), "{html}");
    assert!(html.contains("data-detail-view=\"sections\""), "{html}");
}

#[test]
fn compound_pattern_concatenates_declarations_expansions_and_diagram() {
    let expansion = expand_compound_pattern(
        CATALOG,
        "複合項目〔ことが知られている ＋ 単独〕",
        &["ことが知られている", "単独"],
    )
    .expect("the fixture catalog expands")
    .expect("both components are definitions");
    let html = render(Some(PatternDefinitionView {
        declaration: expansion.declaration,
        expanded: expansion.expanded,
        references: expansion.references.into_vec(),
        diagram: expansion
            .diagram
            .map(lingrep_web::templates::TrustedFragmentHtml::from_rendered),
    }));

    assert!(html.contains("data-definition-band"), "{html}");
    assert!(
        html.contains("pattern ことが知られている := @KOTO [text=が] [lemma=知る] @RERU.\npattern 単独 := [upos=VERB]."),
        "compound definition concatenates authored declarations: {html}"
    );
    assert!(
        html.contains("pattern ことが知られている := [lemma in {こと, 事}] [text=が] [lemma=知る] [lemma=れる].\npattern 単独 := [upos=VERB]."),
        "compound definition concatenates expanded definitions: {html}"
    );
    assert!(html.contains("@KOTO"), "{html}");
    assert!(html.contains("@RERU"), "{html}");
    assert!(
        html.contains("data-definition-diagram") && html.contains("pattern-fsm-svg"),
        "compound definition renders a chained state-machine SVG diagram: {html}"
    );
}

/// The expansion is offered as lingrep patterns — the band renders it in a `pre` so it can
/// be selected and pasted back into the editor — so it has to still be lingrep patterns
/// after templating.
#[test]
fn the_rendered_expansion_is_not_html_escaped_out_of_being_pattern() {
    let expansion = expand_web_pattern(CATALOG, "ことが知られている")
        .expect("the fixture catalog expands")
        .expect("the definition is present")
        .expanded
        .expect("the definition stands on two fragments");

    // Askama escapes `&`, `<` and `>`; lingrep patterns uses all three (`&` for attribute
    // conjunction and expression intersection, `<`/`>` in a capture header).
    // This fixture carries none, which is what keeps the assertion above
    // literal — a fixture that did would pin the escaping, not the expansion.
    assert!(
        !expansion.contains(['&', '<', '>']),
        "fixture drifted into escaped territory: {expansion}"
    );
}
