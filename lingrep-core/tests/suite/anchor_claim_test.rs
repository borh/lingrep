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
//! Executing a declared anchor's `max_nesting` claim.
//!
//! The compiler's half — what shape a claim must have — is pinned in
//! `policy_compile_test`. This is the other half, and it needs a dictionary:
//! the claim says *where* a bounded recognizer lands and *how deep* the
//! sentences it lands on nest, and neither question has an answer without
//! tokens.
//!
//! A `witness` records agreement with an unbounded scanner; a `divergence`
//! records disagreement. The second is what a must-not-match test cannot
//! express — at depth N+1 the `exclude` fragment still matches, and what fails
//! is where the anchor lands.

use crate::policy_helpers::{PUNCT_ANCHOR, verify_claims};
use lingrep_core::fsm::ClaimFailureKind;

/// The witness and the divergence in the shared fixture, replaced.
///
/// `PUNCT_ANCHOR`'s claim is depth 0 over `[upos=PUNCT]+`, so both lists hold
/// exactly one entry and a replacement is unambiguous.
fn with_witness(entry: &str) -> String {
    PUNCT_ANCHOR.replace(
        r#"witness: ["これは重要である|。"]"#,
        &format!("witness: [\"{entry}\"]"),
    )
}

fn with_divergence(entry: &str) -> String {
    PUNCT_ANCHOR.replace(
        r#"divergence: ["これは重要である|（田中）。"]"#,
        &format!("divergence: [\"{entry}\"]"),
    )
}

/// The shipped fixture's claim holds, and holds non-vacuously.
///
/// `checked` is asserted for the reason `ExampleReport::checked` exists: an
/// empty failure list is the same value whether every entry was executed and
/// held or none was found, and those are opposite outcomes.
#[test]
fn a_depth_zero_nesting_claim_verifies() {
    let report = verify_claims(PUNCT_ANCHOR);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(report.checked, 2, "one witness and one divergence");
}

/// A catalog declaring no anchor has no claim, and says so rather than
/// reporting a vacuous success.
#[test]
fn a_catalog_declaring_no_anchor_checks_nothing() {
    let report = verify_claims("pattern toiu := [text=という].\n");
    assert_eq!(report.checked, 0);
    assert!(report.failures.is_empty());
}

/// The sentinel claims the anchor strips everything after `これは`. It does
/// not: `[upos=PUNCT]+`'s longest suffix match is `。` alone, at token 5.
#[test]
fn a_witness_whose_anchor_misses_the_sentinel_fails() {
    let report = verify_claims(&with_witness("これは|重要である。"));
    assert_eq!(
        report
            .failures
            .iter()
            .map(|failure| failure.kind)
            .collect::<Vec<_>>(),
        vec![ClaimFailureKind::WitnessMissedAnchor],
        "{:?}",
        report.failures
    );
}

/// A divergence that agrees records nothing.
///
/// `これは重要である（田中|）。` nests to 1, so it clears the depth
/// requirement, and its sentinel is exactly where `[upos=PUNCT]+` lands. The
/// entry therefore documents a disagreement that does not happen.
#[test]
fn a_divergence_that_actually_agrees_fails() {
    let report = verify_claims(&with_divergence("これは重要である（田中|）。"));
    assert_eq!(
        report
            .failures
            .iter()
            .map(|failure| failure.kind)
            .collect::<Vec<_>>(),
        vec![ClaimFailureKind::DivergenceAgreed],
        "{:?}",
        report.failures
    );
}

/// The number must be load-bearing. An overstated depth is caught because the
/// entries do not nest that deep.
#[test]
fn an_overstated_depth_is_refused() {
    let report = verify_claims(&PUNCT_ANCHOR.replace("depth: 0", "depth: 42"));
    assert_eq!(
        report
            .failures
            .iter()
            .map(|failure| failure.kind)
            .collect::<Vec<_>>(),
        vec![
            ClaimFailureKind::WrongNestingDepth,
            ClaimFailureKind::WrongNestingDepth
        ],
        "{:?}",
        report.failures
    );
}

/// And an understated one, because a divergence must nest to `depth + 1`.
#[test]
fn a_divergence_that_does_not_nest_one_deeper_is_refused() {
    let report = verify_claims(&with_divergence("これは重要である|（田中（改訂））。"));
    assert_eq!(
        report
            .failures
            .iter()
            .map(|failure| failure.kind)
            .collect::<Vec<_>>(),
        vec![ClaimFailureKind::WrongNestingDepth],
        "that nests to 2, not to 1: {:?}",
        report.failures
    );
}

/// Sibling groups do not nest.
///
/// `（田中）（佐藤）` opens two brackets and nests to one. A depth measured by
/// counting openers would call it 2, and every other fixture here is blind to
/// that: each has at most one bracket pair open at a time, so opener count and
/// nesting depth agree.
#[test]
fn sibling_bracket_groups_nest_to_one() {
    let report = verify_claims(&with_divergence("これは重要である|（田中）（佐藤）。"));
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(report.checked, 2);
}

/// An unmatched closer clamps at zero rather than going negative.
///
/// A bare `）` is a *different* approximation from the depth bound —
/// `docs/results/2026-08-04-anchor-nesting-claim/` records it as one this
/// schema does not express — so it must not be counted as nesting. Without the
/// clamp this entry underflows the depth counter instead of reporting 0.
#[test]
fn an_unmatched_closer_does_not_nest() {
    let report = verify_claims(&with_witness("これは重要である|）。"));
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(report.checked, 2);
}

/// A sentinel that splits a token is an authoring mistake, not a rounding case.
///
/// `これは重要であ|る。` marks a boundary inside `ある`. Rounding it to the
/// nearest token would silently accept a claim about a position the tokenizer
/// cannot express.
#[test]
fn a_sentinel_inside_a_token_is_refused() {
    let report = verify_claims(&with_witness("これは重要であ|る。"));
    assert_eq!(
        report
            .failures
            .iter()
            .map(|failure| failure.kind)
            .collect::<Vec<_>>(),
        vec![ClaimFailureKind::SentinelSplitsAToken],
        "{:?}",
        report.failures
    );
}

/// A sentinel at the very end of a sentence is the sentence-end boundary, not
/// an offset past the last token.
///
/// The claim it makes — "this recognizer strips nothing here" — is wrong for
/// `[upos=PUNCT]+`, so it fails; what matters is *how*. Reading it as a split
/// token or as out of range would report the wrong mistake, and the detail
/// naming token 6 of 6 is what shows the final boundary was resolved.
#[test]
fn a_sentinel_at_the_sentence_end_is_the_final_boundary() {
    let report = verify_claims(&with_witness("これは重要である。|"));
    let failure = report.failures.first().expect("the witness fails");
    assert_eq!(failure.kind, ClaimFailureKind::WitnessMissedAnchor);
    assert_eq!(
        failure.detail, "anchor derived at token 5, sentinel at token 6",
        "token 6 of a 6-token sentence is the final boundary; `None` or a \
         split-token report would both be the wrong mistake"
    );
}

/// The report names the entry as the author wrote it, sentinel included.
///
/// A failure quoting the sentinel-stripped sentence would send the author
/// looking for a line their catalog does not contain.
#[test]
fn a_failure_quotes_the_entry_with_its_sentinel() {
    let report = verify_claims(&with_witness("これは|重要である。"));
    let failure = report.failures.first().expect("the witness fails");
    assert_eq!(failure.entry, "これは|重要である。");
    assert_eq!(failure.anchor, "propositional_end");
    assert_eq!(
        failure.detail,
        "anchor derived at token 5, sentinel at token 2"
    );
}
