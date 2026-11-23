use std::sync::Arc;

use lingrep_core::execution::SentenceAdmission;
use lingrep_core::scope::ScopeClause;
use lingrep_core::token_match::BoundProjection;
use lingrep_corpus::{
    CorpusArtifact, CorpusError, PreparedScopeError, PreparedSentenceSource, ProvenanceView,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Section and `--where` clause filters for a non-document search scope.
///
/// Constructed through [`SearchScope::filter`] so an unrestricted filter
/// normalizes to [`SearchScope::WholeCorpus`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeFilter {
    /// `None` admits every labelled sentence; an empty list admits none.
    included_sections: Option<Vec<String>>,
    include_unlabelled: bool,
    /// A `--where` clause every admitted sentence must also satisfy.
    clause: Option<String>,
}

impl ScopeFilter {
    pub fn included_sections(&self) -> Option<&[String]> {
        self.included_sections.as_deref()
    }

    pub fn include_unlabelled(&self) -> bool {
        self.include_unlabelled
    }

    pub fn clause(&self) -> Option<&str> {
        self.clause.as_deref()
    }

    fn selects_every_section(&self) -> bool {
        self.included_sections.is_none() && self.include_unlabelled
    }

    /// The filter as one `--where` clause, given every section label the
    /// corpus records; `None` when every sentence is admitted.
    ///
    /// # Errors
    ///
    /// A selected or recorded label cannot be written as a clause value.
    pub fn where_clause(&self, all: &[String]) -> Result<Option<String>, UnwritableLabel> {
        fn set(labels: &[String]) -> Result<String, UnwritableLabel> {
            if labels.iter().any(|label| label.contains(['"', '\0']))
                || labels.iter().map(String::len).sum::<usize>() > 65_536
            {
                return Err(UnwritableLabel);
            }
            Ok(labels
                .iter()
                .map(|label| format!("\"{label}\""))
                .collect::<Vec<_>>()
                .join(", "))
        }
        let sections = if self.selects_every_section() {
            None
        } else if all.is_empty() {
            (!self.include_unlabelled).then(|| NOTHING.to_owned())
        } else {
            let selected = self.included_sections.as_deref().unwrap_or(all);
            let selected = (!selected.is_empty())
                .then(|| set(selected).map(|values| format!("@section in {{{values}}}")))
                .transpose()?;
            let unlabelled = self
                .include_unlabelled
                .then(|| set(all).map(|values| format!("@section not in {{{values}}}")))
                .transpose()?;
            Some(match (selected, unlabelled) {
                (Some(selected), Some(unlabelled)) => format!("({selected} | {unlabelled})"),
                (Some(clause), None) | (None, Some(clause)) => clause,
                (None, None) => NOTHING.to_owned(),
            })
        };
        Ok(match (sections, &self.clause) {
            (Some(sections), Some(clause)) => Some(format!("({sections}) & ({clause})")),
            (Some(clause), None) => Some(clause),
            (None, clause) => clause.clone(),
        })
    }
}

/// Which sentences of a corpus a search admits.
///
/// A single-document scope (`Document`) is mutually exclusive with section
/// selection and `--where` clauses (`Filtered`), and an unrestricted filter
/// always normalizes to `WholeCorpus`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SearchScope {
    /// Admits every sentence in the corpus.
    #[default]
    WholeCorpus,
    /// Admits sentences matching a section selection and/or a `--where` clause.
    Filtered(ScopeFilter),
    /// Admits only this document, by its corpus document ID.
    Document(u64),
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct WireSearchScope {
    included_sections: Option<Vec<String>>,
    include_unlabelled: bool,
    clause: Option<String>,
    document: Option<u64>,
}

impl Default for WireSearchScope {
    fn default() -> Self {
        Self {
            included_sections: None,
            include_unlabelled: true,
            clause: None,
            document: None,
        }
    }
}

#[derive(Serialize)]
struct WireSearchScopeRef<'a> {
    included_sections: Option<&'a [String]>,
    include_unlabelled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    clause: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    document: Option<u64>,
}

