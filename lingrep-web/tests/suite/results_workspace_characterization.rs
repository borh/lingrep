use askama::Template;
use lingrep_web::{
    pattern_detail::{PatternDetailProjection, RetainedHitText},
    templates::PatternDetailFragment,
};

#[test]
fn measures_one_thousand_long_sentence_rows() {
    let context = "長い科学文の文脈".repeat(40);
    let hits = (0..1_000)
        .map(|reference_ordinal| RetainedHitText {
            reference_ordinal,
            left: context.clone(),
            key: format!("表層{reference_ordinal:04}"),
            right: context.clone(),
            section_labels: Box::new([]),
        })
        .collect::<Vec<_>>();
    // Built before the clock starts: this characterizes projection and render
    // cost, and the tally is the scan's work, not the panel's.
    let sections = lingrep_web::pattern_detail::sample_sections(&hits);
    let started = std::time::Instant::now();
    let projection = PatternDetailProjection::new(
        hits,
        &sections,
        lingrep_web::snapshot::PatternKey::new(0).unwrap(),
    );
    let html = PatternDetailFragment {
        definition: None,
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
    let elapsed = started.elapsed().as_millis();
    let node_count = html.matches('<').count();
    eprintln!(
        "rows=1000 pattern_detail_bytes={} server_projection_render_ms={elapsed} node_count={node_count}",
        html.len()
    );
    assert_eq!(html.matches("class=\"kwic-row\"").count(), 1_000);
    assert_eq!(html.matches("class=\"kwic-sentence\"").count(), 1_000);
    assert_eq!(html.matches("hx-trigger=\"hit-select\"").count(), 1_000);
    assert!(!html.contains("kwic-scroll"));
    assert!(!html.contains("hit-detail-scroll"));
}
