use askama::Template;
use lingrep_web::{
    pattern_detail::{PatternDetailProjection, RetainedHitText},
    templates::PatternDetailFragment,
};

#[test]
fn large_concordance_renders_every_row_with_lazy_inspection() {
    let context = "長い科学文の文脈".repeat(40);
    let hits = (0..1_000)
        .map(|reference_ordinal| RetainedHitText {
            reference_ordinal,
            left: context.clone(),
            key: format!("表層{reference_ordinal:04}"),
            right: context.clone(),
            section_labels: Box::new([]),
            in_chain: false,
        })
        .collect::<Vec<_>>();
    let sections = lingrep_web::pattern_detail::sample_sections(&hits);
    let projection = PatternDetailProjection::new(
        hits,
        &sections,
        lingrep_web::snapshot::PatternKey::new(0).unwrap(),
    );
    let html = PatternDetailFragment {
        definition: None,
        chains: None,
        chained: None,
        documents: None,
        pattern_name: "characterization",
        exact_total: 1_000,
        exact_total_label: "1,000".into(),
        retained_count: 1_000,
        projection: &projection,
        workbench_id: "measurement",
        revision: 1,
        pattern_key: 0,
    }
    .render()
    .unwrap();
    assert_eq!(html.matches("class=\"kwic-row\"").count(), 1_000);
    assert_eq!(html.matches("class=\"kwic-sentence\"").count(), 1_000);
    assert_eq!(html.matches("hx-trigger=\"hit-select\"").count(), 1_000);
    assert!(!html.contains("kwic-scroll"));
    assert!(!html.contains("hit-detail-scroll"));
}
