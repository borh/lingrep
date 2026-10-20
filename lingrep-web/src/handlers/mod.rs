pub(crate) mod asset;
pub mod catalogs;
pub mod corpus_context;
pub mod editor_assets;
pub mod export;
pub mod help;
pub mod index;
pub mod inspect;
pub mod pattern_detail;
pub mod pattern_editor;
pub mod permalink;
pub mod results_assets;
pub mod sample;
pub mod search;
pub mod section_analysis;
pub mod stream;

pub use index::index_page;
pub use search::search;
pub use stream::routes as stream_routes;

use std::collections::HashMap;
use std::sync::Arc;

use askama::Template;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

use lingrep_core::corpus::SentenceId;
use lingrep_corpus::{CorpusArtifact, CorpusError, ProvenanceView, SentenceProvenance};

use crate::coordinator::SnapshotLookupError;
use crate::corpus_runtime::{CorpusRuntime, ReadyCorpus};
use crate::inspection::{InspectionError, SnapshotInspector};
use crate::revision::{RevisionId, WorkbenchId};
use crate::snapshot::{CompletedSnapshot, MatchRef};
use crate::state::RequestStore;

pub(crate) struct BoundProvenance {
    artifact: Arc<CorpusArtifact>,
    view: Arc<ProvenanceView>,
}

impl BoundProvenance {
    /// Binds the ready corpus's **cached** provenance view.
    ///
    /// Deliberately takes the `ReadyCorpus` rather than its artifact: the
    /// artifact-only call opens a fresh view, which revalidates every sentence
    /// row, and this runs on every pattern-detail and hit-detail request.
    async fn open(ready: &ReadyCorpus) -> Result<Self, CorpusError> {
        let view = ready.provenance().await?;
        Ok(Self {
            artifact: Arc::clone(&ready.artifact),
            view,
        })
    }
}

/// Unified detail-endpoint error. Covers snapshot lookup, corpus staleness,
/// and inspection failures across `inspect`, pattern detail, and hit detail so
/// all report the same status/message for the same underlying condition
/// (in particular, a stale corpus is a 409 everywhere).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EndpointInspectionError {
    UnknownWorkbench,
    StaleSnapshot,
    StaleCorpus,
    UnknownPattern,
    UnknownReference,
    InvalidSpan,
    CacheCorruption,
    UnknownToken,
}

impl From<SnapshotLookupError> for EndpointInspectionError {
    fn from(error: SnapshotLookupError) -> Self {
        match error {
            SnapshotLookupError::UnknownWorkbench => Self::UnknownWorkbench,
            SnapshotLookupError::StaleSnapshot => Self::StaleSnapshot,
        }
    }
}

impl From<InspectionError> for EndpointInspectionError {
    fn from(error: InspectionError) -> Self {
        match error {
            InspectionError::StaleCorpus => Self::StaleCorpus,
            InspectionError::UnknownPattern => Self::UnknownPattern,
            InspectionError::UnknownReference => Self::UnknownReference,
            InspectionError::InvalidSpan => Self::InvalidSpan,
            InspectionError::CacheCorruption => Self::CacheCorruption,
            InspectionError::UnknownToken => Self::UnknownToken,
        }
    }
}

pub(crate) fn inspection_response(error: EndpointInspectionError) -> Response {
    match error {
        EndpointInspectionError::UnknownWorkbench
        | EndpointInspectionError::UnknownPattern
        | EndpointInspectionError::UnknownReference
        | EndpointInspectionError::UnknownToken => {
            sanitized(StatusCode::NOT_FOUND, "Inspection target not found.")
        }
        EndpointInspectionError::StaleSnapshot => sanitized(
            StatusCode::CONFLICT,
            "Newer results are available. Refresh this result to inspect it.",
        ),
        EndpointInspectionError::StaleCorpus => sanitized(
            StatusCode::CONFLICT,
            "These results belong to an older corpus cache.",
        ),
        EndpointInspectionError::InvalidSpan => sanitized(
            StatusCode::CONFLICT,
            "Stored inspection coordinates are invalid.",
        ),
        EndpointInspectionError::CacheCorruption => sanitized(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The corpus cache is damaged and will be rebuilt.",
        ),
    }
}

