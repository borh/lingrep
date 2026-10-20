// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic)]
//! The opt-in sentence explanation preserves raw candidate identity across
//! result-policy publication.

use lingrep_core::fsm::{CompileLimits, PatternSource, compile_catalog};
use lingrep_core::testing::token;

#[test]
fn explanation_pairs_each_visible_raw_match_with_its_published_result() {
    let output = compile_catalog(
        PatternSource::named("test.lx", "pattern p := [text=猫]."),
        CompileLimits::default(),
    )
    .expect("catalog compiles");

    let explanation = output
        .matcher
        .explain_sentence(&[token("猫")])
        .expect("policy-free explanation succeeds");

    assert_eq!(explanation.candidates.len(), 1);
    assert_eq!(explanation.candidates[0].raw.pattern, "p");
    assert_eq!(
        explanation.candidates[0]
            .published
            .as_ref()
            .expect("policy-free candidate publishes")
            .pattern,
        "p"
    );
}

#[test]
fn explanation_hides_policy_evidence_and_marks_suppressed_candidates() {
    let output = compile_catalog(
        PatternSource::named(
            "test.lx",
            "short := [text=猫].\n\
             fragment tail := [text=犬].\n\
             policy\n  \
             exclude @short with: @tail, relation: after, evidence: raw.\n\
             end\n",
        ),
        CompileLimits::default(),
    )
    .expect("catalog compiles");

    let explanation = output
        .matcher
        .explain_sentence(&[token("猫"), token("犬")])
        .expect("policy explanation succeeds");

    assert!(
        explanation
            .candidates
            .iter()
            .all(|candidate| !candidate.raw.pattern.starts_with("__policy__"))
    );
    assert!(
        explanation
            .candidates
            .iter()
            .any(|candidate| { candidate.raw.pattern == "short" && candidate.published.is_none() })
    );
}
