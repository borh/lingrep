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

#[derive(Default)]
pub(crate) enum BoundScope {
    #[default]
    All,
    Restricted {
        scope: SearchScope,
        provenance: Arc<ProvenanceView>,
        runtime: tokio::runtime::Handle,
    },
}

impl BoundScope {
    pub fn new(scope: SearchScope, provenance: Arc<ProvenanceView>) -> Self {
        if scope.is_unrestricted() {
            Self::All
        } else {
            Self::Restricted {
                scope,
                provenance,
                runtime: tokio::runtime::Handle::current(),
            }
        }
    }

    pub fn open_source(
        self,
        artifact: &CorpusArtifact,
        projection: &BoundProjection,
    ) -> Result<PreparedSentenceSource, CorpusError> {
        match self {
            Self::All => artifact.bound_sentence_source(projection),
            Self::Restricted {
                scope,
                provenance,
                runtime,
            } => artifact.bound_sentence_source_with_sections(
                projection,
                provenance,
                runtime,
                scope.included_sections,
                scope.include_unlabelled,
            ),
        }
    }

    pub fn specification(&self) -> SearchScope {
        match self {
            Self::All => SearchScope::default(),
            Self::Restricted { scope, .. } => scope.clone(),
        }
    }
}