/// Render an Askama template into an HTML 200, or a bare 500 if rendering fails.
/// Shared by the detail endpoints so a template failure is handled identically
/// everywhere. Callers that need a *custom* failure body (e.g. the sample
/// endpoint's status-specific fragments) do not use this.
pub(crate) fn render_or_500<T: Template>(template: T) -> Response {
    template.render().map(Html).map_or_else(
        |_| StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        IntoResponse::into_response,
    )
}

pub(crate) fn sanitized(status: StatusCode, message: &'static str) -> Response {
    (
        status,
        Html(format!("<div class=\"alert alert-error\">{message}</div>")),
    )
        .into_response()
}

/// Resolve the displayed snapshot for `workbench`/`revision` and bind a
/// [`SnapshotInspector`] against the currently-ready cache of **the workbench's
/// own corpus**. Shared by `inspect`, pattern detail, and hit detail so all
/// fail the same way for the same underlying condition.
///
/// The returned runtime is the one every later step of the same request must
/// use — notably [`decode_refs`], whose corruption teardown has to reach the
/// corpus that was actually read and no other.
pub(crate) async fn bound_inspector(
    store: &RequestStore,
    workbench: &WorkbenchId,
    revision: RevisionId,
) -> Result<
    (
        Arc<CompletedSnapshot>,
        SnapshotInspector,
        Option<BoundProvenance>,
        Arc<CorpusRuntime>,
    ),
    EndpointInspectionError,
> {
    let snapshot = store
        .coordinators
        .displayed_snapshot(workbench, revision)
        .await?;
    // The workbench's corpus, not the store's default: a workbench is bound to
    // one corpus at admission (B3), and that binding is what makes the
    // identity check in `SnapshotInspector::bind` below succeed.
    //
    // The `UnknownWorkbench` arm is not dead: `displayed_snapshot` above raises
    // it for a missing entry, so this one is only reachable when the entry is
    // evicted between the two lookups.
    let runtime = store
        .coordinators
        .bound_corpus(workbench)
        .await
        .ok_or(EndpointInspectionError::UnknownWorkbench)?;
    let ready = runtime
        .ready()
        .await
        .ok_or(EndpointInspectionError::StaleCorpus)?;
    let provenance = BoundProvenance::open(&ready)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "provenance view unavailable; rendering without it");
        })
        .ok();
    let reader = ready
        .reader()
        .map_err(|_| EndpointInspectionError::CacheCorruption)?;
    let inspector = SnapshotInspector::bind(Arc::clone(&snapshot), reader)?;
    Ok((snapshot, inspector, provenance, runtime))
}

/// Read provenance for `ids`, aligned to input order. Provenance is an
/// enhancement: an absent store or a read error degrades to all-`None` so the
/// caller renders without it rather than failing.
pub(crate) async fn read_provenance(
    provenance: Option<&BoundProvenance>,
    ids: &[SentenceId],
) -> Vec<Option<SentenceProvenance>> {
    let Some(provenance) = provenance else {
        return vec![None; ids.len()];
    };
    let mut page = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(lingrep_corpus::MAX_PROVENANCE_PAGE_SIZE) {
        let ordinals = chunk.iter().map(|id| id.get()).collect::<Vec<_>>();
        match provenance
            .artifact
            .sentence_provenance_page(&provenance.view, &ordinals)
            .await
        {
            Ok(sentences) => page.extend(sentences),
            Err(error) => return provenance_or_degrade(Err(error), ids.len()),
        }
    }
    provenance_or_degrade(Ok(page), ids.len())
}

