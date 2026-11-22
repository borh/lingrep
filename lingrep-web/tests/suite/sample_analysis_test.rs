use crate::support::tok;
use lingrep_core::token_match::TokenInfo;
use lingrep_web::inspection::CompositionItem;
use lingrep_web::sample::{SampleAnalysis, analyze_tokens};

#[test]
fn overlap_diagnostics_include_nested_and_crossing_candidates_but_exclude_adjacent_matches() {
    use lingrep_web::inspection::{OverlapCandidate, overlapping_candidates};
    let compiled = lingrep_web::patterns::compile_web_pattern(
        "output_name: \"selected\" a := 甲 乙. b := 乙 丙. c := 甲. d := 丙. policy resolve patterns rank: [longest, start, end, pattern]. end",
    ).unwrap();
    let tokens = vec![tok("甲", "名詞"), tok("乙", "名詞"), tok("丙", "名詞")];
    let explanation = compiled.matcher.explain_sentence(&tokens).unwrap();
    assert_eq!(
        overlapping_candidates(&explanation, "selected", 0..2),
        vec![
            OverlapCandidate {
                name: "c".into(),
                span: 0..1,
                published: false
            },
            OverlapCandidate {
                name: "b".into(),
                span: 1..3,
                published: false
            },
        ]
    );
    assert_eq!(
        overlapping_candidates(&explanation, "d", 2..3),
        vec![OverlapCandidate {
            name: "b".into(),
            span: 1..3,
            published: false
        },]
    );
}

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

/// One token per character, tagged 名詞.
fn characters(text: &str) -> Vec<TokenInfo> {
    text.chars().map(|c| tok(&c.to_string(), "名詞")).collect()
}

fn run_of(source: &str) -> lingrep_web::sample::SampleRun {
    let output =
        lingrep_web::patterns::compile_web_pattern(source).expect("the test catalog compiles");
    lingrep_web::sample::sample_run(
        &output,
        source,
        &lingrep_core::token_match::TokenSchema::unidic_morphology(),
        "test".into(),
        &characters,
    )
}

#[test]
fn claims_are_checked_against_the_definition_and_against_what_policy_publishes() {
    use lingrep_core::fsm::AttributeTestOutcome;
    use lingrep_web::sample::{
        ClaimFailure, ClaimFailureKind, ClaimsReport, SampleClaims, WhyNot, WhyNotElement,
        WhyNotOutcome, WhyNotStop, WhyNotStopKind,
    };
    let run = run_of(
        "examples: [\"甲乙\", \"丙\"] counterexamples: [\"甲\", \"丁\"] pattern a := 甲. \
         examples: [\"甲乙\"] pattern b := 甲 乙. \
         policy resolve patterns rank: [longest, start, end, pattern]. end",
    );
    let failure = |example: &str, kind, outcome: Option<(WhyNotOutcome, usize)>| ClaimFailure {
        definition: "a".into(),
        example: example.into(),
        kind,
        why_not: outcome.map(|(outcome, starts_tried)| WhyNot {
            definition: "a".into(),
            outcome,
            starts_tried,
            truncated: false,
            add_counterexample: None,
        }),
    };
    let stopped = WhyNotOutcome::Stopped(WhyNotStop {
        start: 0,
        consumed: 0,
        position: 0,
        token: Some("丙".into()),
        kind: WhyNotStopKind::Token,
        element: Some(WhyNotElement {
            line: 1,
            column: 64,
            text: "甲".into(),
        }),
        tests: vec![AttributeTestOutcome {
            attribute: "text".into(),
            test: "=甲".into(),
            actual: Some("丙".into()),
            held: false,
        }],
    });
    assert_eq!(
        run.claims,
        SampleClaims::Checked(ClaimsReport {
            checked: 5,
            failures: vec![
                failure("丙", ClaimFailureKind::ExpectedMatch, Some((stopped, 1))),
                failure("甲", ClaimFailureKind::UnexpectedMatch, None),
                failure(
                    "甲乙",
                    ClaimFailureKind::ExpectedPublication,
                    Some((
                        WhyNotOutcome::NotPublished {
                            spans: "0..1".into()
                        },
                        0
                    ))
                ),
            ],
        })
    );
    assert_eq!(run_of("pattern a := 甲.").claims, SampleClaims::Undeclared);
}

#[test]
fn an_attribute_the_tokenizer_lacks_is_named_and_leaves_claims_unchecked() {
    use lingrep_web::sample::{SampleBinding, SampleClaims, UnavailableAttribute};
    let run =
        run_of("examples: [\"甲\"] pattern a := [dep.rel=nsubj] [dep.rel=obj]. pattern b := 甲.");
    assert_eq!(
        run.binding,
        SampleBinding::Bound {
            unavailable: vec![UnavailableAttribute {
                attribute: "dep.rel".into(),
                definitions: vec!["a".into()],
            }],
        }
    );
    assert_eq!(run.claims, SampleClaims::Unverifiable);
}

