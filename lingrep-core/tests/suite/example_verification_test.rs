//! Documented claims are checked against what a catalog PUBLISHES, not only
//! against what it matches and publishes.
//!
//! The raw pass answers "does this definition match its example?". A reader of
//! the output has a different question: "does this example produce a record?"
//! Those answers diverge exactly where policy acts, and before this they could
//! diverge silently — a catalog could document behaviour its own published
//! output contradicted, and every check passed.

use lingrep_core::fsm::{
    CompileLimits, CompiledCatalog, ExampleFailureKind, PatternSource, compile_catalog,
    verify_examples,
};
use lingrep_core::testing::tokenize_japanese;

fn matcher(text: &str) -> CompiledCatalog {
    compile_catalog(
        PatternSource::named("claims.lx", text),
        CompileLimits::default(),
    )
    .expect("catalog compiles")
    .matcher
}

fn report(text: &str) -> lingrep_core::fsm::ExampleReport {
    verify_examples(&matcher(text), &|text| tokenize_japanese(text))
}

#[test]
fn a_renamed_definition_still_verifies_against_its_published_name() {
    // A rename is not a failure: the claim is judged against the name the
    // definition publishes under. Getting this wrong would make every
    // `output_name` catalog fail verification wholesale.
    let report = report(
        r#"
output_name: "published"
examples: ["研究"]
authored := [text=研究].
"#,
    );
    assert_eq!(report.failures, Vec::new(), "raw pass: {report:#?}");
    assert_eq!(
        report.post_policy_failures,
        Vec::new(),
        "post-policy pass: {report:#?}"
    );
    assert_eq!(report.checked, 1);
    assert_eq!(report.post_policy_checked, 1);
}

#[test]
fn an_example_whose_record_policy_suppresses_fails_post_policy_only() {
    // An `admit` against the raw
    // sentence end accepts only sentence-final records; the example ends
    // mid-sentence, so the definition matches it and publishes nothing for it.
    // The raw pass cannot see that.
    let report = report(
        r#"
examples: ["研究が進む"]
mid := [text=研究].

policy
  admit @mid terminal: {exact: true, anchor: sentence_end}.
end
"#,
    );
    assert_eq!(
        report.failures,
        Vec::new(),
        "the definition does match its example: {report:#?}"
    );
    assert_eq!(report.post_policy_failures.len(), 1, "{report:#?}");
    assert_eq!(report.post_policy_failures[0].definition, "mid");
    assert_eq!(
        report.post_policy_failures[0].kind,
        ExampleFailureKind::ExpectedMatch
    );
}

#[test]
fn a_counterexample_that_policy_suppresses_passes_post_policy() {
    // The mirror: a counterexample claims the definition publishes nothing for
    // the text. Policy suppressing the record satisfies that claim, so the
    // post-policy pass must not report it.
    let report = report(
        r#"
counterexamples: ["研究が進む"]
mid := [text=研究].

policy
  admit @mid terminal: {exact: true, anchor: sentence_end}.
end
"#,
    );
    assert_eq!(report.post_policy_failures, Vec::new(), "{report:#?}");
    assert_eq!(report.post_policy_checked, 1);
    // Raw disagrees — the definition does match — which is exactly the
    // divergence the two passes exist to expose.
    assert_eq!(report.failures.len(), 1, "{report:#?}");
    assert_eq!(report.failures[0].kind, ExampleFailureKind::UnexpectedMatch);
}

#[test]
fn a_catalog_with_no_policy_runs_no_second_pass() {
    // Not merely "no failures": running a pass that cannot disagree would
    // report coverage that means nothing, and a vacuous count is what this
    // module's `checked` field exists to prevent.
    let report = report(
        r#"
examples: ["研究"]
plain := [text=研究].
"#,
    );
    assert_eq!(report.checked, 1);
    assert_eq!(report.post_policy_checked, 0);
    assert_eq!(report.post_policy_failures, Vec::new());
}

#[test]
fn a_malformed_claim_is_reported_once_not_twice() {
    // The raw pass owns malformed values. Reporting them again post-policy
    // would double-count one broken claim and overstate the failure list.
    let report = report(
        r#"
output_name: "published"
examples: "not a list"
broken := [text=研究].
"#,
    );
    assert_eq!(report.failures.len(), 1, "{report:#?}");
    assert_eq!(report.failures[0].kind, ExampleFailureKind::Malformed);
    assert_eq!(report.post_policy_failures, Vec::new(), "{report:#?}");
}
