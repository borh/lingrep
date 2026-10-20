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
//! Expression intersection: `E1 & E2` matches where both operands match the
//! same span.

use crate::support::refuses_with;
use lingrep_core::fsm::{
    CompileError, CompileLimits, CompiledCatalog, DiagnosticCode, PatternSource, compile_catalog,
};
use lingrep_core::testing::token;
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

/// One token per character, so a test case reads as a string.
fn tokens(word: &str) -> Vec<TokenInfo> {
    word.chars().map(|c| token(&c.to_string())).collect()
}

fn compile(source: &str) -> CompiledCatalog {
    compile_catalog(
        PatternSource::named("intersection.lx", source),
        CompileLimits::default(),
    )
    .unwrap_or_else(|error| panic!("compile: {error}"))
    .matcher
}

fn compile_err(source: &str) -> CompileError {
    match compile_catalog(
        PatternSource::named("intersection.lx", source),
        CompileLimits::default(),
    ) {
        Ok(_) => panic!("expected a compile error, but this compiled: {source}"),
        Err(error) => error,
    }
}

/// The set of spans the catalog's single pattern reports over `word`.
fn spans(matcher: &CompiledCatalog, word: &str) -> Vec<(usize, usize)> {
    matcher
        .find_matches(&tokens(word))
        .into_iter()
        .map(|matched| (matched.start, matched.end))
        .collect()
}

/// Whether `body` accepts `word` in its entirety.
///
/// Anchored at both ends, because the question every test below asks is
/// language membership, and an unanchored pattern answers the different
/// question of whether some substring is a member.
fn accepts(matcher: &CompiledCatalog, word: &str) -> bool {
    !matcher.find_matches(&tokens(word)).is_empty()
}

fn anchored(body: &str) -> CompiledCatalog {
    compile(&format!("pattern p := ^ {body} $.\n"))
}

// ---------------------------------------------------------------------------
// Precedence. Pinned on the typed program's behavior rather than on parse
// trees — the grammar repository already pins the trees, and what this
// repository has to guarantee is that the compiled meaning agrees with them.
// ---------------------------------------------------------------------------

/// `a b & c d` is `(a b) & (c d)`, not `a (b & c) d`.
///
/// The two readings are distinguishable: under the intended one the pattern
/// matches a two-token span where both sequences agree, and under the other it
/// would want four tokens.
#[test]
fn intersection_binds_looser_than_sequence() {
    let matcher = anchored("[text=a] [text=b] & [text=a] []");

    assert!(accepts(&matcher, "ab"), "both sequences accept `ab`");
    assert!(
        !accepts(&matcher, "ac"),
        "the left operand rejects `ac`, so the intersection must"
    );
    assert!(
        !accepts(&matcher, "abab"),
        "four tokens would only match if `&` bound tighter than sequence"
    );
}

/// `a & b | c & d` is `(a & b) | (c & d)`.
#[test]
fn intersection_binds_tighter_than_alternation() {
    let matcher = anchored("[text=a] & [] | [text=c] & []");

    assert!(accepts(&matcher, "a"), "the first intersection accepts");
    assert!(accepts(&matcher, "c"), "the second intersection accepts");
    assert!(
        !accepts(&matcher, "b"),
        "neither intersection accepts `b`; if `&` bound looser this would be \
         `a & (b | c) & d` and reject everything"
    );
}

/// A chain is one n-ary node, and every operand must agree.
#[test]
fn a_chain_of_intersections_requires_every_operand() {
    let matcher = anchored("[text in {a, b}] & [text in {b, c}] & [text in {b, d}]");

    assert!(accepts(&matcher, "b"), "`b` is in all three");
    for rejected in ["a", "c", "d"] {
        assert!(
            !accepts(&matcher, rejected),
            "`{rejected}` is in only one operand"
        );
    }
}

/// Parentheses put an intersection inside a sequence.
#[test]
fn a_parenthesized_intersection_composes_into_a_sequence() {
    let matcher = anchored("([text in {a, b}] & [text in {b, c}]) [text=z]");

    assert!(accepts(&matcher, "bz"));
    assert!(!accepts(&matcher, "az"));
    assert!(!accepts(&matcher, "b"), "the sequence still needs its tail");
}

