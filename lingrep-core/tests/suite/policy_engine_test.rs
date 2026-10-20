//! Semantics of the sentence-local result-policy engine.
//!
//! One test per clause of the evaluation order, over hand-built policies
//! and hand-built canonical records. Building the records directly is what
//! makes the overlap and ordering cases stateable at all: they need spans that
//! touch at exactly one token, which is fiddly to provoke through a tokenizer
//! and unreadable afterwards.
//!
//! Every assertion compares complete `Match` values — pattern, span and
//! captures — because the engine's whole job is to change those fields, and a
//! count-only assertion cannot tell a relabel from a suppression.
//!
//! The derived sentence-anchor pre-pass is tested here too, at the
//! bottom of the file. It runs before the engine,
//! over raw tokens — but the positions it produces are the engine's input, and
//! the tests that consume them are these. Those tests compile real catalogs
//! through `policy_helpers` rather than hand-building a policy, because an
//! anchor position is computed from a compiled `exclude` fragment and there is
//! nothing to hand-build.

use compact_str::CompactString;
use lingrep_core::fsm::{
    AnchorError, CatalogPolicy, CatalogPolicyBuilder, Match, PatternPolicy, PolicyStats, Terminal,
    apply_policy, strip_hidden,
};
use lingrep_core::testing::tokenize_japanese;
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfoTrait};

use crate::policy_helpers::{
    PUNCT_ANCHOR, catalog_policy_for, matcher_for, no_anchors, positions, punct_anchor_matcher,
    punct_anchor_with_distance, record, sym_anchor_matcher, tokens_with_orth_and_pos,
    tokens_with_pos, unanchored_matcher,
};

/// A token carrying no attributes.
///
/// The engine takes the sentence rather than its length, because a transform
/// reads tokens. None of the policy passes below do — admission, exclusion,
/// priority groups and `trim_*` are all decided from spans — so these tests
/// supply the length and nothing else, which is exactly as much as they were
/// ever asserting about the sentence.
struct Blank;

impl TokenInfoTrait for Blank {
    fn get(&self, _: TokenAttributeIndex) -> Option<&CompactString> {
        None
    }
}

fn sentence(length: usize) -> Vec<Blank> {
    (0..length).map(|_| Blank).collect()
}

/// A catalog policy from `(definition, policy)` pairs, in declaration order.
fn policy(entries: &[(&str, PatternPolicy)]) -> CatalogPolicy {
    let mut builder = CatalogPolicyBuilder::default();
    for (name, entry) in entries {
        builder.insert_entry(CompactString::new(*name), entry.clone());
    }
    builder.build()
}

/// Registers `fragment` as having a synthesized emitter, as the compiler does.
fn with_hidden(mut catalog: CatalogPolicy, fragment: &str) -> CatalogPolicy {
    catalog.hidden.insert(
        CompactString::new(format!("__policy__{fragment}")),
        CompactString::new(fragment),
    );
    catalog
}

fn apply(
    catalog: &CatalogPolicy,
    matches: &[Match],
    sentence_len: usize,
) -> (Vec<Match>, PolicyStats) {
    let mut stats = PolicyStats::default();
    let published = apply_policy(
        catalog,
        matches,
        &sentence(sentence_len),
        &no_anchors(),
        &mut stats,
    )
    .expect("no fixture here names an anchor");
    (published, stats)
}

#[test]
fn output_name_relabels_the_published_record() {
    let catalog = policy(&[(
        "internal_name",
        PatternPolicy {
            output_name: Some(CompactString::new("よう")),
            ..PatternPolicy::default()
        },
    )]);
    let matches = [record("internal_name", 0, 2)];
    let (published, _) = apply(&catalog, &matches, 2);
    assert_eq!(published, vec![record("よう", 0, 2)]);
}

