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
//! The `--where` clause language: parsing and evaluation.

use lingrep_core::corpus::{Metadata, metadata_from_json};
use lingrep_core::scope::{DocumentOutcome, ExclusionReason, ScopeClause, Verdict};
use serde_json::json;

fn document(value: serde_json::Value) -> Metadata {
    metadata_from_json(value, "test").unwrap()
}

/// Every clause in this file is metadata-only, so `evaluate_document` always
/// decides. A `RequiresSentences` here would mean a section predicate had crept
/// into a test that is not about them.
fn verdict(clause: &str, value: serde_json::Value) -> Verdict {
    match ScopeClause::parse(clause)
        .unwrap()
        .evaluate_document(&document(value))
    {
        DocumentOutcome::Decided(verdict) => verdict,
        DocumentOutcome::RequiresSentences => {
            panic!("a metadata-only clause must be decided from metadata alone")
        }
    }
}

#[test]
fn a_bare_equality_predicate_parses() {
    let clause = ScopeClause::parse("@document.subject_area = 社会学").unwrap();
    assert_eq!(clause.source(), "@document.subject_area = 社会学");
}

#[test]
fn conjunction_is_the_default_and_needs_no_parentheses() {
    ScopeClause::parse("@document.subject_area = 社会学 & @document.year >= 2010").unwrap();
}

#[test]
fn a_disjunction_without_parentheses_is_refused_with_its_position() {
    let error = ScopeClause::parse("@document.a = 1 | @document.b = 2").unwrap_err();
    assert!(
        error.message.contains("parenthes"),
        "the message must name the grouping the author has to supply: {}",
        error.message
    );
    assert_eq!(
        &"@document.a = 1 | @document.b = 2"[error.byte_range.clone()],
        "|"
    );
}

#[test]
fn a_parenthesised_disjunction_parses() {
    ScopeClause::parse("(@document.a = 1 | @document.b = 2) & @document.c = 3").unwrap();
}

#[test]
fn a_set_membership_predicate_parses_multiple_members() {
    ScopeClause::parse("@document.journal_name in {村落社会研究ジャーナル, 社会学評論}").unwrap();
}

#[test]
fn negated_forms_parse_for_every_operator_that_has_one() {
    ScopeClause::parse("@document.a != 1").unwrap();
    ScopeClause::parse("@document.a not in {1, 2}").unwrap();
    ScopeClause::parse("@document.keywords not contains 社会学").unwrap();
}

#[test]
fn there_is_no_clause_level_negation() {
    let error = ScopeClause::parse("not (@document.a = 1)").unwrap_err();
    assert!(
        error.message.contains("not"),
        "the refusal must name the construct: {}",
        error.message
    );
}

