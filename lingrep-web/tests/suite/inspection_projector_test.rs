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
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use hegel::generators;
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};
use lingrep_web::inspection::{
    CaptureLane, CaptureOrdinal, CompositionItem, InspectionProjector, ResolvedCapture,
    TOKEN_FIELD_CATALOG, TokenRole, assign_capture_lanes, project_sentence_patterns,
};

#[test]
fn catalog_covers_all_attributes_once() {
    let attributes = TOKEN_FIELD_CATALOG
        .iter()
        .map(|field| field.attribute)
        .collect::<BTreeSet<_>>();
    assert_eq!(attributes.len(), TokenAttributeIndex::ALL.len());
    assert!(
        TokenAttributeIndex::ALL
            .into_iter()
            .all(|item| attributes.contains(&item))
    );
}

#[test]
fn projection_preserves_missing_empty_and_nonempty_values() {
    let token = TokenInfo::default().with_orth("食べる").with_lemma("");
    let projected = InspectionProjector::project_token(&token);
    assert_eq!(
        projected.field("orth").unwrap().value.as_deref(),
        Some("食べる")
    );
    assert_eq!(projected.field("lemma").unwrap().value.as_deref(), Some(""));
    assert_eq!(projected.field("pron").unwrap().value, None);
    assert_eq!(projected.available_fields, 2);
}

#[test]
fn match_window_keeps_sentence_indices_and_roles() {
    let tokens = (0..6)
        .map(|index| TokenInfo::default().with_orth(format!("{index}")))
        .collect::<Vec<_>>();
    let projected = InspectionProjector::project_window(&tokens, 2..4, 1);
    assert_eq!(projected.visible_range, 1..5);
    assert_eq!(
        projected
            .tokens
            .iter()
            .map(|token| (token.sentence_index, token.role))
            .collect::<Vec<_>>(),
        vec![
            (1, TokenRole::Context),
            (2, TokenRole::Match),
            (3, TokenRole::Match),
            (4, TokenRole::Context),
        ]
    );
}

fn capture(index: usize, path: &str, span: Range<usize>) -> ResolvedCapture {
    ResolvedCapture {
        ordinal: CaptureOrdinal::new(index),
        name: path.rsplit('/').next().unwrap().to_owned(),
        path: path.to_owned(),
        span,
    }
}

fn tokens(count: usize) -> Vec<TokenInfo> {
    (0..count)
        .map(|index| TokenInfo::default().with_orth(index.to_string()))
        .collect()
}

#[test]
fn constellation_groups_equal_spans_and_uses_the_nearest_container_as_parent() {
    let matcher = lingrep_core::fsm::compile_catalog(
        lingrep_core::fsm::PatternSource::named(
            "constellation.lx",
            "outer := [text=甲] [text=乙] [text=丙].\n\
             inner_one := [text=乙].\n\
             inner_two := [text=乙].\n",
        ),
        lingrep_core::fsm::CompileLimits::default(),
    )
    .expect("catalog compiles")
    .matcher;
    let sentence = vec![
        TokenInfo::default().with_orth("甲"),
        TokenInfo::default().with_orth("乙"),
        TokenInfo::default().with_orth("丙"),
    ];
    let explanation = matcher
        .explain_sentence(&sentence)
        .expect("policy-free explanation succeeds");

    let constellation =
        project_sentence_patterns(&matcher, &explanation, &sentence, 0).constellation;

    assert_eq!(constellation.groups[0].span, 0..3);
    assert_eq!(constellation.groups[1].span, 1..2);
    assert_eq!(constellation.groups[1].parent, Some(0));
    assert_eq!(constellation.groups[1].alternatives.len(), 2);
}