#[test]
fn synthesized_records_are_never_published() {
    let catalog = with_hidden(CatalogPolicy::default(), "copula");
    let matches = [record("real", 0, 2), record("__policy__copula", 2, 4)];

    let (published, _) = apply(&catalog, &matches, 4);
    assert_eq!(published, vec![record("real", 0, 2)]);

    // And the same under raw mode, which applies nothing else.
    assert_eq!(strip_hidden(&catalog, &matches), vec![record("real", 0, 2)]);
}

#[test]
fn raw_mode_over_a_catalog_that_synthesizes_nothing_is_the_identity() {
    let catalog = policy(&[(
        "renamed",
        PatternPolicy {
            output_name: Some(CompactString::new("published")),
            ..PatternPolicy::default()
        },
    )]);
    let matches = [record("renamed", 0, 2), record("other", 2, 4)];
    assert_eq!(strip_hidden(&catalog, &matches), matches.to_vec());
}

#[test]
fn an_empty_policy_publishes_the_canonical_stream_unchanged() {
    let catalog = CatalogPolicy::default();
    let matches = [record("a", 0, 2), record("b", 1, 3)];
    let (published, stats) = apply(&catalog, &matches, 3);
    assert_eq!(published, matches.to_vec());
    assert_eq!(stats, PolicyStats::default());
}

/// Punctuation must not define the sentence end.
#[test]
fn an_exact_terminal_admits_a_record_ending_at_the_anchor() {
    // Four tokens: three of content, then the period. The record covers the
    // content and stops before the period, which is what a sentence-final
    // expression does, and the anchor is that same boundary.
    let matches = vec![record("toiu", 1, 3)];
    let mut stats = PolicyStats::default();

    let published = apply_policy(
        &catalog_policy_for(PUNCT_ANCHOR),
        &matches,
        &sentence(4),
        &positions(&[("propositional_end", 3)]),
        &mut stats,
    )
    .expect("the positions carry this catalog's digest");

    assert_eq!(published, matches, "the record ends at the anchor");
}

// ---------------------------------------------------------------------------
// Derived sentence anchors: the per-sentence pre-pass.
// ---------------------------------------------------------------------------

/// `PUNCT_ANCHOR` excludes a trailing `[upos=PUNCT]+` run, so the anchor is the
/// boundary before that run.
#[test]
fn anchor_positions_land_on_the_last_non_punctuation_token() {
    let matcher = punct_anchor_matcher();
    let tokens = tokens_with_pos(&["NOUN", "NOUN", "PUNCT"]);
    let positions = matcher.anchor_positions(&tokens);
    assert_eq!(
        positions
            .get("propositional_end", matcher.anchor_digest())
            .unwrap(),
        2
    );
}

/// No trailing apparatus means no boundary-ending match, and the anchor lands
/// on the raw sentence end.
///
/// This is the evaluator's fallback: an unpunctuated sentence's propositional
/// end is its raw end. It is not
/// the forbidden fallback — a missing *entry* is still an error, which
/// [`an_absent_anchor_is_an_error_not_a_raw_end_fallback`] pins.
#[test]
fn an_anchor_with_no_apparatus_falls_back_to_the_raw_end() {
    let matcher = punct_anchor_matcher();
    let tokens = tokens_with_pos(&["NOUN", "NOUN"]);
    let positions = matcher.anchor_positions(&tokens);
    assert_eq!(
        positions
            .get("propositional_end", matcher.anchor_digest())
            .unwrap(),
        2
    );
}

/// The earliest qualifying boundary, not the latest: `[upos=PUNCT]+` matches
/// `1..2` as well as `0..2`, and the anchor is `0`.
#[test]
fn an_all_punctuation_sentence_anchors_at_zero() {
    let matcher = punct_anchor_matcher();
    let tokens = tokens_with_pos(&["PUNCT", "PUNCT"]);
    let positions = matcher.anchor_positions(&tokens);
    assert_eq!(
        positions
            .get("propositional_end", matcher.anchor_digest())
            .unwrap(),
        0
    );
}