/// Provenance is an enhancement: a read error degrades to an aligned all-`None`
/// result (rendered without provenance) rather than failing the request. Pure
/// over the `Result` so both branches are testable without a live database.
fn provenance_or_degrade(
    result: Result<Vec<Option<SentenceProvenance>>, CorpusError>,
    len: usize,
) -> Vec<Option<SentenceProvenance>> {
    match result {
        Ok(page) if page.len() == len => page,
        Ok(_) => vec![None; len],
        Err(error) => {
            tracing::warn!(%error, "provenance read failed; rendering without it");
            vec![None; len]
        }
    }
}

/// Decode every sentence referenced by `refs`, off the executor, in chunks so
/// no single blocking job runs the whole request. Invalidates once on corruption.
///
/// `runtime` must be the one [`bound_inspector`] returned for this request:
/// the corruption teardown has to reach the corpus whose bytes failed to
/// decode, and no other corpus the process happens to serve.
// `Err = Response` is the axum idiom: the error path already owns a fully formed
// HTTP response, and every caller returns it directly. Boxing it to satisfy
// `result_large_err` would add an allocation to each error path and force the
// callers to unbox before returning, with no benefit at this call depth.
#[allow(clippy::result_large_err)]
pub(crate) async fn decode_refs(
    runtime: &CorpusRuntime,
    inspector: &SnapshotInspector,
    refs: &[&MatchRef],
) -> Result<HashMap<u64, Option<Vec<lingrep_core::token_match::TokenInfo>>>, Response> {
    let mut unique: Vec<u64> = refs.iter().map(|r| r.sentence_index).collect();
    unique.sort_unstable();
    unique.dedup();

    let mut map: HashMap<u64, Option<Vec<lingrep_core::token_match::TokenInfo>>> = HashMap::new();
    for chunk in unique.chunks(crate::corpus_runtime::CORPUS_DECODE_CHUNK) {
        let reader = inspector.reader_clone();
        let chunk_vec = chunk.to_vec();
        // One blocking job PER chunk; the await between chunks yields the worker.
        let decoded =
            tokio::task::spawn_blocking(move || reader.decode_sentences(&chunk_vec)).await;
        match decoded {
            Ok(Ok(values)) => {
                for (index, value) in chunk.iter().zip(values) {
                    map.insert(*index, value);
                }
            }
            Ok(Err(_)) => {
                // The workbench's own corpus — the one whose bytes just failed
                // to decode. Invalidating a process-wide runtime here would
                // tear down an unrelated corpus.
                runtime.invalidator().invalidate_corrupt().await; // exactly once
                return Err(inspection_response(
                    EndpointInspectionError::CacheCorruption,
                ));
            }
            Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        }
    }
    Ok(map)
}

pub(crate) fn invalid_provenance_response() -> Response {
    (
        StatusCode::CONFLICT,
        Html(
            "<div class=\"alert alert-error\">Stored result provenance is invalid; rerun the query.</div>",
        ),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    #[test]
    fn invalid_provenance_aborts_the_whole_detail_request() {
        assert_eq!(
            super::invalid_provenance_response().status(),
            axum::http::StatusCode::CONFLICT
        );
    }
}

#[cfg(test)]
mod read_provenance_tests {
    use super::{provenance_or_degrade, read_provenance};
    use lingrep_core::corpus::SentenceId;
    use lingrep_corpus::CorpusError;

    #[tokio::test]
    async fn without_a_store_yields_aligned_none() {
        let ids = [SentenceId::new(0), SentenceId::new(1)];
        let result = read_provenance(None, &ids).await;
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(Option::is_none));
    }

    #[test]
    fn a_read_error_degrades_to_aligned_none() {
        let degraded = provenance_or_degrade(
            Err(CorpusError::InvalidFormat {
                component: "provenance.sqlite",
                message: "injected".into(),
            }),
            3,
        );
        assert_eq!(degraded.len(), 3);
        assert!(degraded.iter().all(Option::is_none));
    }
}
