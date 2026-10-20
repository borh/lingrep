use crate::support::tok;
use askama::Template;
use lingrep_core::token_match::TokenInfo;
use lingrep_web::sample::{SampleAnalysis, analyze_tokens};
use lingrep_web::templates::SampleResults;

#[test]
fn matched_renders_inline_tokens_and_captures_without_fetch_urls() {
    let lines = vec![vec![tok("猫", "名詞"), tok("走る", "動詞")]];
    let analysis = analyze_tokens("pattern v := (?<verb> [upos=動詞]).", &lines);
    assert!(matches!(analysis, SampleAnalysis::Matched { .. }));
    let html = SampleResults { analysis }.render().expect("renders");
    assert!(html.contains("走る"), "match surface shown");
    assert!(html.contains("verb"), "named capture shown");
    // The capture-selector buttons target `sample-capture-{render_index}-{ordinal}`
    // detail divs; those targets must actually be rendered or the click handler's
    // getElementById lookup silently no-ops.
    assert!(
        html.contains("id=\"sample-capture-"),
        "capture detail target div is rendered"
    );
    // Inline contract: NO per-token inspect fetch URLs in the sample fragment.
    assert!(
        !html.contains("/api/inspect/"),
        "sample detail is inline, never fetched"
    );
    assert!(
        html.contains("data-sample-token=\"0\""),
        "sample tokens are interactive buttons: {html}"
    );
    assert!(
        html.contains("id=\"sample-token-0\""),
        "sample token inspection panel is rendered inline: {html}"
    );
    assert!(
        html.contains("title=\"動詞"),
        "sample token buttons carry a morphological tooltip: {html}"
    );
}

#[test]
fn fragments_only_shows_its_distinct_message() {
    let lines = vec![vec![tok("走る", "動詞")]];
    let html = SampleResults {
        analysis: analyze_tokens("fragment F := [upos=動詞].", &lines),
    }
    .render()
    .unwrap();
    assert!(
        html.to_lowercase().contains("fragment"),
        "explains fragment-only documents"
    );
    assert!(
        !html.contains("No matches"),
        "not the bare no-match message"
    );
}

#[test]
fn failed_shows_the_compile_error_message() {
    let lines = vec![vec![tok("猫", "名詞")]];
    let html = SampleResults {
        analysis: analyze_tokens("pattern broken := [upos=", &lines),
    }
    .render()
    .unwrap();
    assert!(
        html.contains("error") || html.contains("Error") || html.contains(":"),
        "shows a diagnostic"
    );
}

#[test]
fn sample_render_shows_constellation_states_classes_and_parent_depth() {
    let source = include_str!("../../../patterns/sf-experimental.lx");
    let analysis = analyze_tokens(
        source,
        &[vec![
            TokenInfo::default().with_orth("もの").with_lemma("物"),
            TokenInfo::default().with_orth("で").with_lemma("だ"),
            TokenInfo::default().with_orth("あろう").with_lemma("有る"),
        ]],
    );
    let html = SampleResults { analysis }.render().expect("renders");

    assert!(html.contains("sample-constellation"));
    assert!(html.contains("S2C"));
    assert!(html.contains("data-publication=\"published\""));
    assert!(html.contains("data-publication=\"suppressed\""));
    assert!(html.contains("data-depth=\"0\""));
    assert!(!html.contains("ものである"));
}

#[test]
fn sample_render_shows_composition_regions_and_a_literal_gap() {
    let lines = vec![vec![
        tok("猫", "名詞"),
        tok("が", "助詞"),
        tok("走る", "動詞"),
    ]];
    let analysis = analyze_tokens(
        "pattern noun := [upos=名詞]. pattern verb := [upos=動詞].",
        &lines,
    );

    let html = SampleResults { analysis }.render().expect("renders");

    assert!(html.contains("sample-composition"), "the strip is rendered");
    assert!(
        html.contains("data-gap-tokens=\"1\""),
        "the gap width is machine-readable: {html}"
    );
    assert!(
        html.contains("data-gap-budget=\"within\""),
        "a one-token gap is inside the initial expectation"
    );
    // The literal intervening token is visible, with its morphology.
    assert!(html.contains("が"), "the gap token surface is shown");
    assert!(html.contains("助詞"), "the gap token POS is shown");
}

#[test]
fn sample_render_labels_a_wide_gap_as_diagnostic() {
    let lines = vec![vec![
        tok("猫", "名詞"),
        tok("は", "助詞"),
        tok("とても", "副詞"),
        tok("速く", "形容詞"),
        tok("走る", "動詞"),
    ]];
    let analysis = analyze_tokens(
        "pattern noun := [upos=名詞]. pattern verb := [upos=動詞].",
        &lines,
    );

    let html = SampleResults { analysis }.render().expect("renders");

    assert!(html.contains("data-gap-tokens=\"3\""));
    assert!(
        html.contains("data-gap-budget=\"investigate\""),
        "a three-token gap is evidence of a missing expression"
    );
    for surface in ["は", "とても", "速く"] {
        assert!(html.contains(surface), "gap token {surface} is not elided");
    }
}

#[test]
fn sample_render_shows_composition_for_the_experimental_catalog() {
    let source = include_str!("../../../patterns/sf-experimental.lx");
    let analysis = analyze_tokens(
        source,
        &[vec![
            TokenInfo::default().with_orth("もの").with_lemma("物"),
            TokenInfo::default().with_orth("で").with_lemma("だ"),
            TokenInfo::default().with_orth("あろう").with_lemma("有る"),
        ]],
    );

    let html = SampleResults { analysis }.render().expect("renders");

    assert!(html.contains("sample-composition"));
    assert!(html.contains("S2C"), "the published class still shows");
    // The composition strip must not resurrect the pattern policy excluded.
    assert!(!html.contains("ものである"));
}
