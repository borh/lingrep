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
use lingrep_web::{
    pattern_detail::{PatternDetailProjection, RetainedHitText},
    templates::PatternDetailFragment,
};

fn render(hits: Vec<RetainedHitText>) -> String {
    let retained_count = hits.len();
    let sections = lingrep_web::pattern_detail::sample_sections(&hits);
    let projection = PatternDetailProjection::new(
        hits,
        &sections,
        lingrep_web::snapshot::PatternKey::new(0).unwrap(),
    );
    PatternDetailFragment {
        definition: None,
        pattern_name: "chips",
        exact_total_label: "1".into(),
        exact_total: 1,
        retained_count,
        projection: &projection,
        workbench_id: "wb",
        revision: 1,
        pattern_key: 0,
    }
    .render()
    .unwrap()
}

fn hit(reference_ordinal: usize, section_labels: Box<[String]>) -> RetainedHitText {
    RetainedHitText {
        reference_ordinal,
        left: "私は".into(),
        key: "食べる".into(),
        right: "。".into(),
        section_labels,
    }
}

#[test]
fn rows_without_section_labels_render_no_section_labels() {
    // Degradation path: when provenance is absent, section_labels is empty and
    // the row's section cell stays empty — no label markup, no title tooltip.
    let html = render(vec![hit(0, Box::new([])), hit(1, Box::new([]))]);
    assert_eq!(html.matches("class=\"kwic-row\"").count(), 2);
    assert!(!html.contains("kwic-section-label"));
    assert!(!html.contains("<td class=\"kwic-section\""));
}
