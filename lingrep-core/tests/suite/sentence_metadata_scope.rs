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
//! Sentence-grain metadata keys: the sentence-values axis of the two-pass
//! evaluator, and admission over a stream whose sentences carry metadata.

use std::io;

use crate::support::metadata;
use lingrep_core::corpus::{
    AdmittedCorpus, AdmittedCorpusEvent, CorpusEvent, Metadata, ScopeObservation,
    metadata_from_json,
};
use lingrep_core::scope::{DocumentOutcome, ExclusionReason, ScopeClause, Verdict};

#[test]
fn an_explicit_sentence_selector_defers_to_sentence_evaluation() {
    let clause = ScopeClause::parse("@sentence.sent_id = train-s42").expect("parses");

    // Deferred even though the document metadata does not carry the key —
    // that is the point: the sentence pass answers it.
    assert_eq!(
        clause.evaluate_document(&metadata(serde_json::json!({}))),
        DocumentOutcome::RequiresSentences
    );
}

#[test]
fn a_false_document_conjunct_still_decides_at_document_grain() {
    // Kleene: False & Indeterminate = False. The document pass never reads a
    // sentence it can already exclude.
    let clause =
        ScopeClause::parse("@sentence.sent_id = s1 & @document.year >= 2010").expect("parses");

    assert_eq!(
        clause.evaluate_document(&metadata(serde_json::json!({"year": "1999"}))),
        DocumentOutcome::Decided(Verdict::Excluded(ExclusionReason::Predicate))
    );
}

#[test]
fn sentence_metadata_has_an_explicit_owner() {
    let clause = ScopeClause::parse("@sentence.sent_id = s1").expect("parses");
    let document = metadata(serde_json::json!({"newdoc_id": "d1"}));
    let labels: [String; 0] = [];

    assert_eq!(
        clause.evaluate_sentence(
            &document,
            &metadata(serde_json::json!({"sent_id": "s1"})),
            labels.as_slice(),
            &Default::default()
        ),
        Verdict::Admitted
    );
    assert_eq!(
        clause.evaluate_sentence(
            &document,
            &metadata(serde_json::json!({"sent_id": "s2"})),
            labels.as_slice(),
            &Default::default()
        ),
        Verdict::Excluded(ExclusionReason::Predicate)
    );
    // A sentence without the key falls back to the document, which lacks it
    // too: a missing field, counted as one.
    assert_eq!(
        clause.evaluate_sentence(
            &document,
            &Metadata::default(),
            labels.as_slice(),
            &Default::default()
        ),
        Verdict::Excluded(ExclusionReason::MissingField)
    );
}

#[test]
fn a_document_key_still_answers_when_the_sentence_lacks_it() {
    let clause = ScopeClause::parse("@document.newdoc_id = d1").expect("parses");
    let labels: [String; 0] = [];

    assert_eq!(
        clause.evaluate_sentence(
            &metadata(serde_json::json!({"newdoc_id": "d1"})),
            &metadata(serde_json::json!({"sent_id": "s1"})),
            labels.as_slice(),
            &Default::default()
        ),
        Verdict::Admitted
    );
}

#[test]
fn metadata_keys_reports_every_named_key_and_no_section_label() {
    let clause = ScopeClause::parse(
        "@sentence.sent_id = s1 & (@section = methods | @document.year >= 2010)",
    )
    .expect("parses");

    assert_eq!(
        clause.metadata_keys(),
        std::collections::BTreeSet::from(["@sentence.sent_id", "@document.year"])
    );
    assert!(clause.requires_sentence_values());
    assert!(
        !ScopeClause::parse("@document.year >= 2010")
            .expect("parses")
            .requires_sentence_values()
    );
}

fn conllu_like_stream() -> Vec<io::Result<CorpusEvent>> {
    let sentence = |text: &str, sent_id: Option<&str>| CorpusEvent::Sentence {
        text: text.to_owned(),
        words: None,
        enhanced_graph: None,
        active_labels: Box::new([]),
        metadata: sent_id.map_or_else(Metadata::default, |id| {
            metadata(serde_json::json!({"sent_id": id}))
        }),
    };
    vec![
        Ok(CorpusEvent::DocumentStart {
            source_name: "fixture.conllu".to_owned(),
            metadata: Metadata::default(),
        }),
        Ok(CorpusEvent::ParagraphStart {
            metadata: Metadata::default(),
        }),
        Ok(sentence("一文目。", Some("s1"))),
        Ok(sentence("二文目。", Some("s2"))),
        Ok(sentence("三文目。", None)),
        Ok(CorpusEvent::ParagraphEnd),
        Ok(CorpusEvent::DocumentEnd),
    ]
}

