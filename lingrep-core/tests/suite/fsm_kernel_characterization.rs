//! Golden characterization of the NFA matching kernel's emission path.
//!
//! Pins whole `Vec<Match>` output (pattern, span, context windows, captures)
//! for the emission cases the decomplection touches: plain, quantified,
//! cross-pattern canonical ordering, and no-match. Captures / same_as /
//! lookaround emission is unchanged by the refactor and stays covered by the
//! existing gate targets (named_groups_test, span_equality_test, oracle).
//!
//! Expected values are derived from the documented contract, not copied from
//! observed output. If any assertion fails on the base commit BEFORE any
//! refactor edit, STOP and characterize the surprise rather than "fixing" the
//! expectation.

use lingrep_core::fsm::{
    Capture, CompileLimits, CompiledCatalog, GraphBinding, Match, Measurement, PatternSource,
    compile_catalog,
};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

fn token(orth: &str) -> TokenInfo {
    let mut t = TokenInfo::default();
    t.set(TokenAttributeIndex::Orth, orth.into());
    t
}

fn matcher(source: &str) -> CompiledCatalog {
    compile_catalog(
        PatternSource::named("golden.lx", source),
        CompileLimits::default(),
    )
    .expect("golden fixture compiles")
    .matcher
}

/// A Match with no captures, spelled out for equality comparison.
fn m(pattern: &str, start: usize, end: usize) -> Match {
    Match {
        pattern: pattern.into(),
        start,
        end,
        captures: Box::default(),
        graph_bindings: Box::default(),
        interpretations: Box::default(),
        lexical: None,
    }
}

#[test]
fn plain_single_token_pattern_emits_each_hit_with_clamped_context() {
    // tokens: [a, b, a] (len 3); pattern matches each single "a".
    let tokens = [token("a"), token("b"), token("a")];
    let got = matcher("p := [text=a].").find_matches(&tokens);
    assert_eq!(got, vec![m("p", 0, 1), m("p", 2, 3),],);
}

#[test]
fn quantified_run_keeps_longest_per_start_at_equal_end() {
    // tokens: [a, a, b] (len 3); [text=a]+ per-start longest = (0,2) and (1,2).
    let tokens = [token("a"), token("a"), token("b")];
    let got = matcher("q := [text=a]+.").find_matches(&tokens);
    assert_eq!(got, vec![m("q", 0, 2), m("q", 1, 2),],);
}

#[test]
fn cross_pattern_ties_emit_in_canonical_pattern_order() {
    // tokens: [x] (len 1); patterns "a" and "b" both match (0,1).
    // Canonical (start, end, pattern) order puts "a" before "b".
    let tokens = [token("x")];
    let got = matcher("a := [text=x].\nb := [text=x].").find_matches(&tokens);
    assert_eq!(got, vec![m("a", 0, 1), m("b", 0, 1),],);
}

#[test]
fn no_match_returns_empty() {
    let tokens = [token("a"), token("a")];
    assert_eq!(
        matcher("p := [text=z].").find_matches(&tokens),
        Vec::<Match>::new()
    );
}

#[test]
fn empty_tokens_return_empty() {
    let empty: [TokenInfo; 0] = [];
    assert_eq!(
        matcher("p := [text=a].").find_matches(&empty),
        Vec::<Match>::new()
    );
}

const NESTING: &str = "[text=の] | [text=猫] [text=の] [text=手]";

fn cat_s_paw() -> [TokenInfo; 3] {
    [token("猫"), token("の"), token("手")]
}

#[test]
fn a_match_nested_in_the_definition_s_earlier_match_is_dropped_by_either_executor() {
    let count = |value| {
        vec![GraphBinding::Measure {
            name: "n".into(),
            value: Measurement::Number {
                sum: value,
                count: std::num::NonZeroU64::MIN,
            },
        }]
        .into_boxed_slice()
    };
    let source = format!(
        "plain := {NESTING}.\n\
         meet := ({NESTING}) & []+.\n\
         measured := {NESTING} measure count(match.words[]) as $n.\n\
         inner := [text=の] measure count(match.words[]) as $n."
    );
    assert_eq!(
        matcher(&source).find_matches(&cat_s_paw()),
        vec![
            Match {
                graph_bindings: count(3),
                ..m("measured", 0, 3)
            },
            m("meet", 0, 3),
            m("plain", 0, 3),
            // Another definition's longer match drops nothing.
            Match {
                graph_bindings: count(1),
                ..m("inner", 1, 2)
            },
        ],
    );
}

#[test]
fn a_tree_evaluated_run_keeps_a_later_start_that_reaches_the_same_end() {
    let tokens = [token("a"), token("a"), token("b")];
    let got = matcher("q := [text=a]+ & []+.").find_matches(&tokens);
    assert_eq!(got, vec![m("q", 0, 2), m("q", 1, 2)]);
}

#[test]
fn any_order_spans_from_one_start_survive_and_a_later_nested_span_is_dropped() {
    let capture = |name: &str, start: usize| Capture {
        name: name.into(),
        path: format!("p/{name}").into(),
        start,
        end: start + 1,
    };
    let hit = |start: usize, end: usize, captures: [Capture; 2]| Match {
        captures: captures.into(),
        ..m("p", start, end)
    };
    let tokens = [token("y"), token("q"), token("y"), token("z")];
    assert_eq!(
        matcher("p := any_order((?<a> []), (?<b> [text=y])).").find_matches(&tokens),
        vec![
            hit(0, 3, [capture("a", 0), capture("b", 2)]),
            hit(0, 4, [capture("b", 0), capture("a", 3)]),
            // 1..3 lies inside 0..4 and is dropped; 2..4 ends where 0..4 does.
            hit(2, 4, [capture("b", 2), capture("a", 3)]),
        ],
    );
}
