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
//! Scope applied at direct-input sentence admission.
//!
//! The runner-side skip lives in `execution_runner`, beside the partitioner
//! helpers it needs.

use lingrep_core::corpus::{
    AdmittedCorpus, AdmittedCorpusEvent, CorpusEvent, ScopeObservation, metadata_from_json,
};
use lingrep_core::scope::ScopeClause;

fn events(documents: &[(serde_json::Value, &[&str])]) -> Vec<std::io::Result<CorpusEvent>> {
    let mut events = Vec::new();
    for (metadata, sentences) in documents {
        events.push(Ok(CorpusEvent::DocumentStart {
            source_name: "test".to_owned(),
            metadata: metadata_from_json(metadata.clone(), "test").unwrap(),
        }));
        events.push(Ok(CorpusEvent::ParagraphStart {
            metadata: Box::new([]),
        }));
        for text in *sentences {
            events.push(Ok(CorpusEvent::Sentence {
                text: (*text).to_owned(),
                words: None,
                enhanced_graph: None,
                active_labels: Box::new([]),
                metadata: Box::new([]),
            }));
        }
        events.push(Ok(CorpusEvent::ParagraphEnd));
        events.push(Ok(CorpusEvent::DocumentEnd));
    }
    events
}

#[test]
fn a_direct_scope_skip_consumes_no_sentence_id() {
    // Ordinals compact, exactly like the empty-line skip on the line above the
    // scope skip. A skipped sentence has never consumed an id on this path.
    let corpus = AdmittedCorpus::new(
        events(&[
            (serde_json::json!({"year": "2005"}), &["古い"] as &[&str]),
            (serde_json::json!({"year": "2013"}), &["新しい", "も"]),
        ])
        .into_iter(),
    )
    .with_scope(
        ScopeClause::parse("@document.year >= 2010").unwrap(),
        ScopeObservation::default(),
    );

    let sentences = corpus
        .filter_map(|event| match event.unwrap() {
            AdmittedCorpusEvent::Sentence(sentence) => Some(sentence),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        sentences
            .iter()
            .map(|sentence| (sentence.id.get(), sentence.text.clone()))
            .collect::<Vec<_>>(),
        vec![(0, "新しい".to_owned()), (1, "も".to_owned())],
        "the admitted document's sentences start at 0"
    );
}

#[test]
fn direct_scope_counts_document_and_sentence_buckets_separately() {
    // The counts are read from the observation handle, not from the corpus:
    // this test crosses the same ownership boundary the command does, so it
    // cannot pass while the command has no way to reach the numbers. Read at
    // the end because on the direct path the denominator is not known until
    // the stream finishes — never as a running fraction.
    let observation = ScopeObservation::default();
    let mut corpus = AdmittedCorpus::new(
        events(&[
            (serde_json::json!({"year": "2013"}), &["a", "b"] as &[&str]),
            (serde_json::json!({"year": "2005"}), &["c"]),
            (serde_json::json!({"title": "no year"}), &["d", "e", "f"]),
            (serde_json::json!({"year": "47-56"}), &["g"]),
        ])
        .into_iter(),
    )
    .with_scope(
        ScopeClause::parse("@document.year >= 2010").unwrap(),
        observation.clone(),
    );

    for event in corpus.by_ref() {
        event.unwrap();
    }
    drop(corpus);

    let counts = observation.counts().unwrap();
    assert_eq!(counts.documents_admitted, 1);
    assert_eq!(counts.documents_excluded_by_predicate, 1);
    assert_eq!(counts.documents_excluded_missing_field, 1);
    assert_eq!(counts.documents_excluded_shape_mismatch, 1);
    assert_eq!(counts.sentences_admitted, 2);
    assert_eq!(counts.sentences_excluded_by_predicate, 1);
    assert_eq!(counts.sentences_excluded_missing_field, 3);
    assert_eq!(counts.sentences_excluded_shape_mismatch, 1);
}

#[test]
fn an_unscoped_reader_admits_every_document_and_counts_nothing_as_excluded() {
    let observation = ScopeObservation::default();
    let corpus = AdmittedCorpus::new(
        events(&[
            (serde_json::json!({"year": "2013"}), &["a"] as &[&str]),
            (serde_json::json!({}), &["b"]),
        ])
        .into_iter(),
    );

    let mut admitted = 0;
    for event in corpus {
        if matches!(event.unwrap(), AdmittedCorpusEvent::Sentence(_)) {
            admitted += 1;
        }
    }

    assert_eq!(admitted, 2, "no clause admits everything");
    assert_eq!(
        observation.counts().unwrap(),
        lingrep_core::corpus::ScopeCounts::default(),
        "an unscoped run's handle is untouched"
    );
}

#[test]
fn a_partially_read_document_still_publishes_its_tallies_on_drop() {
    // Counts are tallied locally and published per document, so a run that
    // stops with the reader inside a document never sees the `DocumentEnd`
    // that would publish it. A `--limit`ed run stops exactly there, and the
    // drop flush is what keeps the reported universe equal to what the reader
    // actually observed.
    let observation = ScopeObservation::default();
    let mut corpus = AdmittedCorpus::new(
        events(&[(
            serde_json::json!({"year": "2013"}),
            &["a", "b", "c", "d"] as &[&str],
        )])
        .into_iter(),
    )
    .with_scope(
        ScopeClause::parse("@document.year >= 2010").unwrap(),
        observation.clone(),
    );

    // Document start, paragraph start, then two sentences — stopping before
    // the paragraph and document ever close.
    for _ in 0..4 {
        corpus.next().unwrap().unwrap();
    }
    assert_eq!(
        observation.counts().unwrap().sentences_admitted,
        0,
        "nothing is published while the document is still open"
    );

    drop(corpus);

    let counts = observation.counts().unwrap();
    assert_eq!(counts.documents_admitted, 1);
    assert_eq!(
        counts.sentences_admitted, 2,
        "the two sentences read before the stop are still counted"
    );
}

#[test]
fn a_closed_document_publishes_without_waiting_for_a_drop() {
    let observation = ScopeObservation::default();
    let mut corpus = AdmittedCorpus::new(
        events(&[
            (serde_json::json!({"year": "2013"}), &["a", "b"] as &[&str]),
            (serde_json::json!({"year": "2005"}), &["c"]),
        ])
        .into_iter(),
    )
    .with_scope(
        ScopeClause::parse("@document.year >= 2010").unwrap(),
        observation.clone(),
    );

    // Document start, paragraph start, two sentences, paragraph end, document
    // end: the first document is complete.
    for _ in 0..6 {
        corpus.next().unwrap().unwrap();
    }

    let counts = observation.counts().unwrap();
    assert_eq!(counts.documents_admitted, 1);
    assert_eq!(counts.sentences_admitted, 2);
}