impl Serialize for SearchScope {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let wire = match self {
            Self::WholeCorpus => WireSearchScopeRef {
                included_sections: None,
                include_unlabelled: true,
                clause: None,
                document: None,
            },
            Self::Filtered(filter) => WireSearchScopeRef {
                included_sections: filter.included_sections.as_deref(),
                include_unlabelled: filter.include_unlabelled,
                clause: filter.clause.as_deref(),
                document: None,
            },
            Self::Document(document) => WireSearchScopeRef {
                included_sections: None,
                include_unlabelled: true,
                clause: None,
                document: Some(*document),
            },
        };
        wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SearchScope {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = WireSearchScope::deserialize(deserializer)?;
        let clause = wire
            .clause
            .map(|clause| clause.trim().to_owned())
            .filter(|clause| !clause.is_empty());
        let selects_every_section = wire.included_sections.is_none() && wire.include_unlabelled;
        if let Some(document) = wire.document {
            if !(selects_every_section && clause.is_none()) {
                return Err(serde::de::Error::custom("Invalid search scope."));
            }
            return Ok(Self::Document(document));
        }
        Ok(Self::filter(
            wire.included_sections,
            wire.include_unlabelled,
            clause,
        ))
    }
}

const NOTHING: &str = "@document.__lingrep_empty_scope = x & @document.__lingrep_empty_scope != x";

impl SearchScope {
    /// Builds a section/clause scope, normalizing an unrestricted filter to
    /// [`Self::WholeCorpus`].
    #[must_use]
    pub fn filter(
        included_sections: Option<Vec<String>>,
        include_unlabelled: bool,
        clause: Option<String>,
    ) -> Self {
        if included_sections.is_none() && include_unlabelled && clause.is_none() {
            Self::WholeCorpus
        } else {
            Self::Filtered(ScopeFilter {
                included_sections,
                include_unlabelled,
                clause,
            })
        }
    }

    /// # Errors
    ///
    /// A message for the search form: the JSON is malformed, a label is out of
    /// bounds, `clause` is not a valid scope clause, or `document` is combined
    /// with sections or a clause.
    pub fn parse(encoded: Option<&str>) -> Result<Self, String> {
        let mut wire: WireSearchScope = match encoded {
            Some(encoded) => serde_json::from_str(encoded).map_err(|_| "Invalid search scope.")?,
            None => WireSearchScope::default(),
        };
        if let Some(labels) = &mut wire.included_sections {
            if labels.len() > lingrep_corpus::MAX_SECTION_LABELS
                || labels
                    .iter()
                    .any(|label| label.is_empty() || label.len() > 65_536)
            {
                return Err("Invalid section selection.".into());
            }
            labels.sort();
            labels.dedup();
        }
        wire.clause = wire
            .clause
            .map(|clause| clause.trim().to_owned())
            .filter(|clause| !clause.is_empty());
        if let Some(clause) = &wire.clause {
            if clause.len() > 65_536 {
                return Err("Invalid search scope.".into());
            }
            ScopeClause::parse(clause).map_err(|error| {
                format!(
                    "The scope clause is not valid at byte {}: {error}",
                    error.byte_range.start
                )
            })?;
        }
        let selects_every_section = wire.included_sections.is_none() && wire.include_unlabelled;
        if let Some(document) = wire.document {
            if !(selects_every_section && wire.clause.is_none()) {
                return Err("Invalid search scope.".into());
            }
            return Ok(Self::Document(document));
        }
        Ok(Self::filter(
            wire.included_sections,
            wire.include_unlabelled,
            wire.clause,
        ))
    }

    pub fn included_sections(&self) -> Option<&[String]> {
        match self {
            Self::Filtered(filter) => filter.included_sections(),
            Self::WholeCorpus | Self::Document(_) => None,
        }
    }

    pub fn include_unlabelled(&self) -> bool {
        match self {
            Self::Filtered(filter) => filter.include_unlabelled(),
            Self::WholeCorpus | Self::Document(_) => true,
        }
    }

    pub fn clause(&self) -> Option<&str> {
        match self {
            Self::Filtered(filter) => filter.clause(),
            Self::WholeCorpus | Self::Document(_) => None,
        }
    }