#[test]
fn constellation_reads_research_classes_only_as_optional_badges() {
    let matcher = lingrep_core::fsm::compile_catalog(
        lingrep_core::fsm::PatternSource::named(
            "constellation.lx",
            "research_classes: [\"S2C\"]\n\
             であろう := [text=で] [text=あろう].\n",
        ),
        lingrep_core::fsm::CompileLimits::default(),
    )
    .expect("catalog compiles")
    .matcher;
    let sentence = vec![
        TokenInfo::default().with_orth("で"),
        TokenInfo::default().with_orth("あろう"),
    ];
    let explanation = matcher
        .explain_sentence(&sentence)
        .expect("policy-free explanation succeeds");

    let node = project_sentence_patterns(&matcher, &explanation, &sentence, 0)
        .constellation
        .groups
        .into_iter()
        .flat_map(|group| group.alternatives)
        .find(|node| node.raw_name == "であろう")
        .expect("matching pattern is projected");

    assert_eq!(node.classes.as_ref(), ["S2C"]);
}

#[test]
fn nested_captures_get_separate_deterministic_lanes() {
    let captures = vec![capture(0, "outer", 1..5), capture(1, "inner", 2..4)];
    let projected = InspectionProjector::project_match(&tokens(6), 1..5, &captures, 1);
    assert_eq!(projected.capture_lanes.len(), 2);
    assert_eq!(
        projected.capture_lanes[0].captures.as_ref(),
        [CaptureOrdinal::new(0)]
    );
    assert_eq!(
        projected.capture_lanes[1].captures.as_ref(),
        [CaptureOrdinal::new(1)]
    );
}

#[test]
fn adjacent_captures_share_a_lane_and_project_membership_and_text() {
    let captures = vec![capture(0, "left", 1..2), capture(1, "right", 2..3)];
    let projected = InspectionProjector::project_match(&tokens(4), 1..3, &captures, 1);
    assert_eq!(projected.capture_lanes.len(), 1);
    assert_eq!(projected.captures[0].text, "1");
    assert_eq!(
        projected.tokens[1].capture_membership.as_ref(),
        [CaptureOrdinal::new(0)]
    );
}

fn lane_map(lanes: &[CaptureLane]) -> BTreeMap<CaptureOrdinal, usize> {
    lanes
        .iter()
        .enumerate()
        .flat_map(|(lane, item)| item.captures.iter().map(move |capture| (*capture, lane)))
        .collect()
}

