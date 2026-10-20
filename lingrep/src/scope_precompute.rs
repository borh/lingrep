//! Resolving a scope clause against a prepared artifact, once per run.
//!
//! Because a document's sentence count falls out of the same scan that yields
//! its metadata, the whole four-bucket universe is known **before** the run
//! starts: the prepared path reports a denominator it never had to observe,
//! and that stays true when the run is cut short by `--limit`.

use std::collections::BTreeMap;

use lingrep_core::corpus::{DocumentId, Metadata, metadata_from_json};
use lingrep_core::execution::SentenceAdmission;
use lingrep_core::scope::{ActiveLabels, DocumentOutcome, ScopeClause, Verdict};
use lingrep_corpus::{DocumentMetadataRow, RegionSpanRow};
use roaring::RoaringTreemap;

use crate::protocol::query::ScopeUniverse;

/// A prepared run's resolved scope: who is admitted, and the whole universe.
#[derive(Debug)]
pub struct ScopeResolution {
    pub admission: SentenceAdmission,
    pub documents: ScopeUniverse,
    pub sentences: ScopeUniverse,
}

/// A stored row this run refuses to interpret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeResolutionError {
    pub document_id: Option<DocumentId>,
    pub message: String,
}

/// Judges every document once and sums both grains.
/// Sentence metadata must be supplied in ascending, unique ordinal order.
///
/// Fallible because absent metadata and malformed metadata are different
/// facts. A `NULL` row is a document the writer recorded as having none and
/// evaluates as a missing field; a row that will not parse, or whose shape the
/// typing rule rejects, is a violated artifact invariant and stops the run.
/// The writer serialises this column with `serde_json::to_string` over an
/// already-validated `Metadata`, so there is no third possibility that is
/// merely unusual data.
///
/// # Errors
///
/// Returns the first document whose stored metadata cannot be interpreted, or
/// an overflow if the accounting would not be exact.
pub fn resolve_scope(
    clause: &ScopeClause,
    rows: impl IntoIterator<Item = Result<DocumentMetadataRow, ScopeResolutionError>>,
    spans: &[RegionSpanRow],
    sentence_metadata: impl IntoIterator<
        Item = Result<
            (
                u64,
                Metadata,
                std::sync::Arc<lingrep_core::corpus::StructuralContext>,
            ),
            ScopeResolutionError,
        >,
    >,
) -> Result<ScopeResolution, ScopeResolutionError> {
    let index = LabelIndex::build(spans);
    // A clause naming no section and no sentence-grain key keeps the
    // document-grain path: one lookup per document instead of one per
    // sentence, and no region or sentence-metadata query was issued either.
    // This is the path every run took before section scope existed, taken by
    // the same code.
    let mut by_sentence = clause.names_sections() || clause.requires_sentence_values();
    let mut reads_sentence_metadata = clause.requires_sentence_values();
    let mut sentence_metadata = sentence_metadata.into_iter().peekable();
    let mut admitted = RoaringTreemap::new();
    let mut admitted_documents = RoaringTreemap::new();
    let mut documents = ScopeUniverse::default();
    let mut sentences = ScopeUniverse::default();
    for row in rows {
        let row = row?;
        let document_reads_sentences = clause.requires_sentence_values();
        reads_sentence_metadata |= document_reads_sentences;
        by_sentence |= document_reads_sentences;
        let overflow = |_| ScopeResolutionError {
            document_id: Some(row.document_id),
            message: "scope accounting overflowed".to_owned(),
        };
        let end = row
            .first_sentence
            .checked_add(row.sentence_count)
            .ok_or_else(|| ScopeResolutionError {
                document_id: Some(row.document_id),
                message: "document sentence range overflowed".to_owned(),
            })?;
        // Typed once and used by both passes. A `NULL` row types as *empty*
        // metadata rather than short-circuiting to `MissingField`: for a
        // metadata predicate the two agree — a lookup into empty metadata is a
        // missing field — but for `@section = methods`, which names no metadata
        // key at all, short-circuiting would exclude every sentence of a
        // document merely because it carried no frontmatter.
        let metadata = document_metadata(&row)?;
        match clause.evaluate_document(&metadata) {
            DocumentOutcome::Decided(verdict) => {
                documents.record(verdict, 1).map_err(overflow)?;
                sentences
                    .record(verdict, row.sentence_count)
                    .map_err(overflow)?;
                if verdict == Verdict::Admitted {
                    admitted.insert_range(row.first_sentence..end);
                    admitted_documents.insert(row.document_id.get());
                }
            }
            DocumentOutcome::RequiresSentences => {
                // Admitted at document grain: the document was *in scope*,
                // whether or not any of its sentences turn out to be. The two
                // grains no longer move together and the report says so.
                documents.record(Verdict::Admitted, 1).map_err(overflow)?;
                for ordinal in row.first_sentence..end {
                    let mut sentence_values = Metadata::default();
                    let mut structure = std::sync::Arc::default();
                    while document_reads_sentences
                        && let Some(value) = sentence_metadata.next_if(|value| {
                            value.as_ref().map_or(true, |(id, _, _)| *id <= ordinal)
                        })
                    {
                        let (id, values, context) = value?;
                        if id == ordinal {
                            sentence_values = values;
                            structure = context;
                            break;
                        }
                    }
                    clause.validate_structure(&structure).map_err(|message| {
                        ScopeResolutionError {
                            document_id: Some(row.document_id),
                            message,
                        }
                    })?;
                    let verdict = clause.evaluate_sentence(
                        &metadata,
                        &sentence_values,
                        &index.at(ordinal),
                        &structure,
                    );
                    sentences.record(verdict, 1).map_err(overflow)?;
                    if verdict == Verdict::Admitted {
                        admitted.insert(ordinal);
                    }
                }
            }
        }
    }
    if reads_sentence_metadata {
        for row in sentence_metadata {
            row?;
        }
    }
    Ok(ScopeResolution {
        admission: if by_sentence {
            SentenceAdmission::Sentences(admitted)
        } else {
            SentenceAdmission::Documents(admitted_documents)
        },
        documents,
        sentences,
    })
}

