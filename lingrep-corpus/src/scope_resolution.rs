//! Resolves scope clauses over the whole prepared artifact, including when a run stops early.

use std::collections::{BTreeMap, BTreeSet};

use crate::{BlockOn, CorpusError, DocumentMetadataRow, ProvenanceView, RegionSpanRow};
use lingrep_core::corpus::{DocumentId, Metadata, metadata_from_json};
use lingrep_core::execution::SentenceAdmission;
use lingrep_core::scope::{ActiveLabels, DocumentOutcome, ScopeClause, Verdict};
use roaring::RoaringTreemap;

use lingrep_core::scope::ExclusionReason;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScopeOverflow;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScopeUniverse {
    pub admitted: u64,
    pub excluded_by_predicate: u64,
    pub excluded_missing_field: u64,
    pub excluded_shape_mismatch: u64,
}

impl ScopeUniverse {
    pub fn record(&mut self, verdict: Verdict, weight: u64) -> Result<(), ScopeOverflow> {
        let bucket = match verdict {
            Verdict::Admitted => &mut self.admitted,
            Verdict::Excluded(ExclusionReason::Predicate) => &mut self.excluded_by_predicate,
            Verdict::Excluded(ExclusionReason::MissingField) => &mut self.excluded_missing_field,
            Verdict::Excluded(ExclusionReason::ShapeMismatch) => &mut self.excluded_shape_mismatch,
        };
        *bucket = bucket.checked_add(weight).ok_or(ScopeOverflow)?;
        Ok(())
    }

    pub fn total(&self) -> Result<u64, ScopeOverflow> {
        self.admitted
            .checked_add(self.excluded_by_predicate)
            .and_then(|sum| sum.checked_add(self.excluded_missing_field))
            .and_then(|sum| sum.checked_add(self.excluded_shape_mismatch))
            .ok_or(ScopeOverflow)
    }
}

/// Admission and counts for the whole artifact, before any run limit applies.
#[derive(Debug)]
pub struct ScopeResolution {
    pub admission: SentenceAdmission,
    pub documents: ScopeUniverse,
    pub sentences: ScopeUniverse,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeResolutionError {
    pub document_id: Option<DocumentId>,
    pub message: String,
}

/// Resolves admission and counts for documents and sentences; sentence metadata
/// must be supplied in ascending, unique ordinal order.
///
/// # Errors
///
/// Returns metadata read, parse or shape errors, invalid sentence-structure
/// errors, or count overflow; absent document metadata is treated as empty.
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

pub fn document_metadata(
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

#[derive(Debug)]
pub enum PreparedScopeError {
    /// The clause names `@section` and the artifact records no section label.
    NoSectionLabels,
    Corpus(CorpusError),
    Resolution(ScopeResolutionError),
}

impl From<CorpusError> for PreparedScopeError {
    fn from(error: CorpusError) -> Self {
        Self::Corpus(error)
    }
}

/// Resolves the whole artifact on a blocking thread and returns its section
/// labels when `clause` references `@section`.
///
/// # Errors
///
/// See [`PreparedScopeError`].
pub fn resolve_prepared_scope(
    view: &ProvenanceView,
    runtime: &impl BlockOn,
    clause: &ScopeClause,
) -> Result<(ScopeResolution, BTreeSet<String>), PreparedScopeError> {
    let (spans, labels) = runtime.block_on(async {
        let (spans, labels) = if clause.names_sections() {
            (
                view.region_spans_for(&clause.section_labels()).await?,
                view.section_coverage(crate::MAX_SECTION_LABELS)
                    .await?
                    .into_iter()
                    .map(|coverage| coverage.label)
                    .collect(),
            )
        } else {
            (Vec::new(), BTreeSet::new())
        };
        Ok::<_, CorpusError>((spans, labels))
    })?;
    if clause.names_sections() && labels.is_empty() {
        return Err(PreparedScopeError::NoSectionLabels);
    }
    let mut page = Vec::new().into_iter();
    let mut next = 0;
    let mut finished = false;
    let sentence_metadata = std::iter::from_fn(|| {
        if finished {
            return None;
        }
        if page.len() == 0 {
            match runtime.block_on(view.scope_metadata_page(next, clause.names_divisions())) {
                Ok(rows) if rows.is_empty() => {
                    finished = true;
                    return None;
                }
                Ok(rows) => page = rows.into_iter(),
                Err(error) => {
                    finished = true;
                    return Some(Err(ScopeResolutionError {
                        document_id: None,
                        message: error.to_string(),
                    }));
                }
            }
        }
        let (ordinal, metadata, structure) = page.next()?;
        next = ordinal + 1;
        Some(Ok((ordinal, metadata, structure)))
    });
    let mut document_page = Vec::new().into_iter();
    let mut document_next = 0;
    let mut documents_finished = false;
    let documents = std::iter::from_fn(|| {
        if documents_finished {
            return None;
        }
        if document_page.len() == 0 {
            match runtime.block_on(view.document_metadata_page(document_next)) {
                Ok(rows) if rows.is_empty() => {
                    documents_finished = true;
                    return None;
                }
                Ok(rows) => document_page = rows.into_iter(),
                Err(error) => {
                    documents_finished = true;
                    return Some(Err(ScopeResolutionError {
                        document_id: None,
                        message: error.to_string(),
                    }));
                }
            }
        }
        let row = document_page.next()?;
        document_next = row.first_sentence + row.sentence_count;
        Some(Ok(row))
    });
    let resolution = resolve_scope(clause, documents, &spans, sentence_metadata)
        .map_err(PreparedScopeError::Resolution)?;
    Ok((resolution, labels))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            lookaround_divisions: None,
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

        assert!(resolved.admission.admits(&at_document(0)));
        assert!(!resolved.admission.admits(&at_document(7)));
    }

    #[test]
    fn a_document_with_no_stored_metadata_is_a_missing_field_exclusion() {
        let clause = ScopeClause::parse("@document.year >= 2010").unwrap();

        let resolved =
            resolve_scope(&clause, &[row(0, 0, 1, None)], &[], std::iter::empty()).unwrap();

        assert_eq!(resolved.documents.excluded_missing_field, 1);
    }

    #[test]
    fn unparseable_stored_json_fails_the_run_and_names_the_document() {
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
            lookaround_divisions: None,
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