/// `&` inside `[…]` is attribute conjunction and does not become an
/// intersection node.
///
/// The two spellings share a glyph, and this is what keeps that a consistency
/// argument rather than an ambiguity: attribute conjunction is only reachable
/// inside brackets, which have closed before an expression-level `&` can
/// appear.
#[test]
fn attribute_conjunction_is_not_expression_intersection() {
    let matcher = anchored("[text=a & lemma=A] & [text=a]");
    let mut both = token("a");
    both.set(TokenAttributeIndex::Lemma, "A".into());

    assert!(
        !matcher.find_matches(&[both]).is_empty(),
        "one token satisfying the conjunction and the second operand"
    );
    assert!(
        !accepts(&matcher, "a"),
        "a token with no lemma fails the conjunction, so the whole fails"
    );
}

// ---------------------------------------------------------------------------
// Semantics.
// ---------------------------------------------------------------------------

/// The same span, not merely the same start or the same end.
///
/// The left operand accepts one or two tokens and the right accepts exactly
/// two, so a start where both can begin still yields only the two-token span.
#[test]
fn operands_must_agree_on_the_whole_span() {
    let matcher = anchored("[]{1,2} & [] []");

    assert!(
        !accepts(&matcher, "a"),
        "the right operand needs two tokens"
    );
    assert!(accepts(&matcher, "ab"));
    assert!(!accepts(&matcher, "abc"), "the left operand stops at two");
}

/// `A & A` is `A`: the idempotent law, checked against the operand alone.
#[test]
fn intersection_is_idempotent() {
    let doubled = compile("pattern p := [text in {a, b}]+ & [text in {a, b}]+.\n");
    let single = compile("pattern p := [text in {a, b}]+.\n");

    for word in ["a", "ab", "aabb", "abc", "c"] {
        assert_eq!(
            spans(&doubled, word),
            spans(&single, word),
            "`A & A` and `A` disagree on `{word}`"
        );
    }
}

/// `A & B` is `B & A`.
///
/// True only because captures are refused in *every* operand: allowing them on
/// one side would make the operand order observable in the emitted `Match`.
#[test]
fn intersection_is_commutative() {
    let forward = compile("pattern p := [text in {a, b}]+ & []{2}.\n");
    let reversed = compile("pattern p := []{2} & [text in {a, b}]+.\n");

    for word in ["a", "ab", "aab", "abc", "ba"] {
        assert_eq!(
            spans(&forward, word),
            spans(&reversed, word),
            "`A & B` and `B & A` disagree on `{word}`"
        );
    }
}

/// `(A & B) & C` is `A & (B & C)`.
#[test]
fn intersection_is_associative() {
    let left = compile("pattern p := ([text in {a, b}]+ & []{2,3}) & [text in {b, c}]+.\n");
    let right = compile("pattern p := [text in {a, b}]+ & ([]{2,3} & [text in {b, c}]+).\n");

    for word in ["b", "bb", "bbb", "ab", "bc", "abc"] {
        assert_eq!(
            spans(&left, word),
            spans(&right, word),
            "the two groupings disagree on `{word}`"
        );
    }
}

/// A lookaround inside `&` is allowed, and is consulted by both operands.
#[test]
fn lookaround_composes_inside_an_intersection() {
    let matcher = compile("pattern p := ([text=a] followed_by([text=b])) & [].\n");

    assert_eq!(spans(&matcher, "ab"), vec![(0, 1)]);
    assert!(
        spans(&matcher, "ac").is_empty(),
        "the zero-width assertion still gates the intersection"
    );
}

/// `&` composes beneath a quantifier.
#[test]
fn an_intersection_composes_beneath_a_quantifier() {
    let matcher = anchored("([text in {a, b}] & [text in {b, c}]){2}");

    assert!(accepts(&matcher, "bb"));
    assert!(!accepts(&matcher, "ab"));
}