/// The named labels' spans, as one bitmap per label.
///
/// Built once per run from at most a few thousand rows — 16 labels over 8,377
/// spans on the reference corpus — and then asked one question per sentence.
#[derive(Debug, Default)]
struct LabelIndex {
    by_label: BTreeMap<String, RoaringTreemap>,
}

impl LabelIndex {
    fn build(spans: &[RegionSpanRow]) -> Self {
        let mut by_label: BTreeMap<String, RoaringTreemap> = BTreeMap::new();
        for span in spans {
            by_label
                .entry(span.label.clone())
                .or_default()
                .insert_range(span.first_sentence..span.end_sentence);
        }
        Self { by_label }
    }

    const fn at(&self, sentence: u64) -> ActiveAt<'_> {
        ActiveAt {
            index: self,
            sentence,
        }
    }
}

/// One sentence's view of the index — the prepared path's [`ActiveLabels`].
struct ActiveAt<'a> {
    index: &'a LabelIndex,
    sentence: u64,
}

impl ActiveLabels for ActiveAt<'_> {
    fn contains(&self, label: &str) -> bool {
        self.index
            .by_label
            .get(label)
            .is_some_and(|spans| spans.contains(self.sentence))
    }
}

/// Types one document's stored metadata under the single typing rule.
///
/// Fallible because absent metadata and malformed metadata are different facts.
/// A `NULL` row is a document the writer recorded as having none; a row that
/// will not parse, or whose shape the typing rule rejects, is a violated
/// artifact invariant and stops the run. The writer serialises this column with
/// `serde_json::to_string` over an already-validated `Metadata`, so there is no
/// third possibility that is merely unusual data.
fn document_metadata(
    row: &DocumentMetadataRow,
) -> Result<lingrep_core::corpus::Metadata, ScopeResolutionError> {
    let Some(raw) = row.metadata_json.as_deref() else {
        return Ok(lingrep_core::corpus::Metadata::default());
    };
    let refuse = |message: String| ScopeResolutionError {
        document_id: Some(row.document_id),
        message,
    };
    let value = serde_json::from_str::<serde_json::Value>(raw).map_err(|error| {
        refuse(format!(
            "stored document metadata is not valid JSON: {error}"
        ))
    })?;
    metadata_from_json(value, "prepared document metadata").map_err(|error| {
        refuse(format!(
            "stored document metadata is not valid metadata: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A location in the named document. Only `document_id` is load-bearing
    /// for document-grain admission; the rest is filled to make one.
    fn resolve_scope(
        clause: &ScopeClause,
        rows: &[DocumentMetadataRow],
        spans: &[RegionSpanRow],
        sentence_metadata: impl IntoIterator<Item = Result<(u64, Metadata), ScopeResolutionError>>,
    ) -> Result<ScopeResolution, ScopeResolutionError> {
        super::resolve_scope(
            clause,
            rows.iter().cloned().map(Ok),
            spans,
            sentence_metadata
                .into_iter()
                .map(|row| row.map(|(id, metadata)| (id, metadata, Default::default()))),
        )
    }

    fn at_document(document_id: u64) -> lingrep_core::execution::SentenceLocation {
        lingrep_core::execution::SentenceLocation {
            source_metadata: None,
            corpus_sentence_ordinal: 0,
            document_id: DocumentId::new(document_id),
            document_ordinal: 0,
            document_sentence_ordinal: 0,
            source_name: std::sync::Arc::from("test"),
        }
    }

    fn row(id: u64, first: u64, count: u64, metadata: Option<&str>) -> DocumentMetadataRow {
        DocumentMetadataRow {
            document_id: DocumentId::new(id),
            first_sentence: first,
            sentence_count: count,
            metadata_json: metadata.map(str::to_owned),
        }
    }

    fn fixture() -> Vec<DocumentMetadataRow> {
        vec![
            row(0, 0, 2, Some(r#"{"year": "2013"}"#)),
            row(1, 2, 1, Some(r#"{"year": "2005"}"#)),
            row(2, 3, 3, Some(r#"{"title": "no year"}"#)),
            row(3, 6, 1, Some(r#"{"year": "47-56"}"#)),
        ]
    }

    #[test]
    fn the_four_buckets_sum_to_the_whole_universe_at_both_grains() {
        let clause = ScopeClause::parse("@document.year >= 2010").unwrap();

        let resolved = resolve_scope(&clause, &fixture(), &[], std::iter::empty()).unwrap();

        assert_eq!(resolved.documents.admitted, 1);
        assert_eq!(resolved.documents.excluded_by_predicate, 1);
        assert_eq!(resolved.documents.excluded_missing_field, 1);
        assert_eq!(resolved.documents.excluded_shape_mismatch, 1);
        assert_eq!(resolved.documents.total().unwrap(), 4);
        assert_eq!(resolved.sentences.admitted, 2);
        assert_eq!(resolved.sentences.excluded_by_predicate, 1);
        assert_eq!(resolved.sentences.excluded_missing_field, 3);
        assert_eq!(resolved.sentences.excluded_shape_mismatch, 1);
        assert_eq!(resolved.sentences.total().unwrap(), 7);
    }

    #[test]
    fn the_admission_set_names_exactly_the_admitted_documents() {
        let rows = vec![
            row(0, 0, 1, Some(r#"{"year": "2013"}"#)),
            row(7, 1, 1, Some(r#"{"year": "2005"}"#)),
        ];
        let clause = ScopeClause::parse("@document.year >= 2010").unwrap();

        let resolved = resolve_scope(&clause, &rows, &[], std::iter::empty()).unwrap();

        // Through `admits` rather than the treemap, so the assertion
        // exercises the same entry point the runner does.
        assert!(resolved.admission.admits(&at_document(0)));
        assert!(!resolved.admission.admits(&at_document(7)));
    }

    #[test]
    fn a_document_with_no_stored_metadata_is_a_missing_field_exclusion() {
        // Absent metadata is data, not corruption: the writer emits NULL for a
        // document whose metadata is empty. Not a shape mismatch either —
        // there is no value present whose shape could be wrong.
        let clause = ScopeClause::parse("@document.year >= 2010").unwrap();

        let resolved =
            resolve_scope(&clause, &[row(0, 0, 1, None)], &[], std::iter::empty()).unwrap();

        assert_eq!(resolved.documents.excluded_missing_field, 1);
    }

    #[test]
    fn unparseable_stored_json_fails_the_run_and_names_the_document() {
        // Malformed stored JSON is a violated artifact invariant, not an odd
        // document: the writer serialises it from an already-validated
        // `Metadata`. Counting it as a missing field would publish a
        // successful run over a false universe.
        let clause = ScopeClause::parse("@document.year >= 2010").unwrap();

        let error = resolve_scope(
            &clause,
            &[row(4, 0, 1, Some("{not json"))],
            &[],
            std::iter::empty(),
        )
        .unwrap_err();

        assert_eq!(error.document_id, Some(DocumentId::new(4)));
        assert!(
            error.message.contains("metadata"),
            "the failure must name what was malformed: {}",
            error.message
        );
    }

    #[test]
    fn stored_json_that_the_typing_rule_rejects_fails_the_run() {
        // Valid JSON, invalid metadata shape — a bare array is not an object.
        // Same disposition: the reader could never have written it.
        let clause = ScopeClause::parse("@document.year >= 2010").unwrap();

        assert_eq!(
            resolve_scope(
                &clause,
                &[row(1, 0, 1, Some("[1, 2, 3]"))],
                &[],
                std::iter::empty(),
            )
            .unwrap_err()
            .document_id,
            Some(DocumentId::new(1))
        );
    }

    #[test]
    fn a_value_of_the_wrong_shape_is_still_a_counted_outcome_not_a_failure() {
        // The spike's rule survives: one malformed *value* must not poison a
        // 1,534-document corpus. `"47-56"` is well-formed JSON carrying a
        // value the predicate cannot use.
        let clause = ScopeClause::parse("@document.year >= 2010").unwrap();

        let resolved = resolve_scope(
            &clause,
            &[row(0, 0, 1, Some(r#"{"year": "47-56"}"#))],
            &[],
            std::iter::empty(),
        )
        .unwrap();

        assert_eq!(resolved.documents.excluded_shape_mismatch, 1);
    }

    fn spans(entries: &[(&str, u64, u64)]) -> Vec<RegionSpanRow> {
        entries
            .iter()
            .map(|(label, first, end)| RegionSpanRow {
                label: (*label).to_owned(),
                first_sentence: *first,
                end_sentence: *end,
            })
            .collect()
    }

    fn at_sentence(ordinal: u64) -> lingrep_core::execution::SentenceLocation {
        lingrep_core::execution::SentenceLocation {
            source_metadata: None,
            corpus_sentence_ordinal: ordinal,
            document_id: DocumentId::new(0),
            document_ordinal: 0,
            document_sentence_ordinal: ordinal,
            source_name: std::sync::Arc::from("test"),
        }
    }

    #[test]
    fn a_section_clause_admits_by_sentence() {
        // One document, sentences 0..6; `methods` covers 1..3.
        let clause = ScopeClause::parse("@section = methods").unwrap();
        let rows = vec![row(0, 0, 6, Some(r#"{"year":"2013"}"#))];

        let resolved = resolve_scope(
            &clause,
            &rows,
            &spans(&[("methods", 1, 3)]),
            std::iter::empty(),
        )
        .unwrap();

        let SentenceAdmission::Sentences(admitted) = &resolved.admission else {
            panic!("expected sentence-grain admission");
        };
        assert_eq!(admitted.iter().collect::<Vec<_>>(), vec![1, 2]);
        assert!(resolved.admission.admits(&at_sentence(1)));
        assert!(!resolved.admission.admits(&at_sentence(3)));
        assert_eq!(resolved.sentences.admitted, 2);
        assert_eq!(resolved.sentences.excluded_by_predicate, 4);
        assert_eq!(resolved.documents.admitted, 1);
    }

    /// A clause naming no section stays on the document-grain path: one lookup
    /// per document instead of one per sentence.
    #[test]
    fn a_metadata_only_clause_still_admits_by_document() {
        let clause = ScopeClause::parse("@document.year >= 2010").unwrap();
        let rows = vec![row(0, 0, 6, Some(r#"{"year":"2013"}"#))];

        let resolved = resolve_scope(&clause, &rows, &[], std::iter::empty()).unwrap();

        assert!(matches!(
            resolved.admission,
            SentenceAdmission::Documents(_)
        ));
    }

    /// The metadata half still excludes whole documents under a section clause,
    /// and those sentences keep the metadata reason rather than `Predicate`.
    #[test]
    fn a_metadata_excluded_document_contributes_no_sentences_and_keeps_its_reason() {
        let clause = ScopeClause::parse("@document.year >= 2010 & @section = methods").unwrap();
        let rows = vec![
            row(0, 0, 3, Some(r#"{"year":"2013"}"#)),
            row(1, 3, 3, Some(r#"{"title":"no year"}"#)),
        ];

        let resolved = resolve_scope(
            &clause,
            &rows,
            &spans(&[("methods", 0, 2)]),
            std::iter::empty(),
        )
        .unwrap();

        assert_eq!(resolved.sentences.admitted, 2);
        assert_eq!(resolved.sentences.excluded_missing_field, 3);
        assert_eq!(resolved.documents.excluded_missing_field, 1);
    }

    /// A `NULL` metadata row must not exclude a document from a clause that
    /// names no metadata key. Short-circuiting `None` to `MissingField` — which
    /// is right for a metadata predicate — would silently empty this run.
    #[test]
    fn a_document_with_no_metadata_still_answers_a_pure_section_clause() {
        let clause = ScopeClause::parse("@section = methods").unwrap();

        let resolved = resolve_scope(
            &clause,
            &[row(0, 0, 2, None)],
            &spans(&[("methods", 0, 1)]),
            std::iter::empty(),
        )
        .unwrap();

        assert_eq!(resolved.sentences.admitted, 1);
        assert_eq!(resolved.sentences.excluded_by_predicate, 1);
        assert_eq!(resolved.sentences.excluded_missing_field, 0);
    }

    /// The four buckets sum to the corpus, section clause or not.
    #[test]
    fn the_four_sentence_buckets_sum_to_the_corpus_under_a_section_clause() {
        let clause = ScopeClause::parse("@section in {methods, results}").unwrap();
        let rows = vec![row(0, 0, 4, Some("{}")), row(1, 4, 4, Some("{}"))];

        let resolved = resolve_scope(
            &clause,
            &rows,
            &spans(&[("methods", 0, 2), ("results", 5, 7)]),
            std::iter::empty(),
        )
        .unwrap();

        assert_eq!(resolved.sentences.admitted, 4);
        assert_eq!(resolved.sentences.total().unwrap(), 8);
    }

    fn sentence_metadata(
        entries: &[(u64, &str)],
    ) -> Vec<Result<(u64, Metadata), ScopeResolutionError>> {
        entries
            .iter()
            .map(|(ordinal, json)| {
                let value = serde_json::from_str::<serde_json::Value>(json).unwrap();
                Ok((*ordinal, metadata_from_json(value, "test").unwrap()))
            })
            .collect()
    }

    /// A clause over a declared sentence-grain key admits by sentence,
    /// selecting exactly the cited one; sentences without the key are
    /// missing-field exclusions.
    #[test]
    fn a_declared_sentence_key_admits_exactly_the_cited_sentence() {
        let clause = ScopeClause::parse("@sentence.sent_id = s2").unwrap();
        let rows = vec![row(0, 0, 3, None)];

        let resolved = resolve_scope(
            &clause,
            &rows,
            &[],
            sentence_metadata(&[(0, r#"{"sent_id":"s1"}"#), (1, r#"{"sent_id":"s2"}"#)]),
        )
        .unwrap();

        let SentenceAdmission::Sentences(admitted) = &resolved.admission else {
            panic!("expected sentence-grain admission");
        };
        assert_eq!(admitted.iter().collect::<Vec<_>>(), vec![1]);
        assert_eq!(resolved.documents.admitted, 1);
        assert_eq!(resolved.sentences.admitted, 1);
        assert_eq!(resolved.sentences.excluded_by_predicate, 1);
        // Sentence 2 carries no sent_id row: a missing field, per sentence.
        assert_eq!(resolved.sentences.excluded_missing_field, 1);
    }

    #[test]
    fn explicit_sentence_scope_never_falls_back_to_document_fields() {
        let clause = ScopeClause::parse("@sentence.sent_id = s2").unwrap();
        let rows = vec![row(0, 0, 3, Some(r#"{"sent_id":"s2"}"#))];
        let resolved = resolve_scope(
            &clause,
            &rows,
            &[],
            sentence_metadata(&[(0, "{}"), (1, "{}"), (2, "{}")]),
        )
        .unwrap();
        assert!(matches!(
            resolved.admission,
            SentenceAdmission::Sentences(_)
        ));
        assert_eq!(resolved.sentences.excluded_missing_field, 3);
        assert_eq!(resolved.sentences.admitted, 0);
    }
    #[test]
    fn malformed_sentence_metadata_fails_even_when_its_document_is_excluded() {
        let clause = ScopeClause::parse("@document.year = 2020 & @sentence.sent_id = yes").unwrap();
        let error = ScopeResolutionError {
            document_id: None,
            message: "sentence 9: invalid stored metadata".to_owned(),
        };
        assert_eq!(
            resolve_scope(
                &clause,
                &[row(0, 0, 10, Some(r#"{"year":2010}"#))],
                &[],
                [Err(error.clone())]
            )
            .unwrap_err(),
            error
        );
    }
}
