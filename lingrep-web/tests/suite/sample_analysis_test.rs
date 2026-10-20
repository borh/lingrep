use crate::support::tok;
use lingrep_core::token_match::TokenInfo;
use lingrep_web::inspection::CompositionItem;
use lingrep_web::sample::{SampleAnalysis, analyze_tokens};

#[test]
fn matched_carries_document_global_spans_and_capture_values() {
    let lines = vec![
        vec![tok("猫", "名詞"), tok("が", "助詞")],
        vec![tok("は", "助詞"), tok("走る", "動詞")],
    ];
    // Named capture so we can assert complete capture values, not just the span.
    let result = analyze_tokens("pattern v := (?<verb> [upos=動詞]).", &lines);
    let SampleAnalysis::Matched {
        tokens,
        matches,
        constellations,
    } = result
    else {
        panic!("expected Matched");
    };
    assert_eq!(tokens.len(), 4, "tokens flattened document-wide");
    assert_eq!(tokens[3].index, 3, "document-global index, not per-line");
    assert_eq!(matches.len(), 1);
    let m = &matches[0];
    assert_eq!(
        (m.start, m.end),
        (3, 4),
        "span in document-global coordinates"
    );
    assert_eq!(m.pattern_name, "v");
    assert_eq!(m.captures.len(), 1, "the named capture is present");
    assert_eq!(m.captures[0].name, "verb");
    assert_eq!(
        (m.captures[0].start, m.captures[0].end),
        (3, 4),
        "capture span is document-global"
    );
    assert_eq!(
        constellations.len(),
        2,
        "one constellation per non-empty line"
    );
    assert_eq!(constellations[1].groups[0].span, 3..4);
}

#[test]
fn multiple_patterns_and_empty_lines_are_handled() {
    let lines = vec![
        vec![tok("猫", "名詞")],
        vec![], // empty line contributes no tokens and no offset drift
        vec![tok("走る", "動詞")],
    ];
    let result = analyze_tokens(
        "pattern n := [upos=名詞]. pattern v := [upos=動詞].",
        &lines,
    );
    let SampleAnalysis::Matched {
        tokens,
        matches,
        constellations,
    } = result
    else {
        panic!("expected Matched")
    };
    assert_eq!(tokens.len(), 2, "empty line adds nothing");
    let names: Vec<_> = matches.iter().map(|m| m.pattern_name.as_str()).collect();
    assert!(
        names.contains(&"n") && names.contains(&"v"),
        "both patterns match"
    );
    // The verb is at document-global index 1 (empty line did not shift it to 2).
    let verb = matches.iter().find(|m| m.pattern_name == "v").unwrap();
    assert_eq!((verb.start, verb.end), (1, 2));
    assert_eq!(
        constellations.len(),
        2,
        "empty lines create no constellation"
    );
}

#[test]
fn sf_experimental_aro_u_is_a_modal_leaf_not_a_mono_dearu_match() {
    let source = include_str!("../../../patterns/sf-experimental.lx");
    let lines = vec![vec![
        TokenInfo::default().with_orth("もの").with_lemma("物"),
        TokenInfo::default().with_orth("で").with_lemma("だ"),
        TokenInfo::default().with_orth("あろう").with_lemma("有る"),
    ]];

    let result = analyze_tokens(source, &lines);
    let SampleAnalysis::Matched { constellations, .. } = result else {
        panic!("expected a visible modal candidate")
    };
    let aro_u = constellations[0]
        .groups
        .iter()
        .flat_map(|group| group.alternatives.iter())
        .find(|candidate| candidate.raw_name == "であろう")
        .expect("であろう is a raw candidate");

    assert_eq!(aro_u.classes.as_ref(), ["S2C"]);
    assert!(
        constellations[0]
            .groups
            .iter()
            .flat_map(|group| group.alternatives.iter())
            .all(|candidate| candidate.raw_name != "ものである"),
        "もの must not extend through speculative あろう as ものである"
    );
}

#[test]
fn fragments_only_document_is_its_own_variant() {
    let lines = vec![vec![tok("走る", "動詞")]];
    assert!(matches!(
        analyze_tokens("fragment F := [upos=動詞].", &lines),
        SampleAnalysis::FragmentsOnly { .. }
    ));
}