/// Positions and expectation must come from the same declared anchor set.
///
/// `sym_anchor_matcher` declares an anchor of the same *name* over a different
/// `exclude`, so the position means something else. Silently answering would be
/// the raw sentence-end fallback this API forbids.
#[test]
fn positions_from_a_structurally_different_catalog_are_refused() {
    let punct = punct_anchor_matcher();
    let other = sym_anchor_matcher();
    let tokens = tokens_with_pos(&["NOUN", "PUNCT"]);
    let positions = punct.anchor_positions(&tokens);
    assert_eq!(
        positions.get("propositional_end", other.anchor_digest()),
        Err(AnchorError::CatalogMismatch)
    );
}

/// The digest is structural, so two compilations of one source interoperate.
///
/// This is what makes `catalog_policy_for` and `matcher_for` safe as separate
/// compilations.
///
/// Asserts the position, not just `is_ok()`: a cross-compilation read has to
/// return the *right* number, and `1` is what this sentence determines.
#[test]
fn two_structurally_identical_catalogs_are_compatible() {
    let one = punct_anchor_matcher();
    let two = punct_anchor_matcher();
    let tokens = tokens_with_pos(&["NOUN", "PUNCT"]);
    assert_eq!(
        one.anchor_positions(&tokens)
            .get("propositional_end", two.anchor_digest()),
        Ok(1)
    );
}

#[test]
fn a_cloned_matcher_keeps_its_anchor_digest() {
    let matcher = punct_anchor_matcher();
    assert_eq!(matcher.clone().anchor_digest(), matcher.anchor_digest());
}

/// An undeclared name is an error, never the raw sentence end.
///
/// The whole reason `AnchorPositions` is opaque. A map would have let a caller
/// write `positions.get(name).unwrap_or(tokens.len())` and reintroduce raw-end
/// admission under an anchored catalog.
///
/// Compares the whole error rather than `matches!(.., Unknown(_))`, so that a
/// variant carrying the wrong name — the anchor it *did* find, say — is caught.
#[test]
fn an_absent_anchor_is_an_error_not_a_raw_end_fallback() {
    let matcher = punct_anchor_matcher();
    let tokens = tokens_with_pos(&["NOUN", "PUNCT"]);
    let positions = matcher.anchor_positions(&tokens);
    assert_eq!(
        positions.get("nowhere", matcher.anchor_digest()),
        Err(AnchorError::Unknown("nowhere".into()))
    );
}

/// Each anchor's position comes from *its own* `exclude`.
///
/// Added for a mutation the seven tests above cannot see. Every one of them
/// declares a single anchor, so a `positions` that paired names with programs
/// wrongly — or computed one position and reused it for every name — answers
/// identically and the suite stays green. Two anchors whose positions differ in
/// the same sentence is the smallest input that separates them.
#[test]
fn each_anchor_reports_the_position_of_its_own_exclude() {
    let catalog = r#"
anchor: [{name: "propositional_end", exclude: "TRAILING_PUNCT"}, {name: "two_back", exclude: "LAST_TWO"}]
max_nesting: {depth: 0, witness: ["これは重要である|。"], divergence: ["これは重要である|（田中）。"]}
fragment TRAILING_PUNCT := [upos=PUNCT]+.

max_nesting: {depth: 0, witness: ["これは重要で|ある。"], divergence: ["これは重要である|（田中）。"]}
fragment LAST_TWO := [] [].

pattern one := [text=x].

pattern two := [text=y].

policy
  admit @one terminal: {exact: true, anchor: propositional_end}.
  admit @two terminal: {exact: true, anchor: two_back}.
end
"#;
    let matcher = matcher_for(catalog);
    // Four tokens: the trailing punctuation run is one long, so the two anchors
    // land two apart and neither equals the raw end.
    let tokens = tokens_with_pos(&["NOUN", "NOUN", "NOUN", "PUNCT"]);
    let positions = matcher.anchor_positions(&tokens);

    assert_eq!(
        positions
            .get("propositional_end", matcher.anchor_digest())
            .unwrap(),
        3
    );
    assert_eq!(
        positions.get("two_back", matcher.anchor_digest()).unwrap(),
        2
    );
}

