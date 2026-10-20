use std::{collections::BTreeMap, sync::Arc};

use lingrep_core::{
    corpus::SentenceId,
    token_match::{TokenAttributeIndex, TokenInfo},
};
use lingrep_corpus::{CorpusArtifact, CorpusError, ProvenanceView, SentenceProvenance};
use thiserror::Error;

use crate::corpus_runtime::{BoundCorpusReader, CorpusIdentity, CorpusReadError, ReadyCorpus};

pub const DEFAULT_RESULT_PAGE_SIZE: usize = 100;
pub const RESULT_PAGE_SIZE_CHOICES: [usize; 4] = [50, 100, 250, 500];
pub const MAX_RESULT_PAGE_SIZE: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedSentence {
    pub provenance: SentenceProvenance,
    pub tokens: Arc<[TokenInfo]>,
}

#[derive(Debug, Error)]
pub enum PageProjectionError {
    #[error("the result page size is invalid")]
    InvalidPageSize,
    #[error("the corpus revision is stale")]
    StaleCorpus,
    #[error("the corpus page is inconsistent")]
    InconsistentArtifacts,
    #[error("the corpus cache could not be decoded")]
    CacheCorruption,
    #[error("the page projection task failed")]
    Task,
    #[error("the corpus provenance could not be read")]
    Provenance(#[source] CorpusError),
}

/// Request-scoped projection over one immutable ready corpus snapshot.
///
/// A handler constructs this after acquiring its result lease and drops it at
/// the end of the HTTP request; it is never retained in `RequestStore`.
pub struct CorpusPageProjector {
    reader: BoundCorpusReader,
    artifact: Arc<CorpusArtifact>,
    provenance: Arc<ProvenanceView>,
}

impl CorpusPageProjector {
    pub async fn bind(
        ready: ReadyCorpus,
        expected_identity: &CorpusIdentity,
    ) -> Result<Self, PageProjectionError> {
        if &ready.identity != expected_identity {
            return Err(PageProjectionError::StaleCorpus);
        }
        let provenance = ready
            .provenance()
            .await
            .map_err(PageProjectionError::Provenance)?;
        let reader = ready
            .reader()
            .map_err(|_| PageProjectionError::CacheCorruption)?
            .bind(expected_identity)
            .ok_or(PageProjectionError::StaleCorpus)?;
        Ok(Self {
            reader,
            artifact: ready.artifact,
            provenance,
        })
    }

    pub async fn project(
        &self,
        sentence_ids: &[SentenceId],
    ) -> Result<Vec<ProjectedSentence>, PageProjectionError> {
        if sentence_ids.len() > MAX_RESULT_PAGE_SIZE {
            return Err(PageProjectionError::InvalidPageSize);
        }
        if sentence_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut unique = sentence_ids.to_vec();
        unique.sort_unstable();
        unique.dedup();
        let cache_ids = unique.iter().map(|id| id.get()).collect::<Vec<_>>();
        let provenance_ids = cache_ids.clone();
        let reader = self.reader.clone();

        let cache_task = tokio::task::spawn_blocking(move || {
            reader.decode_sentences_with_attributes(&cache_ids, &[TokenAttributeIndex::Orth])
        });
        let provenance_task = self
            .artifact
            .sentence_provenance_page(&self.provenance, &provenance_ids);
        let (cache, provenance) = tokio::join!(cache_task, provenance_task);
        let cache = match cache {
            Ok(Ok(rows)) => rows,
            Ok(Err(CorpusReadError::Corruption(_))) => {
                return Err(PageProjectionError::CacheCorruption);
            }
            Err(_) => return Err(PageProjectionError::Task),
        };
        let provenance = provenance.map_err(PageProjectionError::Provenance)?;

        let mut selected = BTreeMap::new();
        for ((id, provenance), tokens) in unique.into_iter().zip(provenance).zip(cache) {
            let Some(provenance) = provenance else {
                return Err(PageProjectionError::InconsistentArtifacts);
            };
            let Some(tokens) = tokens else {
                // Either missing side violates the published corpus artifact pair.
                return Err(PageProjectionError::InconsistentArtifacts);
            };
            selected.insert(id, (provenance, Arc::<[TokenInfo]>::from(tokens)));
        }

        let mut output = Vec::with_capacity(sentence_ids.len());
        for id in sentence_ids {
            let Some((provenance, tokens)) = selected.get(id) else {
                return Err(PageProjectionError::InconsistentArtifacts);
            };
            output.push(ProjectedSentence {
                provenance: provenance.clone(),
                tokens: Arc::clone(tokens),
            });
        }
        Ok(output)
    }
}