#[test]
fn a_quoted_value_may_contain_delimiters_and_spaces() {
    ScopeClause::parse(r#"@document.title = "a, b & c""#).unwrap();
}

#[test]
fn an_unparseable_ordered_comparison_literal_is_refused_at_parse_time() {
    // Authored text is checkable up front; a value that will not parse is a
    // per-document shape mismatch, and the two are not the same fact.
    let error = ScopeClause::parse("@document.year >= abc").unwrap_err();
    assert_eq!(&"@document.year >= abc"[error.byte_range.clone()], "abc");
    assert!(
        error.message.contains("decimal"),
        "the message must say what it wanted: {}",
        error.message
    );
}

#[test]
fn an_empty_clause_is_refused() {
    assert!(ScopeClause::parse("   ").is_err());
}

#[test]
fn an_empty_member_set_is_refused() {
    assert!(ScopeClause::parse("@document.a in {}").is_err());
}

#[test]
fn equality_matches_a_string_value() {
    assert_eq!(
        verdict(
            "@document.subject_area = 社会学",
            json!({"subject_area": "社会学"})
        ),
        Verdict::Admitted
    );
}

#[test]
fn an_ordered_comparison_parses_a_value_that_arrived_as_a_string() {
    // The corpus stores every numeric-looking key as a string: `year` is
    // "2013" on all 1,487 documents carrying it. Dispatching on the arrived
    // variant would answer "no" to every ordered comparison.
    assert_eq!(
        verdict("@document.year >= 2010", json!({"year": "2013"})),
        Verdict::Admitted
    );
    assert_eq!(
        verdict("@document.year >= 2010", json!({"year": 2013})),
        Verdict::Admitted
    );
}

#[test]
fn a_value_that_does_not_parse_is_a_shape_mismatch_not_a_false_predicate() {
    assert_eq!(
        verdict("@document.pages >= 10", json!({"pages": "47-56"})),
        Verdict::Excluded(ExclusionReason::ShapeMismatch)
    );
}

#[test]
fn a_missing_field_is_false_in_both_polarities() {
    assert_eq!(
        verdict("@document.pages = 47", json!({"year": "2013"})),
        Verdict::Excluded(ExclusionReason::MissingField)
    );
    assert_eq!(
        verdict("@document.pages != 47", json!({"year": "2013"})),
        Verdict::Excluded(ExclusionReason::MissingField)
    );
}

#[test]
fn containment_tests_a_list_valued_key() {
    assert_eq!(
        verdict(
            "@document.keywords contains 社会学",
            json!({"keywords": ["社会学", "人類学"]})
        ),
        Verdict::Admitted
    );
    assert_eq!(
        verdict(
            "@document.keywords not contains 農学",
            json!({"keywords": ["社会学"]})
        ),
        Verdict::Admitted
    );
}

#[test]
fn a_scalar_operator_over_a_list_is_a_shape_mismatch() {
    // Reusing `in` for containment would make one spelling mean two things
    // depending on a document's data; instead the mismatch is named.
    assert_eq!(
        verdict(
            "@document.keywords = 社会学",
            json!({"keywords": ["社会学"]})
        ),
        Verdict::Excluded(ExclusionReason::ShapeMismatch)
    );
}

#[test]
fn containment_over_a_scalar_is_a_shape_mismatch() {
    assert_eq!(
        verdict(
            "@document.subject_area contains 社会",
            json!({"subject_area": "社会学"})
        ),
        Verdict::Excluded(ExclusionReason::ShapeMismatch)
    );
}

#[test]
fn a_nested_value_is_a_shape_mismatch_under_every_operator() {
    assert_eq!(
        verdict("@document.meta = 1", json!({"meta": {"a": 1}})),
        Verdict::Excluded(ExclusionReason::ShapeMismatch)
    );
}

#[test]
fn a_boolean_compares_against_its_literal_spelling() {
    assert_eq!(
        verdict("@document.permission != true", json!({"permission": false})),
        Verdict::Admitted
    );
}

#[test]
fn the_reported_reason_is_the_first_unmet_predicate_in_source_order() {
    // Both a missing field and a shape mismatch sit under one false clause.
    // Source order decides, so the answer is stable and explainable.
    assert_eq!(
        verdict(
            "@document.absent = 1 & @document.pages >= 10",
            json!({"pages": "47-56"})
        ),
        Verdict::Excluded(ExclusionReason::MissingField)
    );
    assert_eq!(
        verdict(
            "@document.pages >= 10 & @document.absent = 1",
            json!({"pages": "47-56"})
        ),
        Verdict::Excluded(ExclusionReason::ShapeMismatch)
    );
}

#[test]
fn a_cleanly_false_clause_reports_the_predicate_reason() {
    assert_eq!(
        verdict("@document.year >= 2020", json!({"year": "2013"})),
        Verdict::Excluded(ExclusionReason::Predicate)
    );
}

#[test]
fn an_admitted_document_needs_no_reason_even_with_an_unmet_disjunct() {
    assert_eq!(
        verdict(
            "(@document.absent = 1 | @document.subject_area = 社会学)",
            json!({"subject_area": "社会学"})
        ),
        Verdict::Admitted
    );
}

#[test]
fn an_unmet_predicate_is_reported_even_when_an_earlier_one_was_cleanly_false() {
    // The case that forbids short-circuiting. `year >= 2020` is genuinely
    // false, so `all()` would stop there and report `Predicate`, never
    // reaching the missing `absent`. Then one clause with one answer would
    // report two different reasons depending on the order its predicates were
    // written in. Evaluation visits every branch so the reason is a property
    // of the clause, not of the walk.
    assert_eq!(
        verdict(
            "@document.year >= 2020 & @document.absent = 1",
            json!({"year": "2013"})
        ),
        Verdict::Excluded(ExclusionReason::MissingField)
    );
    assert_eq!(
        verdict(
            "@document.absent = 1 & @document.year >= 2020",
            json!({"year": "2013"})
        ),
        Verdict::Excluded(ExclusionReason::MissingField)
    );
}
