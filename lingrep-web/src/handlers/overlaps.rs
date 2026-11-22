use std::{collections::HashMap, sync::Arc};

use axum::response::{IntoResponse, Response};
use lingrep_core::execution::SentenceAdmission;
use lingrep_core::execution::{
    ExecutionBatch, ExecutionLimits, OversizedSentenceOmissions, SentenceBatchSource,
    TokenizedAdmission, explain_in_windows,
};
use lingrep_core::fsm::{BoundCatalog, CompiledCatalog, SentenceExplanation};

use crate::{
    corpus_runtime::ReadyCorpus,
    inspection::OverlapCandidate,
    search_scope::BoundScope,
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
        let mut compiled =
            crate::patterns::compile_web_pattern(&snapshot.source).map_err(super::server_error)?;
        compiled.matcher = compiled.matcher.with_overlaps(snapshot.overlaps);
        let windows = ScanWindows::new(&compiled.matcher, &ready, &snapshot.admission)?;
        let mut explanations = HashMap::new();
        references
            .into_iter()
            .map(|(sentence_index, span)| {
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    explanations.entry(sentence_index)
                {
                    let explanation = windows.explain(sentence_index)?;
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
    .map_err(super::server_error)?
}

struct ScanWindows {
    bound: BoundCatalog,
    ready: ReadyCorpus,
    admission: SentenceAdmission,
    provenance: Arc<lingrep_corpus::ProvenanceView>,
    division_kinds: Vec<compact_str::CompactString>,
}

impl ScanWindows {
    #[allow(clippy::result_large_err)]
    fn new(
        catalog: &CompiledCatalog,
        ready: &ReadyCorpus,
        admission: &SentenceAdmission,
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
            admission: admission.clone(),
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
            let mut source = BoundScope::new(self.admission.clone(), Arc::clone(&self.provenance))
                .open_source(
                    &self.ready.artifact,
                    self.bound.projection(),
                    self.division_kinds.clone(),
                    Arc::clone(&self.ready.pool),
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
            .map_err(|error| match error {
                lingrep_core::fsm::BoundExplanationError::Bound(
                    lingrep_core::fsm::BoundExecutionError::Resource(error),
                ) => resource_error(error),
                other => super::server_error(other),
            })?;
            if let Some(explanation) = explanation {
                return Ok(explanation);
            }
            margin = margin.saturating_mul(2);
        }
    }
}

fn resource_error(error: lingrep_core::fsm::MatchResourceError) -> Response {
    let (code, message) = match &error {
        lingrep_core::fsm::MatchResourceError::InterpretationReplay { .. } => {
            ("interpretation_replay", error.to_string())
        }
        lingrep_core::fsm::MatchResourceError::Cancelled => ("cancelled", error.to_string()),
        lingrep_core::fsm::MatchResourceError::Sentence { resource, .. }
            if resource.starts_with("interpretation") =>
        {
            ("interpretation_limit", error.to_string())
        }
        lingrep_core::fsm::MatchResourceError::Sentence { .. } => {
            ("graph_assignment_limit", error.to_string())
        }
        lingrep_core::fsm::MatchResourceError::BatchOutput { .. } => (
            "match_batch_too_large",
            format!("{error} Narrow the pattern to produce fewer matches in one sentence."),
        ),
    };
    (
        axum::http::StatusCode::UNPROCESSABLE_ENTITY,
        axum::Json(serde_json::json!({
            "error": message,
            "code": code,
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn batch_output_response_has_a_distinct_code_and_pattern_remedy() {
        let response = super::resource_error(lingrep_core::fsm::MatchResourceError::BatchOutput {
            limit: 1024,
        });
        assert_eq!(
            response.status(),
            axum::http::StatusCode::UNPROCESSABLE_ENTITY
        );
        let bytes = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["code"], "match_batch_too_large");
        let message = body["error"].as_str().unwrap();
        assert!(message.contains("1024"));
        assert!(message.contains("Narrow the pattern"));
        assert!(!message.contains("ordering constraint"));
    }

    #[tokio::test]
    async fn graph_resource_response_preserves_the_pattern_and_remedy() {
        let response = super::resource_error(lingrep_core::fsm::MatchResourceError::Sentence {
            pattern: "connected".into(),
            resource: "graph assignments",
            limit: 65_536,
        });
        assert_eq!(
            response.status(),
            axum::http::StatusCode::UNPROCESSABLE_ENTITY
        );
        let body = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["code"], "graph_assignment_limit");
        assert!(body["error"].as_str().unwrap().contains("connected"));
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("ordering constraint")
        );
    }
}
