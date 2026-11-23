//! Shared scope parsing, prepared resolution, and completion reporting.

use std::collections::BTreeSet;

use lingrep_core::corpus::ScopeCounts;
use lingrep_core::scope::ScopeClause;
use lingrep_corpus::BlockingProvenance;

use crate::error::CliError;
use crate::protocol::query::{ScopeBasis, ScopeReport, ScopeUniverse};
use crate::section_diagnostics::refuse_unanswerable;
use lingrep_corpus::{PreparedScopeError, ScopeResolution};

/// Parses a `--where` clause, reporting the offending byte.
pub(crate) fn parse_scope(clause: Option<&str>) -> Result<Option<ScopeClause>, CliError> {
    parse_scope_flag("--where", clause)
}

/// [`parse_scope`] for a clause given to `flag`, which the error names.
pub(crate) fn parse_scope_flag(
    flag: &str,
    clause: Option<&str>,
) -> Result<Option<ScopeClause>, CliError> {
    clause
        .map(|source| {
            ScopeClause::parse(source).map_err(|error| {
                CliError::InvalidArgs(format!(
                    "{flag} is not a valid scope clause at byte {}: {error}",
                    error.byte_range.start
                ))
            })
        })
        .transpose()
}

/// Resolves a prepared-corpus clause and returns recorded section labels for diagnostics.
pub(crate) fn resolve_prepared_scope(
    reader: &BlockingProvenance,
    clause: &ScopeClause,
) -> Result<(ScopeResolution, BTreeSet<String>), CliError> {
    lingrep_corpus::resolve_prepared_scope(reader.view(), reader, clause).map_err(|error| {
        match error {
            PreparedScopeError::NoSectionLabels => {
                refuse_unanswerable(clause, false, "this artifact").expect_err("names sections")
            }
            PreparedScopeError::Corpus(error) => error.into(),
            PreparedScopeError::Resolution(error) => CliError::Corpus(match error.document_id {
                Some(id) => format!("document {}: {}", id.get(), error.message),
                None => error.message,
            }),
        }
    })
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
    counts: ScopeCounts,
    limited: bool,
) -> Option<ScopeReport> {
    let clause = clause?;
    if let Some(resolved) = prepared {
        return Some(ScopeReport {
            clause: clause.source().to_owned(),
            basis: ScopeBasis::Corpus,
            documents: resolved.documents.clone(),
            sentences: resolved.sentences.clone(),
        });
    }
    Some(ScopeReport {
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
    })
}

/// Each document's ID, corpus sentence range and values of `key`
/// (`@document.NAME`) as `--where` compares them, in corpus order; a document
/// with no value lists none.
pub(crate) fn document_values(
    provenance: &lingrep_corpus::BlockingProvenance,
    key: &str,
) -> Result<Vec<(u64, std::ops::Range<u64>, Vec<String>)>, CliError> {
    use lingrep_core::corpus::{MetadataScalar, MetadataValue};
    let name = key
        .strip_prefix("@document.")
        .expect("--by-metadata names a @document key");
    let mut documents = Vec::new();
    let mut cursor = 0;
    loop {
        let page = provenance.block_on(provenance.view().document_metadata_page(cursor))?;
        if page.is_empty() {
            return Ok(documents);
        }
        for row in page {
            let end = row.first_sentence + row.sentence_count;
            cursor = end;
            let metadata = lingrep_corpus::scope_resolution::document_metadata(&row)
                .map_err(|error| CliError::Corpus(error.message))?;
            let values = match metadata.iter().find(|(candidate, _)| candidate == name) {
                None => Vec::new(),
                Some((_, MetadataValue::Scalar(scalar))) => vec![scalar.comparison_text()],
                Some((_, MetadataValue::Scalars(list))) => {
                    list.iter().map(MetadataScalar::comparison_text).collect()
                }
                Some((_, MetadataValue::Opaque(_))) => {
                    return Err(CliError::InvalidArgs(format!(
                        "--by-metadata {key}: document {} has a value that is neither a scalar \
                         nor a list of scalars",
                        row.document_id.get()
                    )));
                }
            };
            documents.push((row.document_id.get(), row.first_sentence..end, values));
        }
    }
}
