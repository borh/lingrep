//! One proof boundary for query corpus completion and identity publication.

use lingrep_core::execution::ExecutionError;
use lingrep_corpus::{AnalysisInputs, derive_analysis_identity};

use super::query_support::protocol_inventory;
use crate::direct_source::{
    FilesystemObservation, StdinObservation, encode_digest, validate_ordered_inventory,
};
use crate::error::CliError;
use crate::protocol::query::{
    CompleteCorpus, DirectCompletion, DirectRunState, InventoryStats, RunCorpus,
};

pub(crate) enum CompletionObservation {
    Filesystem {
        observation: FilesystemObservation,
        analysis: AnalysisInputs,
    },
    Stdin {
        stream_id: String,
        observation: StdinObservation,
    },
    Prepared {
        source_identity: String,
        analysis_identity: String,
        inventory: InventoryStats,
    },
}

impl CompletionObservation {
    /// The corpus facts available before execution starts.
    pub(crate) fn run_corpus(&self) -> RunCorpus {
        match self {
            Self::Filesystem { .. } => RunCorpus::Direct {
                state: DirectRunState::Pending,
            },
            Self::Stdin { stream_id, .. } => RunCorpus::Stdin {
                stream_id: stream_id.clone(),
            },
            Self::Prepared {
                source_identity,
                analysis_identity,
                ..
            } => RunCorpus::Prepared {
                source_identity: source_identity.clone(),
                analysis_identity: analysis_identity.clone(),
            },
        }
    }

    /// Inventory is known before execution only for an admitted artifact.
    pub(crate) fn run_inventory(&self) -> Option<InventoryStats> {
        match self {
            Self::Prepared { inventory, .. } => Some(inventory.clone()),
            Self::Filesystem { .. } | Self::Stdin { .. } => None,
        }
    }

    pub(crate) fn complete(
        self,
        filesystem: FilesystemCompletion,
    ) -> Result<(CompleteCorpus, Option<InventoryStats>), CliError> {
        match self {
            Self::Stdin {
                stream_id,
                observation,
            } => Ok((
                CompleteCorpus::Stdin {
                    stream_id,
                    observed_content_digest: match filesystem {
                        FilesystemCompletion::Full => observation.observed_content_digest(),
                        FilesystemCompletion::PrefixUnavailable => None,
                    },
                },
                None,
            )),
            Self::Filesystem { .. }
                if matches!(filesystem, FilesystemCompletion::PrefixUnavailable) =>
            {
                Ok((
                    CompleteCorpus::Direct {
                        completion: DirectCompletion::PrefixUnavailable,
                    },
                    None,
                ))
            }
            Self::Filesystem {
                observation,
                analysis,
            } => {
                let discovery = observation.discovery_report().ok_or_else(|| {
                    CliError::Corpus("filesystem discovery did not reach clean EOF".into())
                })?;
                let observed = observation
                    .source
                    .finish()
                    .map_err(|error| CliError::Corpus(error.to_string()))?;
                validate_ordered_inventory(&observation.inventory, &discovery)?;
                if observed.document_count() != discovery.inventory.documents {
                    return Err(ExecutionError::InventoryMismatch.into());
                }
                Ok((
                    CompleteCorpus::Direct {
                        completion: DirectCompletion::Known {
                            source_identity: encode_digest(observed.source_identity().as_bytes()),
                            analysis_identity: encode_digest(
                                derive_analysis_identity(&observed, &analysis).as_bytes(),
                            ),
                        },
                    },
                    Some(protocol_inventory(discovery.inventory)),
                ))
            }
            Self::Prepared {
                source_identity,
                analysis_identity,
                inventory,
            } => Ok((
                CompleteCorpus::Prepared {
                    source_identity,
                    analysis_identity,
                },
                Some(inventory),
            )),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum FilesystemCompletion {
    Full,
    PrefixUnavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_run_and_completion_share_identity_and_inventory() {
        let inventory = InventoryStats {
            documents: 3,
            ..InventoryStats::default()
        };
        let observation = CompletionObservation::Prepared {
            source_identity: "source".into(),
            analysis_identity: "analysis".into(),
            inventory: inventory.clone(),
        };
        assert_eq!(
            observation.run_corpus(),
            RunCorpus::Prepared {
                source_identity: "source".into(),
                analysis_identity: "analysis".into(),
            }
        );
        assert_eq!(observation.run_inventory(), Some(inventory.clone()));
        assert_eq!(
            observation
                .complete(FilesystemCompletion::Full)
                .expect("prepared completion is infallible"),
            (
                CompleteCorpus::Prepared {
                    source_identity: "source".into(),
                    analysis_identity: "analysis".into(),
                },
                Some(inventory),
            )
        );
    }
}