#[test]
fn no_matches_is_distinct_from_fragments_only() {
    let lines = vec![vec![tok("猫", "名詞")]];
    assert!(matches!(
        analyze_tokens("pattern v := [upos=動詞].", &lines),
        SampleAnalysis::NoMatches { .. }
    ));
}

#[test]
fn compile_error_is_failed() {
    let lines = vec![vec![tok("猫", "名詞")]];
    assert!(matches!(
        analyze_tokens("pattern broken := [upos=", &lines),
        SampleAnalysis::Failed(_)
    ));
}

// ---------------------------------------------------------------------------
// Published composition reaches the sample view model.
// ---------------------------------------------------------------------------

fn composition_shape(constellation: &lingrep_web::sample::SampleConstellation) -> Vec<String> {
    constellation
        .composition
        .iter()
        .map(|item| match item {
            CompositionItem::Region(region) => {
                format!("R{}..{}", region.span.start, region.span.end)
            }
            CompositionItem::Gap(gap) => format!("G{}..{}", gap.span.start, gap.span.end),
        })
        .collect()
}

/// Total by construction so the helper itself never panics; a non-`Matched`
/// analysis surfaces as the caller's `expect`, inside the test where the
/// workspace panic floor permits it.
fn matched_constellations(
    source: &str,
    lines: &[Vec<TokenInfo>],
) -> Option<Vec<lingrep_web::sample::SampleConstellation>> {
    match analyze_tokens(source, lines) {
        SampleAnalysis::Matched { constellations, .. } => Some(constellations),
        _ => None,
    }
}

const SPLIT: &str = "pattern noun := [upos=名詞]. pattern verb := [upos=動詞].";

#[test]
fn a_one_token_gap_between_published_regions_is_carried_to_the_view() {
    let lines = vec![vec![
        tok("猫", "名詞"),
        tok("が", "助詞"),
        tok("走る", "動詞"),
    ]];

    let constellations = matched_constellations(SPLIT, &lines).expect("expected Matched");

    assert_eq!(
        composition_shape(&constellations[0]),
        ["R0..1", "G1..2", "R2..3"]
    );
    let CompositionItem::Gap(gap) = &constellations[0].composition[1] else {
        panic!("the middle entry is the gap")
    };
    assert_eq!(gap.tokens[0].orth.as_deref(), Some("が"));
    assert!(gap.within_initial_budget);
}

#[test]
fn a_gap_past_the_initial_budget_is_labeled_not_omitted() {
    let lines = vec![vec![
        tok("猫", "名詞"),
        tok("は", "助詞"),
        tok("とても", "副詞"),
        tok("速く", "形容詞"),
        tok("走る", "動詞"),
    ]];

    let constellations = matched_constellations(SPLIT, &lines).expect("expected Matched");

    let CompositionItem::Gap(gap) = &constellations[0].composition[1] else {
        panic!("the middle entry is the gap")
    };
    assert_eq!(gap.tokens.len(), 3, "every intervening token survives");
    assert!(!gap.within_initial_budget, "diagnostic, not within budget");
}

#[test]
fn overlapping_publications_reach_the_view_as_one_region() {
    let lines = vec![vec![tok("猫", "名詞"), tok("走る", "動詞")]];

    let constellations = matched_constellations(
        "pattern ab := [upos=名詞] [upos=動詞]. pattern b := [upos=動詞].",
        &lines,
    )
    .expect("expected Matched");

    assert_eq!(composition_shape(&constellations[0]), ["R0..2"]);
    let CompositionItem::Region(region) = &constellations[0].composition[0] else {
        panic!("the only entry is a region")
    };
    assert_eq!(region.occurrence_count, 2, "neither alternative is dropped");
}

#[test]
fn no_gap_is_invented_across_a_line_boundary() {
    // Line 1 ends with a published match; line 2 begins with one. Composition
    // is per line, so nothing bridges the boundary.
    let lines = vec![
        vec![tok("が", "助詞"), tok("猫", "名詞")],
        vec![tok("走る", "動詞"), tok("を", "助詞")],
    ];

    let constellations = matched_constellations(SPLIT, &lines).expect("expected Matched");

    assert_eq!(constellations.len(), 2);
    for constellation in &constellations {
        assert!(
            !constellation
                .composition
                .iter()
                .any(|item| matches!(item, CompositionItem::Gap(_))),
            "a GAP must never span two sentences"
        );
    }
}
