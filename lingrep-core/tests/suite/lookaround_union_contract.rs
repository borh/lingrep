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
//! The identity behind the lookaround alternation-union split.
//!
//! A lookaround body that is a top-level alternation compiles to one program
//! per branch plus a union program whose row is the bitwise `or` of theirs,
//! rather than to one automaton swept over the sentence. What guards that
//! today is an accounting pin, a fingerprint snapshot, and byte-identical
//! output across the tracked catalogs — broad evidence of nothing observably
//! changing, and not a statement of the identity itself.
//!
//! Every test here states the identity in lingrep patterns rather than in the compiler's
//! terms, so it stays meaningful if the lowering is replaced:
//!
//! - a **positive** lookaround over `A | B` matches where `followed_by(A)` or
//!   `followed_by(B)` matches — the union of the branch spellings;
//! - a **negative** one matches where *both* negated branches match — the
//!   intersection, because `not (A or B)` is `(not A) and (not B)` and the
//!   union is negated once rather than per branch.
//!
//! Complete [`Match`] values are compared, with only the pattern name erased,
//! so captures and lexical identity participate in the comparison.
//!
//! What this cannot reach: the union program's automaton is never swept — the
//! matcher reads the members' rows instead — so the polarity of the
//! occurrences the compiler synthesizes for it is dead data, and mutating it
//! leaves every test here green. Mutating the row union itself fails nine of
//! the eleven, which is the part that computes anything.

use lingrep_core::fsm::{CompileLimits, Match, PatternSource, compile_catalog};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

fn token(orth: &str) -> TokenInfo {
    token_pos(orth, "X")
}

fn token_pos(orth: &str, pos1: &str) -> TokenInfo {
    let mut token = TokenInfo::default();
    token.set(TokenAttributeIndex::Orth, orth.into());
    token.set(TokenAttributeIndex::Pos1, pos1.into());
    token
}

fn tokens(orths: &[&str]) -> Vec<TokenInfo> {
    orths.iter().copied().map(token).collect()
}

/// Every match of `name` in `source`, with the pattern name erased so two
/// spellings of one definition compare equal, ordered by span.
fn matches_of(source: &str, name: &str, tokens: &[TokenInfo]) -> Vec<Match> {
    let matcher = compile_catalog(
        PatternSource::named("lookaround-union.lx", source),
        CompileLimits::default(),
    )
    .unwrap_or_else(|error| panic!("{source}\nmust compile, but: {error:?}"))
    .matcher;
    let mut found: Vec<Match> = matcher
        .find_matches(tokens)
        .into_iter()
        .filter(|matched| matched.pattern.as_str() == name)
        .map(|mut matched| {
            matched.pattern = "_".into();
            matched
        })
        .collect();
    found.sort_by_key(|matched| (matched.start, matched.end));
    found
}

fn spans(matches: &[Match]) -> Vec<(usize, usize)> {
    matches
        .iter()
        .map(|matched| (matched.start, matched.end))
        .collect()
}

fn union(left: &[Match], right: &[Match]) -> Vec<Match> {
    let mut out = left.to_vec();
    for matched in right {
        if !out.contains(matched) {
            out.push(matched.clone());
        }
    }
    out.sort_by_key(|matched| (matched.start, matched.end));
    out
}

fn intersection(left: &[Match], right: &[Match]) -> Vec<Match> {
    let mut out: Vec<Match> = left
        .iter()
        .filter(|matched| right.contains(matched))
        .cloned()
        .collect();
    out.sort_by_key(|matched| (matched.start, matched.end));
    out
}

/// `x a x b x c` — every `x` is followed by a different branch, and the last
/// by neither, so a union that dropped or over-collected a branch moves.
fn mixed_forward() -> Vec<TokenInfo> {
    tokens(&["x", "a", "x", "b", "x", "c"])
}

/// The mirror image, for the reverse direction.
fn mixed_reverse() -> Vec<TokenInfo> {
    tokens(&["a", "x", "b", "x", "c", "x"])
}

#[test]
fn a_positive_alternation_body_matches_the_union_of_its_branches() {
    let sentence = mixed_forward();
    let alternation = matches_of(
        "pattern p := [text=x] followed_by([text=a] | [text=b]).",
        "p",
        &sentence,
    );
    let first = matches_of(
        "pattern p := [text=x] followed_by([text=a]).",
        "p",
        &sentence,
    );
    let second = matches_of(
        "pattern p := [text=x] followed_by([text=b]).",
        "p",
        &sentence,
    );

    assert_eq!(alternation, union(&first, &second));
    assert_eq!(
        spans(&alternation),
        vec![(0, 1), (2, 3)],
        "the third `x` is followed by `c` and must not match either branch"
    );
}

#[test]
fn a_positive_alternation_body_unions_in_the_reverse_direction_too() {
    let sentence = mixed_reverse();
    let alternation = matches_of(
        "pattern p := preceded_by([text=a] | [text=b]) [text=x].",
        "p",
        &sentence,
    );
    let first = matches_of(
        "pattern p := preceded_by([text=a]) [text=x].",
        "p",
        &sentence,
    );
    let second = matches_of(
        "pattern p := preceded_by([text=b]) [text=x].",
        "p",
        &sentence,
    );

    assert_eq!(alternation, union(&first, &second));
    assert_eq!(spans(&alternation), vec![(1, 2), (3, 4)],);
}