    pub fn document(&self) -> Option<u64> {
        match self {
            Self::Document(document) => Some(*document),
            Self::WholeCorpus | Self::Filtered(_) => None,
        }
    }

    pub fn labels(&self) -> &[String] {
        self.included_sections().unwrap_or_default()
    }

    pub fn is_unrestricted(&self) -> bool {
        matches!(self, Self::WholeCorpus)
    }

    pub fn selects_every_section(&self) -> bool {
        match self {
            Self::Filtered(filter) => filter.selects_every_section(),
            Self::WholeCorpus | Self::Document(_) => true,
        }
    }

    /// The scope as one `--where` clause, given every section label the
    /// corpus records; `None` when every sentence is admitted.
    ///
    /// # Errors
    ///
    /// A selected or recorded label cannot be written as a clause value.
    pub fn where_clause(&self, all: &[String]) -> Result<Option<String>, UnwritableLabel> {
        match self {
            Self::Filtered(filter) => filter.where_clause(all),
            Self::WholeCorpus | Self::Document(_) => Ok(None),
        }
    }
}

/// A section label holds a `"` or NUL, or the labels exceed the clause bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnwritableLabel;

#[derive(Debug, Clone, Default)]
pub struct ResolvedScope {
    /// The scope as the `--where` clause that was resolved; `None` admits all.
    pub clause: Option<String>,
    pub admission: SentenceAdmission,
    pub excluded_sentences: u64,
}

#[derive(Debug)]
pub(crate) enum ResolveError {
    Unanswerable(String),
    Corpus(String),
}

/// Call on a blocking thread.
pub(crate) fn resolve(
    scope: &SearchScope,
    provenance: &ProvenanceView,
    runtime: &tokio::runtime::Handle,
) -> Result<ResolvedScope, ResolveError> {
    let filter = match scope {
        SearchScope::WholeCorpus => return Ok(ResolvedScope::default()),
        SearchScope::Document(document) => {
            return resolve_document(*document, provenance, runtime);
        }
        SearchScope::Filtered(filter) => filter,
    };
    let all = runtime
        .block_on(provenance.section_coverage(lingrep_corpus::MAX_SECTION_LABELS))
        .map_err(|error| ResolveError::Corpus(error.to_string()))?
        .into_iter()
        .map(|coverage| coverage.label)
        .collect::<Vec<_>>();
    let Some(clause) = filter.where_clause(&all).map_err(|UnwritableLabel| {
        ResolveError::Unanswerable(
            "A section label in this corpus cannot be written in a scope clause.".into(),
        )
    })?
    else {
        return Ok(ResolvedScope::default());
    };
    let parsed = ScopeClause::parse(&clause).map_err(|error| {
        ResolveError::Unanswerable(format!("The scope clause is not valid: {error}"))
    })?;
    let (resolution, _) = lingrep_corpus::resolve_prepared_scope(provenance, runtime, &parsed)
        .map_err(|error| match error {
            PreparedScopeError::NoSectionLabels => ResolveError::Unanswerable(
                "The scope clause names `@section`, but this corpus records no section labels."
                    .into(),
            ),
            PreparedScopeError::Corpus(error) => ResolveError::Corpus(error.to_string()),
            PreparedScopeError::Resolution(error) => ResolveError::Corpus(error.message),
        })?;
    let sentences = &resolution.sentences;
    let excluded_sentences = sentences
        .total()
        .map_err(|_| ResolveError::Corpus("scope accounting overflowed".into()))?
        - sentences.admitted;
    Ok(ResolvedScope {
        clause: Some(clause),
        admission: resolution.admission,
        excluded_sentences,
    })
}

