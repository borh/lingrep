use axum::{
    Router,
    extract::{Path, State},
    response::Response,
    routing::get,
};

use crate::inspection::{InspectionError, WHOLE_SENTENCE_CONTEXT};
use crate::revision::{RevisionId, WorkbenchId};
use crate::snapshot::PatternKey;
use crate::state::RequestStore;
use crate::templates::TokenInspectionFragment;

use super::{EndpointInspectionError, bound_inspector, inspection_response};

pub fn routes() -> Router<RequestStore> {
    Router::new().route(
        "/inspect/{workbench_id}/{revision}/{pattern_key}/{reference_ordinal}/{token_index}",
        get(inspect_token),
    )
}

async fn inspect_token(
    State(store): State<RequestStore>,
    Path((workbench_id, revision, pattern_key, reference_ordinal, token_index)): Path<(
        String,
        u64,
        usize,
        usize,
        usize,
    )>,
) -> Response {
    match inspect(
        &store,
        WorkbenchId::new(workbench_id),
        RevisionId(revision),
        pattern_key,
        reference_ordinal,
        token_index,
    )
    .await
    {
        Ok(fragment) => super::render_or_500(fragment),
        Err(error) => inspection_response(error),
    }
}

async fn inspect(
    store: &RequestStore,
    workbench: WorkbenchId,
    revision: RevisionId,
    pattern_index: usize,
    reference_ordinal: usize,
    token_index: usize,
) -> Result<TokenInspectionFragment, EndpointInspectionError> {
    let (_snapshot, inspector, _ready, runtime) =
        bound_inspector(store, &workbench, revision).await?;
    let pattern = PatternKey::new(pattern_index).ok_or(EndpointInspectionError::UnknownPattern)?;
    let inspection = match inspector
        .token(
            pattern,
            reference_ordinal,
            token_index,
            WHOLE_SENTENCE_CONTEXT,
        )
        .await
    {
        Ok(inspection) => inspection,
        Err(InspectionError::CacheCorruption) => {
            // The workbench's own corpus, not the store's default.
            runtime.invalidator().invalidate_corrupt().await;
            return Err(EndpointInspectionError::CacheCorruption);
        }
        Err(other) => return Err(other.into()),
    };
    Ok(TokenInspectionFragment::new(inspection))
}