/// The polarity case the split is easiest to get wrong: negating each member
/// instead of the union computes `(not A) or (not B)`, which on this sentence
/// admits every `x` rather than one.
#[test]
fn a_negative_alternation_body_matches_the_intersection_of_the_negated_branches() {
    let sentence = mixed_forward();
    let alternation = matches_of(
        "pattern p := [text=x] not_followed_by([text=a] | [text=b]).",
        "p",
        &sentence,
    );
    let first = matches_of(
        "pattern p := [text=x] not_followed_by([text=a]).",
        "p",
        &sentence,
    );
    let second = matches_of(
        "pattern p := [text=x] not_followed_by([text=b]).",
        "p",
        &sentence,
    );

    assert_eq!(alternation, intersection(&first, &second));
    assert_eq!(
        spans(&alternation),
        vec![(4, 5)],
        "only the `x` followed by `c` escapes both branches; `(not A) or \
         (not B)` would admit all three"
    );
    assert_eq!(
        spans(&union(&first, &second)),
        vec![(0, 1), (2, 3), (4, 5)],
        "the wrong reading is observably different on this sentence, which is \
         what makes the assertion above worth making"
    );
}

#[test]
fn a_negative_alternation_body_intersects_in_the_reverse_direction_too() {
    let sentence = mixed_reverse();
    let alternation = matches_of(
        "pattern p := not_preceded_by([text=a] | [text=b]) [text=x].",
        "p",
        &sentence,
    );
    let first = matches_of(
        "pattern p := not_preceded_by([text=a]) [text=x].",
        "p",
        &sentence,
    );
    let second = matches_of(
        "pattern p := not_preceded_by([text=b]) [text=x].",
        "p",
        &sentence,
    );

    assert_eq!(alternation, intersection(&first, &second));
    assert_eq!(spans(&alternation), vec![(5, 6)],);
}

/// Branches reach the union through `BoundExpr::Definition`, which carries the
/// span of the *site* that wrote it and derives `PartialEq`. Interning by the
/// reference rather than by the definition's own expression makes `@A` at two
/// sites two programs; that is not a correctness bug, but it is the whole of
/// what the split exists to avoid, and it cost 43% when it shipped.
#[test]
fn referenced_branches_match_what_inline_branches_match() {
    let sentence = mixed_forward();
    let referenced = matches_of(
        concat!(
            "fragment A := [text=a].\n",
            "fragment B := [text=b].\n",
            "pattern p := [text=x] followed_by(@A | @B).",
        ),
        "p",
        &sentence,
    );
    let inline = matches_of(
        "pattern p := [text=x] followed_by([text=a] | [text=b]).",
        "p",
        &sentence,
    );

    assert_eq!(referenced, inline);
    assert!(!referenced.is_empty(), "the comparison must not be vacuous");
}

/// `A | A` interns one member and emits two `Fact` edges into one accept
/// state. A row is a set of boundaries, so the `or` is idempotent and the
/// result must equal the single branch — including under negation, where a
/// duplicate that was miscounted would flip the answer.
#[test]
fn duplicate_branches_are_idempotent() {
    let sentence = mixed_forward();
    for (duplicated, single) in [
        (
            "pattern p := [text=x] followed_by([text=a] | [text=a]).",
            "pattern p := [text=x] followed_by([text=a]).",
        ),
        (
            "pattern p := [text=x] not_followed_by([text=a] | [text=a]).",
            "pattern p := [text=x] not_followed_by([text=a]).",
        ),
    ] {
        assert_eq!(
            matches_of(duplicated, "p", &sentence),
            matches_of(single, "p", &sentence),
            "{duplicated}",
        );
    }
}

/// Branches whose match sets intersect without either containing the other.
/// `a/P` satisfies both, `a/Q` only the first, `d/P` only the second — so the
/// union is strictly larger than either branch, and a union that stopped at
/// the first satisfied branch or that or-ed the wrong rows lands on one of
/// them rather than between.
#[test]
fn overlapping_branches_union_rather_than_double_count() {
    let sentence = vec![
        token_pos("x", "X"),
        token_pos("a", "P"),
        token_pos("x", "X"),
        token_pos("a", "Q"),
        token_pos("x", "X"),
        token_pos("d", "P"),
        token_pos("x", "X"),
        token_pos("e", "Q"),
    ];
    let alternation = matches_of(
        "pattern p := [text=x] followed_by([text=a] | [unidic.pos1=P]).",
        "p",
        &sentence,
    );
    let by_orth = matches_of(
        "pattern p := [text=x] followed_by([text=a]).",
        "p",
        &sentence,
    );
    let by_pos = matches_of(
        "pattern p := [text=x] followed_by([unidic.pos1=P]).",
        "p",
        &sentence,
    );

    assert_eq!(alternation, union(&by_orth, &by_pos));
    assert_eq!(
        spans(&by_orth),
        vec![(0, 1), (2, 3)],
        "the `a`s, whichever pos1 they carry",
    );
    assert_eq!(
        spans(&by_pos),
        vec![(0, 1), (4, 5)],
        "the `P`s, whichever orth they carry",
    );
    assert_eq!(
        spans(&alternation),
        vec![(0, 1), (2, 3), (4, 5)],
        "strictly larger than either branch; the `x` before `e/Q` matches \
         neither",
    );
}

