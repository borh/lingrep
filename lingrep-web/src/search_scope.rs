use std::sync::Arc;

use lingrep_core::execution::SentenceAdmission;
use lingrep_core::scope::ScopeClause;
use lingrep_core::token_match::BoundProjection;
use lingrep_corpus::{
    CorpusArtifact, CorpusError, PreparedScopeError, PreparedSentenceSource, ProvenanceView,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchScope {
    /// None admits every labelled sentence; an empty list admits none.
    pub included_sections: Option<Vec<String>>,
    pub include_unlabelled: bool,
    /// A `--where` clause every admitted sentence must also satisfy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clause: Option<String>,
}

impl Default for SearchScope {
    fn default() -> Self {
        Self {
            included_sections: None,
            include_unlabelled: true,
            clause: None,
        }
    }
}

const NOTHING: &str = "@document.__lingrep_empty_scope = x & @document.__lingrep_empty_scope != x";

impl SearchScope {
    /// # Errors
    ///
    /// A message for the search form: the JSON is malformed, a label is out of
    /// bounds, or `clause` is not a valid scope clause.
    pub fn parse(encoded: Option<&str>) -> Result<Self, String> {
        let mut scope: Self = match encoded {
            Some(encoded) => serde_json::from_str(encoded).map_err(|_| "Invalid search scope.")?,
            None => Self::default(),
        };
        if let Some(labels) = &mut scope.included_sections {
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
        scope.clause = scope
            .clause
            .map(|clause| clause.trim().to_owned())
            .filter(|clause| !clause.is_empty());
        if let Some(clause) = &scope.clause {
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
        Ok(scope)
    }

    pub fn labels(&self) -> &[String] {
        self.included_sections.as_deref().unwrap_or_default()
    }

    pub fn is_unrestricted(&self) -> bool {
        self.selects_every_section() && self.clause.is_none()
    }

    fn selects_every_section(&self) -> bool {
        self.included_sections.is_none() && self.include_unlabelled
    }

    /// The scope as one `--where` clause, given every section label the
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
    if scope.is_unrestricted() {
        return Ok(ResolvedScope::default());
    }
    let all = runtime
        .block_on(provenance.section_coverage(lingrep_corpus::MAX_SECTION_LABELS))
        .map_err(|error| ResolveError::Corpus(error.to_string()))?
        .into_iter()
        .map(|coverage| coverage.label)
        .collect::<Vec<_>>();
    let Some(clause) = scope.where_clause(&all).map_err(|UnwritableLabel| {
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
    ) -> Result<PreparedSentenceSource, CorpusError> {
        let source = artifact
            .bound_sentence_source(projection)?
            .with_admission(self.admission);
        match self.provenance {
            Some((provenance, runtime)) if !division_kinds.is_empty() => artifact
                .with_shared_lookaround_divisions(source, division_kinds, provenance, runtime),
            _ => Ok(source),
        }
    }
}
