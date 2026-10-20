//! Shared scope parsing, prepared resolution, and completion reporting.

use std::collections::BTreeSet;

use lingrep_core::corpus::ScopeObservation;
use lingrep_core::scope::ScopeClause;
use lingrep_corpus::BlockingProvenance;

use crate::error::CliError;
use crate::protocol::query::{ScopeBasis, ScopeReport, ScopeUniverse};
use crate::scope_precompute::{ScopeResolution, ScopeResolutionError, resolve_scope};
use crate::section_diagnostics::refuse_unanswerable;

/// Parses a `--where` clause, reporting the offending byte.
pub(crate) fn parse_scope(clause: Option<&str>) -> Result<Option<ScopeClause>, CliError> {
    clause
        .map(|source| {
            ScopeClause::parse(source).map_err(|error| {
                CliError::InvalidArgs(format!(
                    "--where is not a valid scope clause at byte {}: {error}",
                    error.byte_range.start
                ))
            })
        })
        .transpose()
}

/// Resolves a clause against a prepared artifact's document metadata.
///
/// Also returns the labels the artifact records, so the caller can report the
/// ones the clause named and the corpus does not have. An artifact knows its
/// whole label set up front, unlike a stream.
pub(crate) fn resolve_prepared_scope(
    reader: &BlockingProvenance,
    clause: &ScopeClause,
) -> Result<(ScopeResolution, BTreeSet<String>), CliError> {
    let runtime = reader;
    let (view, spans, labels) = runtime.block_on(async {
        let view = reader.view();
        // Both are skipped entirely for a metadata-only clause: `section_labels`
        // is empty, so no region query is issued and the prepared path costs
        // what it did before section scope existed.
        let (spans, labels) = if clause.names_sections() {
            (
                view.region_spans_for(&clause.section_labels()).await?,
                view.section_coverage(lingrep_corpus::MAX_SECTION_LABELS)
                    .await?
                    .into_iter()
                    .map(|coverage| coverage.label)
                    .collect(),
            )
        } else {
            (Vec::new(), BTreeSet::new())
        };
        Ok::<_, lingrep_corpus::CorpusError>((view, spans, labels))
    })?;
    refuse_unanswerable(clause, !labels.is_empty(), "this artifact")?;
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
    let resolution =
        resolve_scope(clause, documents, &spans, sentence_metadata).map_err(|error| {
            CliError::Corpus(match error.document_id {
                Some(id) => format!("document {}: {}", id.get(), error.message),
                None => error.message,
            })
        })?;
    Ok((resolution, labels))
}

/// Builds the scope report a completion record carries.
///
/// The `basis` is the load-bearing field. A prepared run's universe is
/// precomputed from the whole artifact and does not shrink when the run stops
/// early, so it stays `Corpus`. A direct run's counts are whatever its reader
/// observed — and the reader crosses document boundaries to fill a batch while
/// the runner can stop inside an earlier partition, so once a run is limited
/// those counts are neither the corpus nor the matched prefix.
pub(crate) fn build_scope_report(
    clause: Option<&ScopeClause>,
    prepared: Option<&ScopeResolution>,
    observation: &ScopeObservation,
    limited: bool,
) -> Result<Option<ScopeReport>, CliError> {
    let Some(clause) = clause else {
        return Ok(None);
    };
    if let Some(resolved) = prepared {
        return Ok(Some(ScopeReport {
            clause: clause.source().to_owned(),
            basis: ScopeBasis::Corpus,
            documents: resolved.documents.clone(),
            sentences: resolved.sentences.clone(),
        }));
    }
    let counts = observation.counts().map_err(CliError::Io)?;
    Ok(Some(ScopeReport {
        clause: clause.source().to_owned(),
        basis: if limited {
            ScopeBasis::ObservedPrefix
        } else {
            ScopeBasis::Corpus
        },
        documents: ScopeUniverse {
            admitted: counts.documents_admitted,
            excluded_by_predicate: counts.documents_excluded_by_predicate,
            excluded_missing_field: counts.documents_excluded_missing_field,
            excluded_shape_mismatch: counts.documents_excluded_shape_mismatch,
        },
        sentences: ScopeUniverse {
            admitted: counts.sentences_admitted,
            excluded_by_predicate: counts.sentences_excluded_by_predicate,
            excluded_missing_field: counts.sentences_excluded_missing_field,
            excluded_shape_mismatch: counts.sentences_excluded_shape_mismatch,
        },
    }))
}