/// A catalog declaring no anchor still yields a usable, compatible answer.
///
/// It takes the early return in `positions`, which builds no session. This is
/// the path for anchor-free catalogs that still declare other policy.
///
/// The distinction being pinned is `Unknown` versus `CatalogMismatch`: the empty
/// set must carry this catalog's own digest, so a reader presenting that digest
/// is told the anchor does not exist rather than that it asked the wrong
/// catalog.
#[test]
fn an_anchor_free_catalog_yields_an_empty_but_compatible_position_set() {
    let matcher = unanchored_matcher();
    let tokens = tokens_with_pos(&["NOUN", "PUNCT"]);
    let positions = matcher.anchor_positions(&tokens);
    assert_eq!(
        positions.get("propositional_end", matcher.anchor_digest()),
        Err(AnchorError::Unknown("propositional_end".into()))
    );
}

/// The recorded depth-2 transcript, re-derived by the engine.
///
/// The three anchors were derived by hand. Reading the committed catalog
/// (`lingrep-core/tests/fixtures/catalogs/apparatus-depth2.lx`) rather than
/// inlining it keeps the two from drifting apart.
///
/// Dictionary-backed and deliberately not `#[ignore]`d, for the reason
/// `policy_helpers::verify_claims` gives: `nix develop` sets
/// `LINGREP_DICTIONARY_PATH`.
#[test]
fn the_recorded_depth_two_transcript_is_reproduced() {
    let source =
        std::fs::read_to_string("../lingrep-core/tests/fixtures/catalogs/apparatus-depth2.lx")
            .expect("the committed depth-2 recognizer");
    let matcher = matcher_for(&source);
    for (sentence, expected) in [
        ("これは重要である（田中 2020）。", 5usize),
        ("これは重要である（田中 2020（改訂））。", 5),
        ("これは重要である（田中 2020（改訂（第2版）））。", 17),
    ] {
        let tokens = tokenize_japanese(sentence);
        let derived = matcher
            .anchor_positions(&tokens)
            .get("propositional_end", matcher.anchor_digest())
            .expect("the recognizer declares this anchor");
        assert_eq!(derived, expected, "{sentence}");
    }
}

// ---------------------------------------------------------------------------
// Anchored admission: the anchor deciding what is published.
// ---------------------------------------------------------------------------

/// The record the raw anchor refused: `という` before the sentence-final period.
///
/// End to end from a sentence, where
/// [`an_exact_terminal_admits_a_record_ending_at_the_anchor`] above asserts the
/// same repair from a stated anchor position — so this one is also what shows
/// the recognizer derives that position rather than it being supplied.
#[test]
fn an_anchored_exact_terminal_admits_the_record_before_the_period() {
    let matcher = punct_anchor_matcher();
    let tokens = tokens_with_orth_and_pos(&[("研究", "NOUN"), ("という", "NOUN"), ("。", "PUNCT")]);
    let published = matcher
        .find_published_matches(&tokens)
        .expect("the catalog's own positions resolve");
    assert_eq!(published.len(), 1, "got {published:?}");
    assert_eq!((published[0].start, published[0].end), (1, 2));
}

/// A `within` admission counts back from the anchor, not the raw token count.
#[test]
fn an_anchored_within_measures_from_the_anchor_not_the_raw_end() {
    let anchors = positions(&[("propositional_end", 3)]);

    let admitted = apply_policy(
        &catalog_policy_for(&punct_anchor_with_distance(Terminal::Within(1))),
        &[record("toiu", 0, 2)],
        &sentence(4),
        &anchors,
        &mut PolicyStats::default(),
    )
    .expect("the positions carry this catalog's digest");
    assert_eq!(
        admitted.len(),
        1,
        "one token short of the anchor is within 1"
    );

    let refused = apply_policy(
        &catalog_policy_for(&punct_anchor_with_distance(Terminal::Within(0))),
        &[record("toiu", 0, 2)],
        &sentence(4),
        &anchors,
        &mut PolicyStats::default(),
    )
    .expect("the positions carry this catalog's digest");
    assert!(refused.is_empty(), "Within(0) refuses one token short");
}