#[test]
fn admission_selects_exactly_the_cited_sentence_and_counts_the_rest() {
    let clause = ScopeClause::parse("@sentence.sent_id = s2").expect("parses");
    let observation = ScopeObservation::default();
    let admitted = AdmittedCorpus::new(conllu_like_stream().into_iter())
        .with_scope(clause, observation.clone())
        .collect::<io::Result<Vec<_>>>()
        .expect("admission succeeds");

    let sentences = admitted
        .iter()
        .filter_map(AdmittedCorpusEvent::sentence)
        .collect::<Vec<_>>();
    assert_eq!(sentences.len(), 1);
    assert_eq!(sentences[0].text, "二文目。");
    // Compacted ordinal: the excluded sentence before it consumed none.
    assert_eq!(sentences[0].id.get(), 0);

    let counts = observation.counts().expect("counts publish");
    assert_eq!(counts.documents_admitted, 1);
    assert_eq!(counts.sentences_admitted, 1);
    assert_eq!(counts.sentences_excluded_by_predicate, 1);
    assert_eq!(counts.sentences_excluded_missing_field, 1);
}

#[test]
fn explicit_sentence_ownership_is_independent_of_format_key_declarations() {
    // The same events without the declaration: the document pass decides
    // missing-field for the whole document, the path every other format takes.
    let clause = ScopeClause::parse("@sentence.sent_id = s2").expect("parses");
    let observation = ScopeObservation::default();
    let admitted = AdmittedCorpus::new(conllu_like_stream().into_iter())
        .with_scope(clause, observation.clone())
        .collect::<io::Result<Vec<_>>>()
        .expect("admission succeeds");

    assert_eq!(
        admitted
            .iter()
            .filter_map(AdmittedCorpusEvent::sentence)
            .count(),
        1
    );
    let counts = observation.counts().expect("counts publish");
    assert_eq!(counts.documents_admitted, 1);
    assert_eq!(counts.sentences_excluded_missing_field, 1);
}

#[test]
fn source_metadata_projection_preserves_types_override_absence_and_bounds() {
    use lingrep_core::corpus::SourceMetadataProjection;
    let document = metadata_from_json(serde_json::json!({"id":"document", "zero":0, "empty":"", "flag":false, "null":null, "list":[1,"x"], "object":{"x":1}, "private":"omit"}), "test").unwrap();
    let sentence = metadata_from_json(serde_json::json!({"id":"sentence"}), "test").unwrap();
    let projection = SourceMetadataProjection::new(
        [
            "@sentence.id",
            "@document.zero",
            "@document.empty",
            "@document.flag",
            "@document.null",
            "@document.list",
            "@document.object",
            "@document.missing",
        ]
        .map(str::to_owned)
        .to_vec(),
    )
    .unwrap();
    assert_eq!(
        serde_json::Value::Object(
            projection
                .project(&document, &sentence, &Default::default())
                .unwrap()
        ),
        serde_json::json!({"sentence":{"id":"sentence"},"document":{"zero":0, "empty":"", "flag":false, "null":null, "list":[1,"x"], "object":{"x":1}}})
    );
    for keys in [
        vec![String::new()],
        vec!["x".to_owned(), "x".to_owned()],
        vec!["x".repeat(257)],
        (0..257).map(|key| key.to_string()).collect(),
    ] {
        assert!(SourceMetadataProjection::new(keys).is_err());
    }
    let huge = metadata_from_json(serde_json::json!({"id":"x".repeat(65536)}), "test").unwrap();
    assert!(
        SourceMetadataProjection::new(vec!["@document.id".to_owned()])
            .unwrap()
            .project(&huge, &Default::default(), &Default::default())
            .is_err()
    );
}
