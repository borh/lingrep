use std::sync::Arc;

use lingrep_core::token_match::BoundProjection;
use lingrep_corpus::{CorpusArtifact, CorpusError, PreparedSentenceSource, ProvenanceView};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchScope {
    /// None admits every labelled sentence; an empty list admits none.
    pub included_sections: Option<Vec<String>>,
    pub include_unlabelled: bool,
}

impl Default for SearchScope {
    fn default() -> Self {
        Self {
            included_sections: None,
            include_unlabelled: true,
        }
    }
}

impl SearchScope {
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
        Ok(scope)
    }

    pub fn labels(&self) -> &[String] {
        self.included_sections.as_deref().unwrap_or_default()
    }

    pub fn is_unrestricted(&self) -> bool {
        self.included_sections.is_none() && self.include_unlabelled
    }
}

/// A search scope with the provenance view from which a scan reads sections
/// and division ids. The default is unrestricted and has no view, so its
/// sources record no division ids.
#[derive(Default)]
pub(crate) struct BoundScope {
    scope: SearchScope,
    provenance: Option<(Arc<ProvenanceView>, tokio::runtime::Handle)>,
}

impl BoundScope {
    /// Must be called inside a Tokio runtime, which runs the provenance
    /// queries.
    pub fn new(scope: SearchScope, provenance: Arc<ProvenanceView>) -> Self {
        Self {
            scope,
            provenance: Some((provenance, tokio::runtime::Handle::current())),
        }
    }

    /// Opens a source over the scope's sentences that records each
    /// sentence's innermost division of each of `division_kinds`.
    pub fn open_source(
        self,
        artifact: &CorpusArtifact,
        projection: &BoundProjection,
        division_kinds: Vec<compact_str::CompactString>,
    ) -> Result<PreparedSentenceSource, CorpusError> {
        let source = match &self.provenance {
            Some((provenance, runtime)) if !self.scope.is_unrestricted() => artifact
                .bound_sentence_source_with_sections(
                    projection,
                    Arc::clone(provenance),
                    runtime.clone(),
                    self.scope.included_sections,
                    self.scope.include_unlabelled,
                )?,
            _ => artifact.bound_sentence_source(projection)?,
        };
        match self.provenance {
            Some((provenance, runtime)) if !division_kinds.is_empty() => artifact
                .with_shared_lookaround_divisions(source, division_kinds, provenance, runtime),
            _ => Ok(source),
        }
    }

    pub fn specification(&self) -> SearchScope {
        self.scope.clone()
    }
}
