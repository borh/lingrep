use std::{collections::HashMap, sync::Arc};

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use lingrep_core::execution::{
    ExecutionBatch, ExecutionLimits, OversizedSentenceOmissions, SentenceBatchSource,
    TokenizedAdmission, explain_in_windows,
};
use lingrep_core::fsm::{BoundCatalog, CompiledCatalog, SentenceExplanation};
use lingrep_core::token_match::TokenInfo;

use crate::{
    corpus_runtime::ReadyCorpus,
    inspection::OverlapCandidate,
    search_scope::{BoundScope, SearchScope},
    snapshot::{CompletedSnapshot, MatchRef, PatternKey},
    state::RequestStore,
};

#[allow(clippy::result_large_err)]
pub(super) async fn inspect(
    store: &RequestStore,
    ready: &ReadyCorpus,
    snapshot: Arc<CompletedSnapshot>,
    key: PatternKey,
    refs: &[&MatchRef],
    sentences: HashMap<u64, Option<Vec<TokenInfo>>>,
) -> Result<Vec<Vec<OverlapCandidate>>, Response> {
    let Some(pattern) = snapshot.pattern(key) else {
        return Err(super::inspection_response(
            super::EndpointInspectionError::UnknownPattern,
        ));
    };
    if pattern.components().is_some() || refs.is_empty() {
        return Ok(vec![Vec::new(); refs.len()]);
    }
    let name = pattern.name.clone();
    let references = refs
        .iter()
        .map(|reference| (reference.sentence_index, reference.span.clone()))
        .collect::<Vec<_>>();
    let ready = ready.clone();
    let permit = store.coordinators.acquire_scan_permit().await;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let compiled = crate::patterns::compile_web_pattern(&snapshot.source)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
        let windows = if compiled.matcher.lookaround_reach().is_empty() {
            None
        } else {
            Some(ScanWindows::new(
                &compiled.matcher,
                &ready,
                &snapshot.search_scope,
            )?)
        };
        let mut explanations = HashMap::new();
        references
            .into_iter()
            .map(|(sentence_index, span)| {
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    explanations.entry(sentence_index)
                {
                    let Some(Some(tokens)) = sentences.get(&sentence_index) else {
                        return Err(super::invalid_provenance_response());
                    };
                    let explanation = match &windows {
                        Some(windows) => windows.explain(sentence_index)?,
                        None => compiled
                            .matcher
                            .explain_sentence(tokens)
                            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?,
                    };
                    entry.insert(explanation);
                }
                Ok(crate::inspection::overlapping_candidates(
                    &explanations[&sentence_index],
                    &name,
                    span,
                ))
            })
            .collect()
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?
}

struct ScanWindows {
    bound: BoundCatalog,
    ready: ReadyCorpus,
    scope: SearchScope,
    provenance: Arc<lingrep_corpus::ProvenanceView>,
    division_kinds: Vec<compact_str::CompactString>,
}

impl ScanWindows {
    #[allow(clippy::result_large_err)]
    fn new(
        catalog: &CompiledCatalog,
        ready: &ReadyCorpus,
        scope: &SearchScope,
    ) -> Result<Self, Response> {
        let corrupt =
            || super::inspection_response(super::EndpointInspectionError::CacheCorruption);
        let schema = ready.artifact.token_schema().map_err(|_| corrupt())?;
        let bound = catalog
            .bind_with_projection(&schema, catalog.execution_attributes())
            .map_err(|_| corrupt())?;
        let provenance = tokio::runtime::Handle::current()
            .block_on(ready.provenance())
            .map_err(|_| corrupt())?;
        Ok(Self {
            bound,
            ready: ready.clone(),
            scope: scope.clone(),
            provenance,
            division_kinds: catalog.lookaround_division_kinds(),
        })
    }

    #[allow(clippy::result_large_err)]
    fn explain(&self, ordinal: u64) -> Result<SentenceExplanation, Response> {
        let corrupt =
            || super::inspection_response(super::EndpointInspectionError::CacheCorruption);
        let count = self.ready.artifact.sentence_count();
        let mut margin = 4_u64;
        loop {
            let range =
                ordinal.saturating_sub(margin)..ordinal.saturating_add(margin + 1).min(count);
            let mut source = BoundScope::new(self.scope.clone(), Arc::clone(&self.provenance))
                .open_source(
                    &self.ready.artifact,
                    self.bound.projection(),
                    self.division_kinds.clone(),
                )
                .map_err(|_| corrupt())?
                .with_ordinal_range(range.clone())
                .with_tokenized_admission(TokenizedAdmission::Omit(Arc::new(
                    std::sync::Mutex::new(OversizedSentenceOmissions::default()),
                )));
            let (mut locations, mut inputs) = (Vec::new(), Vec::new());
            while let Some(batch) = source
                .next_batch(&ExecutionLimits::default())
                .map_err(|_| corrupt())?
            {
                let ExecutionBatch::BoundTokenized { sentences, .. } = batch else {
                    return Err(corrupt());
                };
                for sentence in sentences {
                    locations.push(sentence.location);
                    inputs.push(sentence.value);
                }
            }
            let Some(target) = locations
                .iter()
                .position(|location| location.corpus_sentence_ordinal == ordinal)
            else {
                return Err(super::invalid_provenance_response());
            };
            let explanation = explain_in_windows(
                &self.bound,
                &locations,
                &inputs,
                target,
                range.start > 0,
                range.end < count,
            )
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
            if let Some(explanation) = explanation {
                return Ok(explanation);
            }
            margin = margin.saturating_mul(2);
        }
    }
}
