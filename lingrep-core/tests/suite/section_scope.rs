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
//! `@section` predicates: parsing, refusals, and the two-pass evaluator.

use std::collections::BTreeSet;

use crate::support::metadata;
use lingrep_core::corpus::Metadata;
use lingrep_core::scope::{DocumentOutcome, ExclusionReason, ScopeClause, Verdict};

fn labels(active: &[&str]) -> Vec<String> {
    active.iter().map(|label| (*label).to_owned()).collect()
}

#[test]
fn section_predicates_parse_in_all_four_spellings() {
    for source in [
        "@section = methods",
        "@section != references",
        "@section in {methods, results}",
        "@section not in {references, caption}",
    ] {
        assert!(
            ScopeClause::parse(source).is_ok(),
            "expected `{source}` to parse"
        );
    }
}

#[test]
fn section_labels_reports_every_named_label_once() {
    let clause = ScopeClause::parse(
        "@section in {methods, results} & (@document.year >= 2010 | @section = methods)",
    )
    .expect("clause parses");
    assert!(clause.names_sections());
    assert_eq!(
        clause.section_labels(),
        BTreeSet::from(["methods", "results"])
    );
}

#[test]
fn a_metadata_only_clause_names_no_sections() {
    let clause = ScopeClause::parse("@document.year >= 2010 & @document.subject_area = 社会学")
        .expect("parses");
    assert!(!clause.names_sections());
    assert!(clause.section_labels().is_empty());
}

/// A label is an opaque string: it has no order and no parts, so the operators
/// that assume either are refused where the author wrote them.
#[test]
fn ordered_and_containment_operators_are_refused_on_section() {
    for source in [
        "@section > methods",
        "@section <= methods",
        "@section contains methods",
        "@section not contains methods",
    ] {
        let error = ScopeClause::parse(source).expect_err("expected a refusal");
        assert!(
            error.message.contains("`=`, `!=`, `in` or `not in`"),
            "message did not name the available operators: {}",
            error.message
        );
    }
}

#[test]
fn an_unknown_structural_fact_names_the_known_ones() {
    let error = ScopeClause::parse("@paragraph_initial = true").expect_err("expected a refusal");
    assert!(
        error.message.contains("@section"),
        "message did not name the known facts: {}",
        error.message
    );
    assert_eq!(error.byte_range, 0.."@paragraph_initial".len());
}

/// The sigil exists so a metadata key spelled `section` stays reachable. This
/// is the property the whole spelling decision was made for.
#[test]
fn a_metadata_key_named_section_is_still_queryable() {
    let clause = ScopeClause::parse("@document.section = methods").expect("clause parses");
    assert!(!clause.names_sections());
}