fn overlaps(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

#[hegel::test(test_cases = 300, derandomize = true)]
fn lane_assignment_is_input_order_independent_and_non_overlapping(tc: hegel::TestCase) {
    let raw: Vec<(u8, u8)> = tc.draw(
        generators::vecs(generators::tuples!(
            generators::integers::<u8>().max_value(31),
            generators::integers::<u8>().max_value(8),
        ))
        .max_size(24),
    );
    let captures = raw
        .into_iter()
        .enumerate()
        .map(|(index, (start, width))| {
            let start = usize::from(start);
            let end = start.saturating_add(usize::from(width)).min(32);
            capture(index, &format!("p/{index}"), start..end)
        })
        .collect::<Vec<_>>();
    let ranges = captures
        .iter()
        .map(|capture| (capture.ordinal, capture.span.clone()))
        .collect::<BTreeMap<_, _>>();
    let baseline = assign_capture_lanes(&captures);
    let mut reordered = captures.clone();
    reordered.reverse();
    if !reordered.is_empty() {
        let rotation = tc.draw(generators::integers::<usize>().max_value(reordered.len() - 1));
        reordered.rotate_left(rotation);
    }
    let permuted = assign_capture_lanes(&reordered);
    assert_eq!(lane_map(&baseline), lane_map(&permuted));
    for lane in &baseline {
        for left in 0..lane.captures.len() {
            for right in left + 1..lane.captures.len() {
                assert!(!overlaps(
                    &ranges[&lane.captures[left]],
                    &ranges[&lane.captures[right]]
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Published composition: regions, gaps, and the coordinate rule.
// ---------------------------------------------------------------------------

/// Renders a composition sequence as compact shape strings so a test asserts
/// the whole alternation at once instead of indexing into it.
fn shape(items: &[CompositionItem]) -> Vec<String> {
    items
        .iter()
        .map(|item| match item {
            CompositionItem::Region(region) => {
                format!("R{}..{}", region.span.start, region.span.end)
            }
            CompositionItem::Gap(gap) => format!("G{}..{}", gap.span.start, gap.span.end),
        })
        .collect()
}

fn compiled(source: &str) -> lingrep_core::fsm::CompiledCatalog {
    lingrep_core::fsm::compile_catalog(
        lingrep_core::fsm::PatternSource::named("composition.lx", source),
        lingrep_core::fsm::CompileLimits::default(),
    )
    .expect("catalog compiles")
    .matcher
}

fn sentence_of(orths: &[&str]) -> Vec<TokenInfo> {
    orths
        .iter()
        .map(|orth| TokenInfo::default().with_orth(*orth))
        .collect()
}

fn compose(
    source: &str,
    orths: &[&str],
    base_index: usize,
) -> lingrep_web::inspection::SentencePatternAnalysis {
    let matcher = compiled(source);
    let tokens = sentence_of(orths);
    let explanation = matcher
        .explain_sentence(&tokens)
        .expect("policy-free explanation succeeds");
    project_sentence_patterns(&matcher, &explanation, &tokens, base_index)
}

const SPLIT: &str = "pattern left := [text=甲].\npattern right := [text=丙].\n";

#[test]
fn one_literal_token_between_regions_is_a_visible_gap() {
    let analysis = compose(SPLIT, &["甲", "間", "丙"], 0);

    assert_eq!(shape(&analysis.composition), ["R0..1", "G1..2", "R2..3"]);
    let CompositionItem::Gap(gap) = &analysis.composition[1] else {
        panic!("the middle entry is the gap")
    };
    assert_eq!(gap.tokens.len(), 1);
    assert_eq!(gap.tokens[0].orth.as_deref(), Some("間"));
    assert!(gap.within_initial_budget);
}

#[test]
fn a_gap_wider_than_the_initial_budget_stays_whole() {
    let analysis = compose(SPLIT, &["甲", "間", "隙", "空", "丙"], 0);

    assert_eq!(shape(&analysis.composition), ["R0..1", "G1..4", "R4..5"]);
    let CompositionItem::Gap(gap) = &analysis.composition[1] else {
        panic!("the middle entry is the gap")
    };
    // Diagnostic, never elided: every intervening token survives.
    assert_eq!(
        gap.tokens
            .iter()
            .map(|token| token.orth.as_deref().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["間", "隙", "空"]
    );
    assert!(!gap.within_initial_budget);
}

#[test]
fn adjacent_regions_stay_separate_and_expose_an_empty_gap() {
    let analysis = compose(SPLIT, &["甲", "丙"], 0);

    assert_eq!(shape(&analysis.composition), ["R0..1", "G1..1", "R1..2"]);
    let CompositionItem::Gap(gap) = &analysis.composition[1] else {
        panic!("the middle entry is the gap")
    };
    assert!(gap.tokens.is_empty(), "GAP 0 has no tokens");
    assert!(gap.within_initial_budget);
}

#[test]
fn strictly_overlapping_publications_merge_into_one_region() {
    let analysis = compose(
        "pattern ab := [text=甲] [text=乙].\npattern bc := [text=乙] [text=丙].\n",
        &["甲", "乙", "丙"],
        0,
    );

    assert_eq!(shape(&analysis.composition), ["R0..3"]);
    let CompositionItem::Region(region) = &analysis.composition[0] else {
        panic!("the only entry is a region")
    };
    assert_eq!(region.occurrence_count, 2);
}

#[test]
fn every_published_alternative_is_counted_into_exactly_one_region() {
    let analysis = compose(
        "pattern ab := [text=甲] [text=乙].\npattern bc := [text=乙] [text=丙].\n",
        &["甲", "乙", "丙"],
        0,
    );

    let published = analysis
        .constellation
        .groups
        .iter()
        .flat_map(|group| group.alternatives.iter())
        .filter(|alternative| alternative.published.is_some())
        .count();
    let counted: usize = analysis
        .composition
        .iter()
        .filter_map(|item| match item {
            CompositionItem::Region(region) => Some(region.occurrence_count),
            CompositionItem::Gap(_) => None,
        })
        .sum();

    assert!(published > 0, "the fixture must publish something");
    assert_eq!(counted, published, "no published alternative is discarded");
}

#[test]
fn base_index_is_applied_exactly_once() {
    let analysis = compose(SPLIT, &["甲", "間", "丙"], 10);

    // Same three-token slice; only the reported coordinates move.
    assert_eq!(
        shape(&analysis.composition),
        ["R10..11", "G11..12", "R12..13"]
    );
    let CompositionItem::Gap(gap) = &analysis.composition[1] else {
        panic!("the middle entry is the gap")
    };
    assert_eq!(gap.tokens[0].orth.as_deref(), Some("間"));
}

#[test]
fn class_summary_is_deduplicated_in_first_occurrence_order() {
    let analysis = compose(
        "research_classes: [\"S2E\", \"S3C\"]\n\
         pattern ab := [text=甲] [text=乙].\n\
         research_classes: [\"S3C\"]\n\
         pattern bc := [text=乙] [text=丙].\n",
        &["甲", "乙", "丙"],
        0,
    );

    let CompositionItem::Region(region) = &analysis.composition[0] else {
        panic!("the only entry is a region")
    };
    // S3C stays visible on its own terms; nothing invents an S2->S3 sequence.
    assert_eq!(region.class_summary.as_ref(), ["S2E", "S3C"]);
}

#[test]
fn a_detached_tail_cannot_erase_itself_from_a_gap() {
    // Hand-built because no ordinary source syntax expresses a raw/published
    // span pair this compactly: the first candidate recognized 0..3 but
    // published only 0..2.
    let matcher = compiled(SPLIT);
    let tokens = sentence_of(&["甲", "乙", "丙"]);
    let explanation = lingrep_core::fsm::SentenceExplanation {
        candidates: Box::new([
            lingrep_core::fsm::ExplainedMatch {
                raw: lingrep_core::fsm::Match {
                    pattern: "left".into(),
                    start: 0,
                    end: 3,
                    captures: Box::default(),
                    graph_bindings: Box::default(),
                    lexical: None,
                },
                published: Some(lingrep_core::fsm::Match {
                    pattern: "left".into(),
                    start: 0,
                    end: 2,
                    captures: Box::default(),
                    graph_bindings: Box::default(),
                    lexical: None,
                }),
            },
            lingrep_core::fsm::ExplainedMatch {
                raw: lingrep_core::fsm::Match {
                    pattern: "right".into(),
                    start: 2,
                    end: 3,
                    captures: Box::default(),
                    graph_bindings: Box::default(),
                    lexical: None,
                },
                published: Some(lingrep_core::fsm::Match {
                    pattern: "right".into(),
                    start: 2,
                    end: 3,
                    captures: Box::default(),
                    graph_bindings: Box::default(),
                    lexical: None,
                }),
            },
        ]),
    };

    let analysis = project_sentence_patterns(&matcher, &explanation, &tokens, 0);

    // The published span wins; the raw 0..3 does not swallow the second region.
    assert_eq!(shape(&analysis.composition), ["R0..2", "G2..2", "R2..3"]);
    assert_eq!(
        analysis.constellation.groups[0].span,
        0..3,
        "raw evidence is untouched"
    );
}

#[test]
fn suppressed_candidates_publish_no_composition() {
    let matcher = compiled(SPLIT);
    let tokens = sentence_of(&["甲", "丙"]);
    let explanation = lingrep_core::fsm::SentenceExplanation {
        candidates: Box::new([lingrep_core::fsm::ExplainedMatch {
            raw: lingrep_core::fsm::Match {
                pattern: "left".into(),
                start: 0,
                end: 1,
                captures: Box::default(),
                graph_bindings: Box::default(),
                lexical: None,
            },
            published: None,
        }]),
    };

    let analysis = project_sentence_patterns(&matcher, &explanation, &tokens, 0);

    assert!(analysis.composition.is_empty(), "policy published nothing");
    assert!(
        !analysis.constellation.groups.is_empty(),
        "the raw candidate is still explained"
    );
}