/// A union whose member is itself a union. Reached through a fragment whose
/// body is an alternation, which makes the nesting independent of whether the
/// parser flattens `a | (b | c)`.
///
/// This is the shape that can reach the runtime's
/// `expect("union members are materialized as dependencies")`: the inner
/// union must be materialized before the outer one reads its row, which is
/// `ensure`'s recursion through `dependencies` and not an ordering anyone
/// wrote down. No tracked catalog contains one.
#[test]
fn a_nested_union_materializes_its_inner_union_first() {
    let sentence = tokens(&["x", "a", "x", "b", "x", "c", "x", "d"]);
    let nested = matches_of(
        concat!(
            "fragment BC := [text=b] | [text=c].\n",
            "pattern p := [text=x] followed_by([text=a] | @BC).",
        ),
        "p",
        &sentence,
    );
    let flat = matches_of(
        "pattern p := [text=x] followed_by([text=a] | [text=b] | [text=c]).",
        "p",
        &sentence,
    );

    assert_eq!(nested, flat);
    assert_eq!(
        spans(&nested),
        vec![(0, 1), (2, 3), (4, 5)],
        "the `x` followed by `d` matches no branch at any nesting depth",
    );
}

#[test]
fn a_nested_union_negates_once_at_the_outermost_lookaround() {
    let sentence = tokens(&["x", "a", "x", "b", "x", "c", "x", "d"]);
    let nested = matches_of(
        concat!(
            "fragment BC := [text=b] | [text=c].\n",
            "pattern p := [text=x] not_followed_by([text=a] | @BC).",
        ),
        "p",
        &sentence,
    );

    assert_eq!(
        spans(&nested),
        vec![(6, 7)],
        "only the `x` before `d` escapes all three branches",
    );
}

/// A boundary row is `tokens.len() + 1` bits wide and `union_with` asserts the
/// two rows agree on that length. One and zero tokens are where an off-by-one
/// in that width would show, and where a `debug_assert` would be all that
/// stood behind the loop in a release build.
#[test]
fn degenerate_sentences_do_not_trip_the_union_row() {
    let positive = "pattern p := [text=x] followed_by([text=a] | [text=b]).";
    let negative = "pattern p := [text=x] not_followed_by([text=a] | [text=b]).";

    assert!(
        matches_of(positive, "p", &[]).is_empty(),
        "an empty sentence has no token for the pattern to consume",
    );
    assert!(matches_of(negative, "p", &[]).is_empty());

    let single = tokens(&["x"]);
    assert!(
        matches_of(positive, "p", &single).is_empty(),
        "nothing follows the only token, so no branch holds",
    );
    assert_eq!(
        spans(&matches_of(negative, "p", &single)),
        vec![(0, 1)],
        "and therefore the negation holds at the sentence end",
    );

    let pair = tokens(&["x", "b"]);
    assert_eq!(spans(&matches_of(positive, "p", &pair)), vec![(0, 1)],);
    assert!(matches_of(negative, "p", &pair).is_empty());
}

/// Programs are interned across the whole catalog, so two lookarounds over the
/// same branches share their members and a third over one of them shares that.
/// Sharing is the point of the split; it must also be inert. Rewriting the
/// same catalog so that nothing is shared may not change a single match.
#[test]
fn sharing_branches_between_lookarounds_changes_no_match() {
    let sentence = tokens(&["x", "a", "y", "b", "x", "c", "y", "a"]);
    let shared = matches_of(
        concat!(
            "fragment A := [text=a].\n",
            "fragment B := [text=b].\n",
            "pattern p := [text=x] followed_by(@A | @B).\n",
            "pattern q := [text=y] followed_by(@A | @B).\n",
            "pattern r := [text=y] followed_by(@A).",
        ),
        "p",
        &sentence,
    );
    let distinct = matches_of(
        "pattern p := [text=x] followed_by([text=a] | [text=b]).",
        "p",
        &sentence,
    );
    assert_eq!(shared, distinct);

    for (name, expected) in [("q", vec![(2, 3), (6, 7)]), ("r", vec![(6, 7)])] {
        assert_eq!(
            spans(&matches_of(
                concat!(
                    "fragment A := [text=a].\n",
                    "fragment B := [text=b].\n",
                    "pattern p := [text=x] followed_by(@A | @B).\n",
                    "pattern q := [text=y] followed_by(@A | @B).\n",
                    "pattern r := [text=y] followed_by(@A).",
                ),
                name,
                &sentence,
            )),
            expected,
            "{name}",
        );
    }
}