/// `&` composes through a fragment reference.
///
/// The shape `patterns/double-wo.lx` needs: the intersection lives in a
/// fragment, and the emitting pattern only references it.
#[test]
fn an_intersection_composes_through_a_fragment_reference() {
    let matcher = compile(
        "fragment inner := [text in {a, b}]+ & []{2}.\npattern p := [text=z] @inner [text=z].\n",
    );

    assert_eq!(spans(&matcher, "zabz"), vec![(0, 4)]);
    assert!(
        spans(&matcher, "zaz").is_empty(),
        "the fragment's `[]{{2}}` operand still binds inside the reference"
    );
}

// ---------------------------------------------------------------------------
// The design document's §2 witness.
//
// This is the sharpest available test, because a careful hand analysis of the
// same equivalence produced a *wrong* rewrite on the first attempt, and the
// two-token string `SB` is what caught it. The classes are forced by the
// predicates `patterns/double-wo.lx` actually uses:
//
//   N — ordinary gap token:              in gap, in nbgap, not a boundary
//   B — `orthBase=　 & unidic.pos1=空白`:   in gap, not in nbgap, IS a boundary
//   S — `orthBase=　`, other pos1:   in gap, not in nbgap, not a boundary
//
// `S` is the whole difficulty: the shipped alternation requires every token
// before the first boundary to be `nbgap`, which excludes `S`, while the
// tempting rewrite only required them to be `gap`, which admits it.
// ---------------------------------------------------------------------------

const CLASSES: &str = "\
fragment gap := [text in {N, B, S}].
fragment nbgap := [text=N].
fragment bnd := [text=B].
";

/// The eight branches `patterns/double-wo.lx` ships, verbatim in shape.
const SHIPPED: &str = "\
  @nbgap{0} @bnd @gap{0,7}
| @nbgap{1} @bnd @gap{0,6}
| @nbgap{2} @bnd @gap{0,5}
| @nbgap{3} @bnd @gap{0,4}
| @nbgap{4} @bnd @gap{0,3}
| @nbgap{5} @bnd @gap{0,2}
| @nbgap{6} @bnd @gap{0,1}
| @nbgap{7} @bnd\
";

const CORRECTED: &str = "( @nbgap* @bnd @gap* ) & []{1,8}";
const NAIVE: &str = "@gap{1,8} & ( []* @bnd []* )";

/// Every string of length at most `MAX_LEN` over the three classes.
const MAX_LEN: usize = 8;

fn class_strings() -> Vec<String> {
    let mut all = vec![String::new()];
    let mut frontier = vec![String::new()];
    for _ in 0..MAX_LEN {
        let mut next = Vec::new();
        for word in &frontier {
            for class in ['N', 'B', 'S'] {
                let mut grown = word.clone();
                grown.push(class);
                next.push(grown);
            }
        }
        all.extend(next.iter().cloned());
        frontier = next;
    }
    all
}

fn anchored_with_classes(body: &str) -> CompiledCatalog {
    compile(&format!("{CLASSES}pattern p := ^ {body} $.\n"))
}

/// The corrected rewrite is *exactly* equivalent to the eight branches.
///
/// Exhaustive over the induced regular language, not sampled: the classes are
/// a genuine partition of what the predicates can distinguish, so agreement on
/// every string up to the length bound is agreement on the language.
#[test]
fn the_intersection_rewrite_is_exactly_the_eight_branch_expansion() {
    let shipped = anchored_with_classes(&format!("( {SHIPPED} )"));
    let corrected = anchored_with_classes(CORRECTED);

    let mismatches = class_strings()
        .into_iter()
        .filter(|word| accepts(&shipped, word) != accepts(&corrected, word))
        .collect::<Vec<_>>();

    assert!(
        mismatches.is_empty(),
        "the rewrite is not equivalent; shortest witness: {:?}",
        mismatches.first()
    );
}