fn resolve_document(
    document: u64,
    provenance: &ProvenanceView,
    runtime: &tokio::runtime::Handle,
) -> Result<ResolvedScope, ResolveError> {
    let found = runtime.block_on(crate::provenance_view::for_each_document_metadata_page(
        provenance,
        |error| match error {
            crate::provenance_view::MetadataPageError::Corpus(error) => {
                ResolveError::Corpus(error.to_string())
            }
            crate::provenance_view::MetadataPageError::NonMonotonicCursor => {
                ResolveError::Unanswerable("This corpus has no such document.".into())
            }
        },
        |rows| {
            if let Some(row) = rows.iter().find(|row| row.document_id.get() == document) {
                return Ok(std::ops::ControlFlow::Break(ResolvedScope {
                    clause: None,
                    admission: SentenceAdmission::Documents(std::iter::once(document).collect()),
                    excluded_sentences: provenance.sentence_count() - row.sentence_count,
                }));
            }
            Ok(std::ops::ControlFlow::Continue(()))
        },
    ))?;
    found.ok_or_else(|| ResolveError::Unanswerable("This corpus has no such document.".into()))
}

#[derive(Default)]
pub(crate) struct BoundScope {
    admission: SentenceAdmission,
    provenance: Option<(Arc<ProvenanceView>, tokio::runtime::Handle)>,
}

impl BoundScope {
    /// Must be called inside a Tokio runtime, which runs the provenance
    /// queries.
    pub fn new(admission: SentenceAdmission, provenance: Arc<ProvenanceView>) -> Self {
        Self {
            admission,
            provenance: Some((provenance, tokio::runtime::Handle::current())),
        }
    }

    /// Opens a source over the admitted sentences that records each
    /// sentence's innermost division of each of `division_kinds`.
    pub fn open_source(
        self,
        artifact: &CorpusArtifact,
        projection: &BoundProjection,
        division_kinds: Vec<compact_str::CompactString>,
        decode_pool: Arc<rayon::ThreadPool>,
    ) -> Result<PreparedSentenceSource, CorpusError> {
        let source = artifact
            .bound_sentence_source(projection)?
            .with_decode_pool(decode_pool)
            .with_admission(self.admission);
        match self.provenance {
            Some((provenance, runtime)) if !division_kinds.is_empty() => artifact
                .with_shared_lookaround_divisions(source, division_kinds, provenance, runtime),
            _ => Ok(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SearchScope;

    #[test]
    fn a_document_scope_stands_alone() {
        let document = SearchScope::parse(Some(r#"{"document":5}"#)).unwrap();
        assert_eq!(document, SearchScope::Document(5));
        assert_eq!(document.document(), Some(5));
        assert!(!document.is_unrestricted());
        for combined in [
            r#"{"document":5,"clause":"@document.genre = x"}"#,
            r#"{"document":5,"included_sections":["a"]}"#,
            r#"{"document":5,"include_unlabelled":false}"#,
        ] {
            assert!(SearchScope::parse(Some(combined)).is_err(), "{combined}");
            assert!(
                serde_json::from_str::<SearchScope>(combined).is_err(),
                "serde Deserialize must also reject {combined}"
            );
        }
    }

    #[test]
    fn unrestricted_filters_normalize_to_whole_corpus_and_preserve_wire_json() {
        assert_eq!(
            SearchScope::filter(None, true, None),
            SearchScope::WholeCorpus
        );
        assert_eq!(SearchScope::parse(None).unwrap(), SearchScope::WholeCorpus);
        assert_eq!(
            SearchScope::parse(Some(
                r#"{"included_sections":null,"include_unlabelled":true}"#
            ))
            .unwrap(),
            SearchScope::WholeCorpus
        );
        assert_eq!(
            serde_json::to_string(&SearchScope::WholeCorpus).unwrap(),
            r#"{"included_sections":null,"include_unlabelled":true}"#
        );
        let filtered = SearchScope::parse(Some(
            r#"{"included_sections":["abstract"],"include_unlabelled":true}"#,
        ))
        .unwrap();
        assert_eq!(
            serde_json::to_string(&filtered).unwrap(),
            r#"{"included_sections":["abstract"],"include_unlabelled":true}"#
        );
        let document = SearchScope::Document(5);
        assert_eq!(
            serde_json::to_string(&document).unwrap(),
            r#"{"included_sections":null,"include_unlabelled":true,"document":5}"#
        );
        assert_eq!(
            serde_json::from_str::<SearchScope>(&serde_json::to_string(&document).unwrap())
                .unwrap(),
            document
        );
    }
}
