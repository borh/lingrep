use std::{collections::HashMap, sync::Arc};

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use lingrep_core::token_match::TokenInfo;

use crate::{
    inspection::OverlapCandidate,
    snapshot::{CompletedSnapshot, MatchRef, PatternKey},
    state::RequestStore,
};

#[allow(clippy::result_large_err)]
pub(super) async fn inspect(
    store: &RequestStore,
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
    let permit = store.coordinators.acquire_scan_permit().await;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let compiled = crate::patterns::compile_web_pattern(&snapshot.source)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
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
                    let explanation = compiled
                        .matcher
                        .explain_sentence(tokens)
                        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
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