/// The rewrite that looked right and was not.
///
/// Kept as a test rather than a comment because it is the evidence that the
/// previous test is not vacuous: the same harness, one operand written the
/// obvious way, and the languages differ. Were both rewrites to pass, the
/// exhaustive check would be proving nothing.
#[test]
fn the_naive_rewrite_over_matches_and_sb_is_the_witness() {
    let shipped = anchored_with_classes(&format!("( {SHIPPED} )"));
    let naive = anchored_with_classes(NAIVE);

    assert!(!accepts(&shipped, "SB"), "the catalog rejects `SB`");
    assert!(accepts(&naive, "SB"), "the naive rewrite admits `SB`");

    let mismatches = class_strings()
        .into_iter()
        .filter(|word| accepts(&shipped, word) != accepts(&naive, word))
        .count();
    assert!(
        mismatches > 0,
        "if this ever reaches zero, the two languages have converged and the \
         equivalence test above has stopped discriminating"
    );
}

// ---------------------------------------------------------------------------
// Refusals.
// ---------------------------------------------------------------------------

/// Disjoint operand widths prove the intersection empty.
#[test]
fn an_intersection_of_disjoint_widths_is_a_compile_error() {
    let error = compile_err("pattern p := [] & [] [].\n");
    let text = error.to_string();

    assert!(
        refuses_with(&error, DiagnosticCode::EmptyIntersection),
        "{text}"
    );
    assert!(
        text.contains("at least 2") && text.contains("at most 1"),
        "the diagnostic reports the two intervals that cannot meet: {text}"
    );
}

/// Sound but incomplete: overlapping widths prove nothing, so they compile.
#[test]
fn an_intersection_of_overlapping_widths_compiles() {
    let matcher = anchored("[]{1,3} & []{2,4}");

    assert!(!accepts(&matcher, "a"), "below the common interval");
    assert!(accepts(&matcher, "ab"));
    assert!(accepts(&matcher, "abc"));
    assert!(!accepts(&matcher, "abcd"), "above the common interval");
}

/// Overlapping widths that share no string still compile.
///
/// The other half of "sound but incomplete", written down so the incompleteness
/// is a recorded decision rather than an untested claim: these operands agree
/// on width and share no token, and deciding that needs language emptiness,
/// which is not a width question.
#[test]
fn widths_that_overlap_but_share_no_string_still_compile() {
    let matcher = anchored("[text=a] & [text=b]");

    assert!(!accepts(&matcher, "a"));
    assert!(!accepts(&matcher, "b"));
}

/// A capture is refused in every operand, not just the
/// second, so that `&` keeps the commutativity the algebra tests assert.
#[test]
fn a_capture_inside_an_intersection_is_refused_on_either_side() {
    for source in [
        "pattern p := (?<x> []) & [].\n",
        "pattern p := [] & (?<x> []).\n",
    ] {
        let error = compile_err(source);
        assert!(
            refuses_with(&error, DiagnosticCode::CaptureInsideIntersection),
            "{error}"
        );
    }
}

/// A capture outside the intersection is fine.
#[test]
fn a_capture_around_an_intersection_is_allowed() {
    let matcher = compile("pattern p := (?<x> [text in {a, b}] & [text in {b, c}]).\n");
    let found = matcher.find_matches(&tokens("b"));

    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].captures.len(),
        1,
        "the capture is still observable"
    );
}

/// `same_as` is likewise refused in every operand.
#[test]
fn span_equality_inside_an_intersection_is_refused() {
    let error = compile_err("pattern p := (?<x> []) ([] & same_as($x)).\n");
    assert!(
        refuses_with(&error, DiagnosticCode::SpanEqualityInsideIntersection),
        "{error}"
    );
}

/// `&` inside a lookaround body is refused; the reverse nesting is not.
#[test]
fn an_intersection_inside_a_lookaround_is_refused() {
    let error = compile_err("pattern p := [] followed_by([] & []).\n");
    let text = error.to_string();

    assert!(
        refuses_with(&error, DiagnosticCode::IntersectionInsideLookaround),
        "{text}"
    );
    assert!(
        text.contains("boundaries"),
        "the message says why, not only that: {text}"
    );
}

/// A guard is a token predicate, and an intersection is not one.
#[test]
fn an_intersection_is_not_a_transform_guard() {
    let text = compile_err(
        "transform: [{op: \"drop_edge\", edge: \"trailing\", drop: \"[upos=AUX] & [upos=ADP]\"}]\n\
         pattern p := [text=a].\n",
    )
    .to_string();
    assert!(text.contains("single token predicate"), "{text}");
}