#[test]
fn sample_binding_rejects_dependency_clause_without_dependency_attributes() {
    use lingrep_web::sample::SampleBinding;
    let run = run_of("pattern a := (?<n> [text=甲]) where (?<v> [text=乙]) -[nsubj]-> $n.");
    assert!(
        matches!(&run.binding, SampleBinding::Refused(reason) if reason.contains("dep.")),
        "{:?}",
        run.binding
    );
}

#[test]
fn a_sample_line_without_a_match_is_explained_for_each_definition() {
    use lingrep_web::sample::{WhyNotOutcome, WhyNotStopKind};
    let dictionary = lingrep_core::testing::MiniDictionary::new();
    let tokenizer =
        lingrep_core::dictionary::create_tokenizer_from_path(&dictionary.path().to_string_lossy())
            .expect("the dictionary loads");
    let plan = lingrep_core::analysis::VibratoPlan::new(tokenizer, dictionary.path())
        .expect("the plan builds");
    let report = lingrep_web::sample::analyze_sample(
        "pattern a := 猫 犬. pattern b := 猫.",
        "猫鳥\n猫犬\n鳥",
        &plan,
        "test".into(),
    );
    let unmatched = report.run.expect("the pattern compiles").unmatched;

    let summary = unmatched
        .iter()
        .map(|line| {
            let definitions = line
                .definitions
                .iter()
                .map(|why| {
                    let WhyNotOutcome::Stopped(stop) = &why.outcome else {
                        panic!("expected a stopped attempt: {why:?}");
                    };
                    assert_eq!(stop.kind, WhyNotStopKind::Token);
                    (
                        why.definition.as_str(),
                        stop.consumed,
                        stop.token.as_deref(),
                    )
                })
                .collect::<Vec<_>>();
            (line.text.as_str(), definitions)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        vec![("鳥", vec![("a", 0, Some("鳥")), ("b", 0, Some("鳥"))])],
        "猫鳥 is matched by b and 猫犬 by both, so only 鳥 is unmatched"
    );
}

#[test]
fn a_sample_sentence_can_be_inserted_into_a_definitions_claims_once() {
    use lingrep_web::sample::{ClaimInsert, SampleAnalysis};
    let dictionary = lingrep_core::testing::MiniDictionary::new();
    let tokenizer =
        lingrep_core::dictionary::create_tokenizer_from_path(&dictionary.path().to_string_lossy())
            .expect("the dictionary loads");
    let plan = lingrep_core::analysis::VibratoPlan::new(tokenizer, dictionary.path())
        .expect("the plan builds");
    let edits = |source: &str| {
        let report = lingrep_web::sample::analyze_sample(source, "猫\n鳥", &plan, "test".into());
        let SampleAnalysis::Matched { matches, .. } = report.analysis else {
            panic!("猫 matches");
        };
        let unmatched = report.run.expect("the pattern compiles").unmatched;
        (
            matches[0].add_example.clone(),
            unmatched[0].definitions[0].add_counterexample.clone(),
        )
    };
    let apply = |source: &str, edit: &ClaimInsert| {
        let (before, after) = source
            .split_at_checked(edit.offset)
            .expect("the offset is a character boundary");
        assert!(after.starts_with(&edit.expects));
        format!("{before}{}{after}", edit.text)
    };

    let bare = "pattern p := 猫.";
    let (example, counterexample) = edits(bare);
    let with_example = apply(bare, &example.expect("猫 is not yet an example"));
    assert_eq!(with_example, "examples: [\"猫\"]\npattern p := 猫.");
    assert_eq!(
        apply(
            bare,
            &counterexample.expect("鳥 is not yet a counterexample")
        ),
        "counterexamples: [\"鳥\"]\npattern p := 猫."
    );

    let listed = "examples: [\"犬]\", \"猫\"] counterexamples: [] pattern p := 猫.";
    let (example, counterexample) = edits(listed);
    assert_eq!(example, None, "猫 is already an example");
    assert_eq!(
        apply(listed, &counterexample.expect("the list is empty")),
        "examples: [\"犬]\", \"猫\"] counterexamples: [\"鳥\"] pattern p := 猫."
    );

    let appended = "examples: [\"犬]\"] pattern p := 猫.";
    assert_eq!(
        apply(appended, &edits(appended).0.expect("猫 is not listed")),
        "examples: [\"犬]\", \"猫\"] pattern p := 猫."
    );

    let inherited = "defaults\n  examples: [\"犬\"]\nend\npattern p := 猫.";
    assert_eq!(edits(inherited).0, None, "a default's list is not edited");
}