/// A quoted `"@section"` is a value, not the sigil.
#[test]
fn a_quoted_at_section_is_an_ordinary_value() {
    let clause = ScopeClause::parse(r#"@document.topic = "@section""#).expect("clause parses");
    assert!(!clause.names_sections());
}

/// The property the whole two-pass split exists for: a clause naming no
/// sections is *always* decided, so it never reaches per-sentence evaluation.
#[test]
fn a_metadata_only_clause_is_always_decided() {
    let clause = ScopeClause::parse("@document.year >= 2010").expect("parses");
    assert_eq!(
        clause.evaluate_document(&metadata(serde_json::json!({"year": "2013"}))),
        DocumentOutcome::Decided(Verdict::Admitted)
    );
    assert_eq!(
        clause.evaluate_document(&metadata(serde_json::json!({"year": "2005"}))),
        DocumentOutcome::Decided(Verdict::Excluded(ExclusionReason::Predicate))
    );
    assert_eq!(
        clause.evaluate_document(&metadata(serde_json::json!({"title": "none"}))),
        DocumentOutcome::Decided(Verdict::Excluded(ExclusionReason::MissingField))
    );
}

#[test]
fn a_section_clause_defers_to_sentences() {
    let clause = ScopeClause::parse("@section = methods").expect("parses");
    assert_eq!(
        clause.evaluate_document(&metadata(serde_json::json!({}))),
        DocumentOutcome::RequiresSentences
    );
}

/// A conjunction whose metadata half is already false is decided without
/// looking at a single label — the document skip survives section scope.
#[test]
fn a_false_metadata_conjunct_decides_a_section_clause() {
    let clause = ScopeClause::parse("@document.year >= 2010 & @section = methods").expect("parses");
    assert_eq!(
        clause.evaluate_document(&metadata(serde_json::json!({"year": "2005"}))),
        DocumentOutcome::Decided(Verdict::Excluded(ExclusionReason::Predicate))
    );
    assert_eq!(
        clause.evaluate_document(&metadata(serde_json::json!({"year": "2013"}))),
        DocumentOutcome::RequiresSentences
    );
}

/// A missing field decides a conjunction too, and keeps its own reason.
#[test]
fn a_missing_field_decides_a_section_conjunction_and_keeps_its_reason() {
    let clause = ScopeClause::parse("@document.year >= 2010 & @section = methods").expect("parses");
    assert_eq!(
        clause.evaluate_document(&metadata(serde_json::json!({"title": "no year"}))),
        DocumentOutcome::Decided(Verdict::Excluded(ExclusionReason::MissingField))
    );
}

/// A disjunction whose metadata half is true is decided *admitted*, so its
/// sentences are never evaluated.
#[test]
fn a_true_metadata_disjunct_decides_a_section_clause() {
    let clause =
        ScopeClause::parse("(@document.year >= 2010 | @section = methods)").expect("parses");
    assert_eq!(
        clause.evaluate_document(&metadata(serde_json::json!({"year": "2013"}))),
        DocumentOutcome::Decided(Verdict::Admitted)
    );
}

#[test]
fn section_membership_evaluates_against_active_labels() {
    let empty = metadata(serde_json::json!({}));
    let cases = [
        ("@section = methods", &["methods", "results"][..], true),
        ("@section = methods", &["results"][..], false),
        ("@section != methods", &["results"][..], true),
        ("@section != methods", &["methods"][..], false),
        ("@section in {methods, results}", &["results"][..], true),
        ("@section in {methods, results}", &["caption"][..], false),
        ("@section not in {methods}", &["results"][..], true),
        ("@section not in {methods}", &["methods"][..], false),
        ("@section = methods", &[][..], false),
        ("@section != methods", &[][..], true),
    ];
    for (source, active, expected) in cases {
        let clause = ScopeClause::parse(source).expect("parses");
        let verdict = clause.evaluate_sentence(
            &empty,
            &Metadata::default(),
            labels(active).as_slice(),
            &Default::default(),
        );
        assert_eq!(
            verdict == Verdict::Admitted,
            expected,
            "`{source}` against {active:?}"
        );
    }
}

/// A section predicate is two-valued: it can only ever produce `Predicate`.
/// The other two reasons are properties of a metadata *value*, and a label set
/// is neither absent nor ill-shaped.
#[test]
fn a_failing_section_predicate_reports_predicate_and_nothing_else() {
    let clause = ScopeClause::parse("@section in {methods}").expect("parses");
    assert_eq!(
        clause.evaluate_sentence(
            &metadata(serde_json::json!({})),
            &Metadata::default(),
            labels(&[]).as_slice(),
            &Default::default()
        ),
        Verdict::Excluded(ExclusionReason::Predicate)
    );
}

/// Reason precedence stays order-independent once section leaves are in the
/// tree: the reported reason is the first *unmet* leaf in source order, and a
/// section leaf is never unmet, so it cannot displace a metadata reason.
#[test]
fn a_section_leaf_never_displaces_a_metadata_reason() {
    let both = metadata(serde_json::json!({"pages": "47-56"}));
    for source in [
        "@section = methods & @document.pages >= 40",
        "@document.pages >= 40 & @section = methods",
    ] {
        let clause = ScopeClause::parse(source).expect("parses");
        assert_eq!(
            clause.evaluate_sentence(
                &both,
                &Metadata::default(),
                labels(&["results"]).as_slice(),
                &Default::default()
            ),
            Verdict::Excluded(ExclusionReason::ShapeMismatch),
            "`{source}` reported the wrong reason"
        );
    }
}

use lingrep_core::corpus::{AdmittedCorpus, AdmittedCorpusEvent, CorpusEvent, ScopeObservation};

fn document(year: &str, sentences: &[(&str, &[&str])]) -> Vec<std::io::Result<CorpusEvent>> {
    let mut events = vec![
        Ok(CorpusEvent::DocumentStart {
            source_name: "test".to_owned(),
            metadata: metadata(serde_json::json!({ "year": year })),
        }),
        Ok(CorpusEvent::ParagraphStart {
            metadata: Box::new([]),
        }),
    ];
    for (text, active) in sentences {
        events.push(Ok(CorpusEvent::Sentence {
            text: (*text).to_owned(),
            words: None,
            enhanced_graph: None,
            active_labels: labels(active).into_boxed_slice(),
            metadata: Box::new([]),
        }));
    }
    events.push(Ok(CorpusEvent::ParagraphEnd));
    events.push(Ok(CorpusEvent::DocumentEnd));
    events
}

fn admitted_texts(
    events: Vec<std::io::Result<CorpusEvent>>,
    clause: &str,
    observation: &ScopeObservation,
) -> Vec<String> {
    AdmittedCorpus::new(events.into_iter())
        .with_scope(
            ScopeClause::parse(clause).expect("clause parses"),
            observation.clone(),
        )
        .filter_map(|event| match event.expect("reader") {
            AdmittedCorpusEvent::Sentence(sentence) => Some(sentence.text),
            _ => None,
        })
        .collect()
}

#[test]
fn a_section_clause_selects_sentences_within_one_document() {
    let events = document(
        "2013",
        &[
            ("猫が鳥", &["methods"]),
            ("犬も鳥", &["results"]),
            ("猫は犬", &["methods", "results"]),
            ("鳥が一", &[]),
        ],
    );
    let observation = ScopeObservation::default();
    let texts = admitted_texts(events, "@section = methods", &observation);
    assert_eq!(texts, vec!["猫が鳥".to_owned(), "猫は犬".to_owned()]);

    let counts = observation.counts().expect("counts");
    assert_eq!(counts.sentences_admitted, 2);
    assert_eq!(counts.sentences_excluded_by_predicate, 2);
    assert_eq!(counts.sentences_excluded_missing_field, 0);
    assert_eq!(counts.sentences_excluded_shape_mismatch, 0);
}

/// Direct-input ordinals compact, so a section-excluded sentence leaves no gap,
/// matching metadata-based exclusion.
#[test]
fn section_exclusion_compacts_direct_ordinals() {
    let events = document(
        "2013",
        &[
            ("猫が鳥", &["results"]),
            ("犬も鳥", &["methods"]),
            ("猫は犬", &["methods"]),
        ],
    );
    let ordinals: Vec<u64> = AdmittedCorpus::new(events.into_iter())
        .with_scope(
            ScopeClause::parse("@section = methods").expect("parses"),
            ScopeObservation::default(),
        )
        .filter_map(|event| match event.expect("reader") {
            AdmittedCorpusEvent::Sentence(sentence) => Some(sentence.id.get()),
            _ => None,
        })
        .collect();
    assert_eq!(ordinals, vec![0, 1]);
}

/// The document grain and the sentence grain need not move together, and the
/// report must show it rather than quietly reconciling them.
#[test]
fn a_document_can_be_admitted_with_no_admitted_sentences() {
    let events = document("2013", &[("猫が鳥", &["references"])]);
    let observation = ScopeObservation::default();
    let texts = admitted_texts(
        events,
        "@document.year >= 2010 & @section = methods",
        &observation,
    );
    assert!(texts.is_empty());

    let counts = observation.counts().expect("counts");
    assert_eq!(counts.documents_admitted, 1);
    assert_eq!(counts.sentences_admitted, 0);
    assert_eq!(counts.sentences_excluded_by_predicate, 1);
}

/// The metadata half still decides whole documents, so a section clause does
/// not force per-sentence work on documents the metadata already excluded.
#[test]
fn a_metadata_excluded_document_keeps_its_own_reason_under_a_section_clause() {
    let events = document("2005", &[("猫が鳥", &["methods"])]);
    let observation = ScopeObservation::default();
    let texts = admitted_texts(
        events,
        "@document.year >= 2010 & @section = methods",
        &observation,
    );
    assert!(texts.is_empty());

    let counts = observation.counts().expect("counts");
    assert_eq!(counts.documents_excluded_by_predicate, 1);
    assert_eq!(counts.sentences_excluded_by_predicate, 1);
}