/// One-sided: past the anchor is refused at every distance.
///
/// Unreachable before this task — a match could not end past its own sentence,
/// and the raw sentence end was the only anchor. A derived anchor can precede a
/// valid match's end, so the branch is live, data-dependent behaviour from here.
/// Three distances, because `Exact` and `Within` refuse it by different
/// expressions and a wide `Within` is the one that would admit it if the
/// comparison were a magnitude rather than a direction.
#[test]
fn a_record_reaching_past_the_anchor_is_refused() {
    let anchors = positions(&[("propositional_end", 2)]);
    for distance in [Terminal::Exact, Terminal::Within(0), Terminal::Within(8)] {
        let published = apply_policy(
            &catalog_policy_for(&punct_anchor_with_distance(distance)),
            &[record("toiu", 0, 4)],
            &sentence(4),
            &anchors,
            &mut PolicyStats::default(),
        )
        .expect("the positions carry this catalog's digest");
        assert!(
            published.is_empty(),
            "{distance:?} must refuse a record ending past the anchor"
        );
    }
}

/// With anchor-based admission, declaring policy changes what
/// is *published* and never what is matched.
#[test]
fn find_matches_is_unchanged_by_an_anchor_declaration() {
    let tokens = tokens_with_orth_and_pos(&[("研究", "NOUN"), ("という", "NOUN"), ("。", "PUNCT")]);
    assert_eq!(
        punct_anchor_matcher().find_matches(&tokens),
        unanchored_matcher().find_matches(&tokens)
    );
}

/// Each definition is judged against the anchor *it* names.
///
/// Added for a mutation the tests above cannot see: every one of them declares
/// a single anchor, so a resolution loop that mispaired statements with
/// positions — or resolved one anchor and reused it for every `admit` — answers
/// identically and the suite stays green. Two statements naming two anchors
/// that land two tokens apart is the smallest input that separates them, and
/// both records are admitted only under the correct pairing: swapping the
/// positions refuses both.
///
/// `each_anchor_reports_the_position_of_its_own_exclude` pins the same property
/// one layer down, in the evaluator. This pins it in the engine, which is a
/// different mapping — statement to anchor rather than anchor to program.
#[test]
fn each_definition_measures_against_the_anchor_it_names() {
    let catalog = r#"
anchor: [{name: "propositional_end", exclude: "TRAILING_PUNCT"}, {name: "two_back", exclude: "LAST_TWO"}]
max_nesting: {depth: 0, witness: ["これは重要である|。"], divergence: ["これは重要である|（田中）。"]}
fragment TRAILING_PUNCT := [upos=PUNCT]+.

max_nesting: {depth: 0, witness: ["これは重要で|ある。"], divergence: ["これは重要である|（田中）。"]}
fragment LAST_TWO := [] [].

pattern at_end := [text=x].
pattern two_short := [text=y].

policy
  admit @at_end terminal: {exact: true, anchor: propositional_end}.
  admit @two_short terminal: {exact: true, anchor: two_back}.
end
"#;
    let matcher = matcher_for(catalog);
    // Four tokens with a one-token punctuation run: `propositional_end` is 3
    // and `two_back` is 2, so each record ends at exactly one of them.
    let tokens =
        tokens_with_orth_and_pos(&[("a", "NOUN"), ("y", "NOUN"), ("x", "NOUN"), ("。", "PUNCT")]);

    let published = matcher
        .find_published_matches(&tokens)
        .expect("the catalog's own positions resolve");

    assert_eq!(
        published,
        vec![record("two_short", 1, 2), record("at_end", 2, 3)],
        "each record is admitted by its own anchor and no other"
    );
}
