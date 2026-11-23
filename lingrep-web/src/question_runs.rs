use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response, Sse, sse::Event};
use axum::{
    Json, Router,
    routing::{get, post},
};
use futures::StreamExt;
use lingrep_core::execution::{
    ExecutionBatch, ExecutionError, ExecutionLimits, SentenceAdmission, SentenceBatchSource,
};
use lingrep_core::fsm::{GraphBinding, LexicalProjectedKeyValue};
use lingrep_core::token_match::BoundTokenSentence;
use lingrep_corpus::SentenceProvenance;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::coordinator::{CoordinatorView, RevisionRequest};
use crate::corpus_runtime::ReadyCorpus;
use crate::question_corpus::{TokenRow, token_rows_for_keys};
use crate::result_index::{ResultIndex, ResultOccurrence};
use crate::revision::{AttemptPhase, ErrorCategory, RevisionId, WorkbenchId};
use crate::snapshot::{CaptureDescriptor, CompletedSnapshot, PatternKey};
use crate::state::RequestStore;

const REVISION: RevisionId = RevisionId(1);
const INSPECTION_BYTES: usize = 1024 * 1024;
const PREVIEW_BYTES: usize = 4 * 1024 * 1024;
static VERSION: AtomicU64 = AtomicU64::new(1);

type Result<T> = std::result::Result<T, ApiError>;

struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
fn expired() -> ApiError {
    ApiError(
        StatusCode::GONE,
        "This search has expired. Run the saved query again.".into(),
    )
}
fn invalid(message: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, message.into())
}
fn unavailable(error: impl std::fmt::Display) -> ApiError {
    tracing::warn!(cause=%error,"question result access failed");
    ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "The results could not be read. Try again or rerun this search.".into(),
    )
}
fn workbench(id: &str) -> Result<WorkbenchId> {
    let parsed = uuid::Uuid::parse_str(id).map_err(|_| expired())?;
    Ok(WorkbenchId::new(parsed.simple().to_string()))
}

pub(crate) fn routes() -> Router<RequestStore> {
    Router::new()
        .route("/runs", post(submit))
        .route("/runs/{id}", get(snapshot))
        .route("/runs/{id}/events", get(events))
        .route("/runs/{id}/cancel", post(cancel))
        .route("/runs/{id}/documents", get(documents))
        .route("/runs/{id}/work-samples", get(work_samples))
        .route("/runs/{id}/occurrence", get(occurrence))
        .route("/runs/{id}/export", get(export))
}

#[derive(Deserialize)]
struct Submission {
    source: String,
    dataset: String,
    #[serde(default, rename = "where")]
    condition: String,
    #[serde(default)]
    expected_analysis_identity: Option<String>,
    expected_vectors_blake3: Option<String>,
    /// Publishes only this pattern of `source`.
    #[serde(default)]
    pattern: Option<String>,
    /// Admits only this corpus document; refused beside a `where` condition.
    #[serde(default)]
    document: Option<u64>,
}
async fn submit(
    State(store): State<RequestStore>,
    body: std::result::Result<Json<Submission>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>> {
    let Json(body) = body.map_err(|_| {
        invalid("The search request could not be read. Check the pattern and document conditions.")
    })?;
    if body.source.len() > 1024 * 1024 {
        return Err(invalid(
            "The pattern is too large. Remove unused definitions and search again.",
        ));
    }
    let expected_vectors_blake3 = body
        .expected_vectors_blake3
        .as_deref()
        .map(blake3::Hash::from_hex)
        .transpose()
        .map_err(|_| invalid("The saved word-vector digest must contain 64 hexadecimal digits."))?;
    let slot = store.corpora.resolve(Some(&body.dataset)).ok_or_else(|| {
        invalid("This corpus is not available on this server. Choose another corpus.")
    })?;
    slot.runtime.start_or_observe().await;
    let clause = body.condition;
    let scope = crate::search_scope::SearchScope::parse(Some(
        &json!({"clause":if clause.trim().is_empty(){None}else{Some(clause)},"document":body.document}).to_string(),
    ))
    .map_err(invalid)?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    store
        .coordinators
        .accept_revision(
            WorkbenchId::new(&id),
            slot,
            RevisionRequest {
                source: body.source,
                pattern: body.pattern.filter(|pattern| !pattern.is_empty()),
                retained: Some(30),
                document_reach: true,
                search_scope: scope,
                index_results: true,
                expected_vectors_blake3,
                expected_analysis_identity: body
                    .expected_analysis_identity
                    .filter(|s| !s.is_empty()),
                ..Default::default()
            },
        )
        .await
        .map_err(|error| {
            ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                match error {
                    crate::coordinator::AdmitError::RegistryFull => {
                        "This server is busy with other searches. Try again shortly.".into()
                    }
                    crate::coordinator::AdmitError::CorpusConflict => {
                        "This search belongs to another corpus. Start a new search.".into()
                    }
                },
            )
        })?;
    Ok(Json(json!({"id":id})))
}

#[derive(Serialize)]
struct RunSnapshot {
    id: String,
    phase: &'static str,
    source: String,
    dataset: String,
    #[serde(rename = "where")]
    condition: String,
    counts: BTreeMap<String, u64>,
    rows: BTreeMap<String, Vec<Value>>,
    definitions: Vec<String>,
    bodies: BTreeMap<String, String>,
    received: u64,
    started: u64,
    elapsed: f64,
    version: u64,
    error: Option<String>,
    error_code: Option<&'static str>,
    /// Set on a completed search whose preview cannot be projected.
    #[serde(skip_serializing_if = "Option::is_none")]
    search_complete: Option<bool>,
    /// Whether the run's export can hold every match; present with `search_complete`.
    #[serde(skip_serializing_if = "Option::is_none")]
    exportable: Option<bool>,
    diagnostics: Vec<Value>,
    notices: Vec<String>,
    counters: Option<Value>,
    contract_run: Option<Value>,
}

async fn view(store: &RequestStore, id: &str) -> Result<(WorkbenchId, CoordinatorView)> {
    let workbench = workbench(id)?;
    let view = store
        .coordinators
        .snapshot(&workbench)
        .await
        .ok_or_else(expired)?;
    if view
        .state
        .current_attempt()
        .is_none_or(|a| a.revision != REVISION)
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "This search was replaced. Run the saved query again.".into(),
        ));
    }
    Ok((workbench, view))
}
async fn ready(store: &RequestStore, workbench: &WorkbenchId) -> Result<(String, ReadyCorpus)> {
    let (key, runtime) = store
        .coordinators
        .bound_entry(workbench)
        .await
        .ok_or_else(expired)?;
    let ready = runtime.ready().await.ok_or_else(|| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "The corpus is still preparing. Try again shortly.".into(),
        )
    })?;
    Ok((key.as_str().to_owned(), ready))
}
async fn completed(
    store: &RequestStore,
    id: &str,
) -> Result<(Arc<CompletedSnapshot>, ReadyCorpus, Arc<ResultIndex>)> {
    let (workbench, view) = view(store, id).await?;
    if !matches!(
        view.state.current_attempt().map(|a| a.phase),
        Some(AttemptPhase::Completed)
    ) {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Complete this search before browsing or exporting all its results.".into(),
        ));
    }
    let snapshot = store
        .coordinators
        .displayed_snapshot(&workbench, REVISION)
        .await
        .map_err(unavailable)?;
    let (_, ready) = ready(store, &workbench).await?;
    if snapshot.corpus_identity != ready.identity {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "The corpus for this search has changed. Run the query again.".into(),
        ));
    }
    let index = snapshot.result_index.clone().ok_or_else(|| {
        ApiError(
            StatusCode::CONFLICT,
            "Complete results are unavailable for this search. Run it again.".into(),
        )
    })?;
    Ok((snapshot, ready, index))
}

type LivePreviewCache = HashMap<u64, (Value, usize)>;

async fn project_run(
    store: &RequestStore,
    id: &str,
    view: &CoordinatorView,
) -> Result<RunSnapshot> {
    project_run_with_cache(store, id, view, None).await
}

async fn project_run_with_cache(
    store: &RequestStore,
    id: &str,
    view: &CoordinatorView,
    mut live_cache: Option<&mut LivePreviewCache>,
) -> Result<RunSnapshot> {
    let _permit = store.coordinators.acquire_reading_permit().await;
    let version = VERSION.fetch_add(1, Ordering::Relaxed);
    let workbench = workbench(id)?;
    let (key, runtime) = store
        .coordinators
        .bound_entry(&workbench)
        .await
        .ok_or_else(expired)?;
    let attempt = view.state.current_attempt().ok_or_else(expired)?;
    if attempt.revision != REVISION {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "This search was replaced. Run the saved query again.".into(),
        ));
    }
    let phase = match attempt.phase {
        AttemptPhase::Completed => "complete",
        AttemptPhase::Cancelled(_) => "cancelled",
        AttemptPhase::Failed => "failed",
        _ => "running",
    };
    let snapshot = view.state.displayed_snapshot();
    let mut counts = BTreeMap::new();
    let mut rows = BTreeMap::new();
    let mut preview_bytes = 0usize;
    let mut definitions = Vec::new();
    let mut contract_run = None;
    let mut counters = None;
    let current_ready = runtime.ready().await;
    let mut condition = attempt.search_scope.clause.clone().unwrap_or_default();
    let completed = attempt.phase == AttemptPhase::Completed;
    let mut undisplayable: Option<String> = None;
    if let Some(snapshot) = snapshot.filter(|snapshot| snapshot.revision == attempt.revision) {
        condition = snapshot.search_scope.clause.clone().unwrap_or_default();
        for pattern in &snapshot.patterns {
            definitions.push(pattern.name.clone());
            counts.insert(pattern.name.clone(), pattern.exact_total);
        }
        if let (Some(index), Some(ready)) = (&snapshot.result_index, &current_ready) {
            for pattern in &snapshot.patterns {
                let page = index
                    .page(pattern.key, None, -1, 30)
                    .await
                    .map_err(unavailable)?;
                let group = match project_occurrences(
                    ready,
                    &page.rows,
                    &snapshot.capture_catalog,
                    &definitions,
                    index.publication_attributes(),
                    INSPECTION_BYTES,
                    OccurrenceOutput::Matches,
                )
                .await
                {
                    Ok(group) => group,
                    Err(error) if completed && error.0 == StatusCode::PAYLOAD_TOO_LARGE => {
                        undisplayable =
                            Some("An example in these results is too large to display.".into());
                        break;
                    }
                    Err(error) => return Err(error),
                };
                preview_bytes = preview_bytes
                    .saturating_add(serde_json::to_vec(&group).map_err(unavailable)?.len());
                if preview_bytes > PREVIEW_BYTES {
                    let message =
                        "The result preview is too large to display. Search fewer patterns.";
                    if completed {
                        undisplayable = Some(message.into());
                        break;
                    }
                    return Err(ApiError(StatusCode::PAYLOAD_TOO_LARGE, message.into()));
                }
                rows.insert(pattern.name.clone(), group);
            }
        }
        let documents = snapshot.documents.as_ref().map_or_else(
            || match &snapshot.admission {
                SentenceAdmission::Documents(set) => set.len(),
                _ => 0,
            },
            |d| d.documents_scanned,
        );
        counters = Some(
            json!({"counters":{"documents_admitted":documents,"matches_found":counts.values().sum::<u64>()}}),
        );
    } else if let Some(summary) = &view.live_summary {
        for pattern in summary.rows.iter() {
            definitions.push(pattern.name.to_string());
            counts.insert(pattern.name.to_string(), pattern.total);
        }
        if let Some(ready) = &current_ready {
            let uncached: Vec<ResultOccurrence> = match live_cache.as_deref() {
                Some(cache) => summary
                    .preview
                    .iter()
                    .filter(|occ| !cache.contains_key(&occ.sequence))
                    .cloned()
                    .collect(),
                None => summary.preview.to_vec(),
            };
            let mut projected = if uncached.is_empty() {
                Vec::new().into_iter()
            } else {
                project_occurrences(
                    ready,
                    &uncached,
                    &summary.capture_catalog,
                    &definitions,
                    &summary.publication_attributes,
                    INSPECTION_BYTES,
                    OccurrenceOutput::Matches,
                )
                .await?
                .into_iter()
            };
            for occurrence in summary.preview.iter() {
                let name = definitions
                    .get(occurrence.reference.pattern_key.index())
                    .cloned()
                    .unwrap_or_default();
                let (row, entry_bytes) = if let Some((cached, bytes)) = live_cache
                    .as_deref()
                    .and_then(|cache| cache.get(&occurrence.sequence))
                {
                    (cached.clone(), *bytes)
                } else {
                    let row = projected
                        .next()
                        .expect("every uncached preview occurrence was projected");
                    // Matches `serde_json::to_vec(&[row]).len()` (`row` plus `[` and `]`).
                    let entry_bytes = serde_json::to_vec(&row)
                        .map_err(unavailable)?
                        .len()
                        .saturating_add(2);
                    if let Some(cache) = live_cache.as_deref_mut()
                        && preview_bytes.saturating_add(entry_bytes) <= PREVIEW_BYTES
                    {
                        cache.insert(occurrence.sequence, (row.clone(), entry_bytes));
                    }
                    (row, entry_bytes)
                };
                preview_bytes = preview_bytes.saturating_add(entry_bytes);
                if preview_bytes > PREVIEW_BYTES {
                    return Err(ApiError(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "The result preview is too large to display. Search fewer patterns.".into(),
                    ));
                }
                rows.entry(name).or_insert_with(Vec::new).push(row);
            }
        }
    }
    if let Some(ready) = &current_ready {
        let mut data = json!({"analysis_identity":lingrep_corpus::format_identity(ready.artifact.analysis_identity().as_bytes())});
        if let Some(digest) = snapshot
            .filter(|s| s.revision == attempt.revision)
            .and_then(|s| s.vectors_binding.as_ref())
            .map(|v| v.digest)
        {
            data["vectors_blake3"] = json!(digest.to_hex().as_str());
        }
        contract_run = Some(data);
    }
    let diagnostics = if attempt
        .failure
        .as_ref()
        .is_some_and(|failure| failure.category == ErrorCategory::Compile)
    {
        let source = attempt.source.clone();
        store.coordinators.pattern_task(move||crate::patterns::compile_web_pattern(&source).err().map(|error|error.diagnostics.into_iter().map(|diagnostic|{
            let location=diagnostic.location();let span=location.byte_range();
            let related=diagnostic.related_locations().iter().map(|related|{let location=related.location();let span=location.byte_range();json!({"message":related.message(),"location":{"source_name":location.source_name(),"source_span":{"start":span.start,"end":span.end},"line":location.line(),"column":location.column()}})}).collect::<Vec<_>>();
            json!({"schema":"lingrep.diagnostic/v1","type":"diagnostic","data":{"stage":"catalog","severity":"error","message":diagnostic.message,"location":{"source_name":location.source_name(),"source_span":{"start":span.start,"end":span.end},"line":location.line(),"column":location.column()},"related_locations":related}})
        }).collect::<Vec<_>>()).unwrap_or_default()).await.map_err(unavailable)?
    } else {
        Vec::new()
    };
    if definitions.is_empty()
        && attempt
            .failure
            .as_ref()
            .is_some_and(|failure| failure.category == ErrorCategory::ResultLimit)
    {
        let source = attempt.source.clone();
        definitions = store
            .coordinators
            .pattern_task(move || catalog_definitions(&source))
            .await
            .map_err(unavailable)?;
    }
    let (phase, error, search_complete, exportable) = match undisplayable {
        Some(message) => {
            rows.clear();
            ("failed", Some(message), Some(true), Some(true))
        }
        None => (
            phase,
            attempt.failure.as_ref().map(|e| e.message.clone()),
            None,
            None,
        ),
    };
    Ok(RunSnapshot {
        id: id.into(),
        phase,
        source: attempt.source.to_string(),
        dataset: key.as_str().to_owned(),
        condition,
        received: counts.values().sum(),
        counts,
        rows,
        definitions,
        bodies: BTreeMap::new(),
        started: 0,
        elapsed: 0.0,
        version,
        error,
        error_code: attempt.failure.as_ref().and_then(|e| match e.category {
            ErrorCategory::VectorsChanged => Some("vectors_changed"),
            ErrorCategory::AnalysisChanged => Some("analysis_changed"),
            ErrorCategory::ResultLimit => Some("result_limit"),
            _ => None,
        }),
        search_complete,
        exportable,
        diagnostics,
        notices: crate::revision_status::project_revision_status(Some(view))
            .notices
            .into_iter()
            .map(|notice| notice.text)
            .collect(),
        counters,
        contract_run,
    })
}

async fn snapshot(
    State(store): State<RequestStore>,
    Path(id): Path<String>,
) -> Result<Json<RunSnapshot>> {
    let (_, view) = view(&store, &id).await?;
    Ok(Json(project_run(&store, &id, &view).await?))
}
async fn cancel(State(store): State<RequestStore>, Path(id): Path<String>) -> Result<Json<Value>> {
    let id = workbench(&id)?;
    let cancelled = store
        .coordinators
        .cancel(&id, REVISION)
        .await
        .map_err(|_| expired())?;
    Ok(Json(json!({"cancelled":cancelled})))
}

async fn events(State(store): State<RequestStore>, Path(id): Path<String>) -> Result<Response> {
    let (workbench, _) = view(&store, &id).await?;
    let receiver = store
        .coordinators
        .subscribe(&workbench)
        .await
        .ok_or_else(expired)?;
    let stream = futures::stream::unfold(
        (
            store,
            id,
            receiver,
            None::<Value>,
            LivePreviewCache::new(),
            false,
            false,
        ),
        |(store, id, mut receiver, prior, mut live_cache, wait, finished)| async move {
            if finished || wait && receiver.changed().await.is_err() {
                return None;
            }
            let view = receiver.borrow_and_update().clone();
            let projected =
                match project_run_with_cache(&store, &id, &view, Some(&mut live_cache)).await {
                    Ok(projected) => projected,
                    Err(error) => {
                        let counts = run_counts(&view);
                        let received = counts.values().sum::<u64>();
                        let version = VERSION.fetch_add(1, Ordering::Relaxed);
                        let continues = search_continues(&view);
                        let patch = if continues {
                            json!({"counts":counts,"received":received,"version":version})
                        } else {
                            live_cache.clear();
                            json!({"phase":"failed","error":error.1,"version":version})
                        };
                        let event = Event::default().event("update").data(
                            json!({"id":id,"reset":false,"patch":patch,"append":{}}).to_string(),
                        );
                        return Some((
                            Ok::<_, Infallible>(event),
                            (store, id, receiver, prior, live_cache, true, !continues),
                        ));
                    }
                };
            let finished = projected.phase != "running";
            if finished {
                live_cache.clear();
            }
            let undisplayable = projected.search_complete.is_some();
            let value = serde_json::to_value(projected).expect("question snapshots serialize");
            let mut patch = value.as_object().expect("snapshot object").clone();
            let all_rows = patch.remove("rows").unwrap_or_else(|| json!({}));
            let reset = prior.is_none();
            let mut append = serde_json::Map::new();
            for (pattern, rows) in all_rows.as_object().expect("rows object") {
                let old = prior
                    .as_ref()
                    .and_then(|p| p.get("rows"))
                    .and_then(|rows| rows.get(pattern))
                    .and_then(Value::as_array);
                let seen = old
                    .into_iter()
                    .flatten()
                    .filter_map(|r| r.get("sequence").and_then(Value::as_u64))
                    .collect::<HashSet<_>>();
                let added = rows
                    .as_array()
                    .expect("row array")
                    .iter()
                    .filter(|row| {
                        reset
                            || !row
                                .get("sequence")
                                .and_then(Value::as_u64)
                                .is_some_and(|s| seen.contains(&s))
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if !added.is_empty() {
                    append.insert(pattern.clone(), Value::Array(added));
                }
            }
            if let Some(old) = prior.as_ref().filter(|_| !undisplayable) {
                patch.retain(|key, value| key == "version" || old.get(key) != Some(value));
            }
            let event = Event::default()
                .event("update")
                .data(json!({"id":id,"reset":reset,"patch":patch,"append":append}).to_string());
            Some((
                Ok::<_, Infallible>(event),
                (store, id, receiver, Some(value), live_cache, true, finished),
            ))
        },
    );
    Ok(Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response())
}

fn run_counts(view: &CoordinatorView) -> BTreeMap<String, u64> {
    let published = view
        .state
        .displayed_snapshot()
        .filter(|snapshot| snapshot.revision == REVISION);
    if let Some(snapshot) = published {
        return snapshot
            .patterns
            .iter()
            .map(|pattern| (pattern.name.clone(), pattern.exact_total))
            .collect();
    }
    view.live_summary
        .iter()
        .flat_map(|summary| summary.rows.iter())
        .map(|pattern| (pattern.name.to_string(), pattern.total))
        .collect()
}

fn search_continues(view: &CoordinatorView) -> bool {
    !view.state.is_terminal()
        && view
            .state
            .current_attempt()
            .is_some_and(|attempt| attempt.revision == REVISION)
}

async fn provenance(
    ready: &ReadyCorpus,
    ordinals: &[u64],
) -> Result<Vec<Option<SentenceProvenance>>> {
    let view = ready.provenance().await.map_err(unavailable)?;
    ready
        .artifact
        .sentence_provenance_page(&view, ordinals)
        .await
        .map_err(unavailable)
}
fn document_metadata(provenance: Option<&SentenceProvenance>) -> Option<Value> {
    let text = provenance?.rich.metadata_json.as_deref()?;
    serde_json::from_str(text).ok()
}

fn document_field(metadata: Option<&Value>, key: &str) -> String {
    metadata
        .and_then(|value| value.get(key))
        .map(|value| {
            value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned)
        })
        .unwrap_or_default()
}

fn document_labels(provenance: Option<&SentenceProvenance>) -> (String, String) {
    let metadata = document_metadata(provenance);
    let source_name = provenance.map_or("", |p| &*p.location.source_name);
    (
        document_title(document_field(metadata.as_ref(), "title"), source_name),
        document_field(metadata.as_ref(), "author"),
    )
}

/// `title`, or when it is empty the source file name without a corpus
/// extension, as the question page's `documentTitle` names a row.
fn document_title(title: String, source_name: &str) -> String {
    if !title.is_empty() {
        return title;
    }
    source_name
        .rsplit_once('.')
        .filter(|(_, extension)| {
            ["conllu", "jsonl", "txt", "md", "xml"]
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
        .map_or(source_name, |(stem, _)| stem)
        .to_owned()
}
fn graph_node(node: lingrep_core::corpus::conllu::EnhancedNodeId) -> Value {
    match node {
        lingrep_core::corpus::conllu::EnhancedNodeId::Word { token_index } => {
            json!({"node_kind":"word","token_index":token_index})
        }
        lingrep_core::corpus::conllu::EnhancedNodeId::Empty { boundary, subindex } => {
            json!({"node_kind":"empty","boundary":boundary,"subindex":subindex})
        }
    }
}
fn graph_binding(binding: &GraphBinding) -> Value {
    match binding {
        GraphBinding::Node {
            name,
            node,
            attributes,
        } => {
            json!({"binding_kind":"node","name":name,"node":graph_node(*node),"attributes":attributes.iter().map(|a|json!({"name":a.name.pattern_key(),"value":a.value})).collect::<Vec<_>>()})
        }
        GraphBinding::Measure { name, value } => {
            json!({"binding_kind":"measure","name":name,"value":value.number(),"status":value.status()})
        }
        GraphBinding::Count { name, value } => {
            json!({"binding_kind":"count", "name":name, "value":value})
        }
        GraphBinding::Edge {
            name,
            head,
            dependent,
            relation,
        } => {
            json!({"binding_kind":"edge","name":name,"head":graph_node(*head),"dependent":graph_node(*dependent),"relation":relation})
        }
    }
}
enum OccurrenceOutput {
    Matches,
    DocumentPage,
    WorkSamples,
}

enum DecodeBatchError {
    Unavailable(String),
    Oversized,
}

type DecodedSentences = HashMap<u64, Option<(BoundTokenSentence, Vec<TokenRow>)>>;

fn decode_projected_sentences(
    artifact: &lingrep_corpus::CorpusArtifact,
    reader: &crate::corpus_runtime::BoundCorpusReader,
    pool: Arc<rayon::ThreadPool>,
    schema: &lingrep_core::token_match::TokenSchema,
    attributes: &[lingrep_core::token_match::AttributeKey],
    unique_sentences: Vec<u64>,
    max_sentence_bytes: usize,
) -> std::result::Result<DecodedSentences, DecodeBatchError> {
    let mut decoded_map = HashMap::with_capacity(unique_sentences.len());
    if unique_sentences.is_empty() {
        return Ok(decoded_map);
    }
    if unique_sentences
        .last()
        .is_some_and(|&ordinal| ordinal >= artifact.sentence_count())
    {
        return Err(DecodeBatchError::Unavailable(
            "sentence ordinal is outside the corpus".into(),
        ));
    }
    if unique_sentences.len() == 1 {
        let ordinal = unique_sentences[0];
        let sentence = reader
            .sentence_projection_bounded(ordinal, attributes, max_sentence_bytes)
            .map_err(|error| DecodeBatchError::Unavailable(error.to_string()))?;
        let projected = sentence.map(|sentence| {
            let rows = token_rows_for_keys(&sentence, attributes);
            (sentence, rows)
        });
        decoded_map.insert(ordinal, projected);
        return Ok(decoded_map);
    }
    let projection = schema
        .bind_projection(attributes.iter().cloned())
        .map_err(|error| DecodeBatchError::Unavailable(error.to_string()))?;
    let mut source = artifact
        .bound_sentence_source(&projection)
        .map_err(|error| DecodeBatchError::Unavailable(error.to_string()))?
        .with_decode_pool(pool)
        .with_ordinals(unique_sentences.clone())
        .map_err(|error| DecodeBatchError::Unavailable(error.to_string()))?;
    let batch_limit = if max_sentence_bytes == usize::MAX {
        usize::MAX
    } else {
        max_sentence_bytes.max(64 * 1024 * 1024)
    };
    let limits = ExecutionLimits {
        max_stage_sentences: unique_sentences.len().max(1),
        max_tokenized_bytes: batch_limit,
        ..ExecutionLimits::default()
    };
    while let Some(batch) = source.next_batch(&limits).map_err(|error| match error {
        ExecutionError::TokenizedSentenceTooLarge { .. } => DecodeBatchError::Oversized,
        other => DecodeBatchError::Unavailable(other.to_string()),
    })? {
        if let ExecutionBatch::BoundTokenized { sentences, .. } = batch {
            for sentence in sentences {
                let ordinal = sentence.location.corpus_sentence_ordinal;
                let words = sentence.value.into_words();
                let within_limit = if max_sentence_bytes == usize::MAX {
                    true
                } else {
                    words
                        .resident_bytes()
                        .map_err(|error| DecodeBatchError::Unavailable(error.to_string()))?
                        <= max_sentence_bytes
                };
                if within_limit {
                    let rows = token_rows_for_keys(&words, attributes);
                    decoded_map.insert(ordinal, Some((words, rows)));
                } else {
                    decoded_map.insert(ordinal, None);
                }
            }
        }
    }
    Ok(decoded_map)
}

async fn project_occurrences(
    ready: &ReadyCorpus,
    occurrences: &[ResultOccurrence],
    captures: &[CaptureDescriptor],
    patterns: &[String],
    publication_attributes: &[lingrep_core::token_match::AttributeKey],
    max_sentence_bytes: usize,
    projection: OccurrenceOutput,
) -> Result<Vec<Value>> {
    let (rows, _) = project_occurrences_with_provenance(
        ready,
        occurrences,
        captures,
        patterns,
        publication_attributes,
        max_sentence_bytes,
        projection,
    )
    .await?;
    Ok(rows)
}

async fn project_occurrences_with_provenance(
    ready: &ReadyCorpus,
    occurrences: &[ResultOccurrence],
    captures: &[CaptureDescriptor],
    patterns: &[String],
    publication_attributes: &[lingrep_core::token_match::AttributeKey],
    max_sentence_bytes: usize,
    projection: OccurrenceOutput,
) -> Result<(Vec<Value>, HashMap<u64, Option<SentenceProvenance>>)> {
    if occurrences.is_empty() {
        return Ok((Vec::new(), HashMap::new()));
    }
    let max_response_bytes = match projection {
        OccurrenceOutput::Matches => None,
        OccurrenceOutput::DocumentPage | OccurrenceOutput::WorkSamples => Some(32 * 1024 * 1024),
    };
    let reader = ready
        .reader()
        .map_err(unavailable)?
        .bind(&ready.identity)
        .ok_or_else(expired)?;
    let schema = ready.artifact.token_schema().map_err(unavailable)?;
    let attributes: Arc<[_]> = publication_attributes
        .iter()
        .filter(|key| schema.keys().contains(key))
        .cloned()
        .collect();
    let mut unique_sentences: Vec<u64> = occurrences
        .iter()
        .map(|r| r.reference.sentence_index)
        .collect();
    unique_sentences.sort_unstable();
    unique_sentences.dedup();

    let decode_sentences = unique_sentences.clone();
    let artifact = Arc::clone(&ready.artifact);
    let pool = Arc::clone(&ready.pool);
    let decode_schema = schema.clone();
    let decode_attributes = Arc::clone(&attributes);
    let decode_task = tokio::task::spawn_blocking(move || {
        decode_projected_sentences(
            &artifact,
            &reader,
            pool,
            &decode_schema,
            &decode_attributes,
            decode_sentences,
            max_sentence_bytes,
        )
    });
    let provenance_task = provenance(ready, &unique_sentences);
    let (decoded_res, provenance_res) = tokio::join!(decode_task, provenance_task);
    let origins_vec = provenance_res?;
    let origins_by_sentence: HashMap<u64, Option<SentenceProvenance>> =
        unique_sentences.into_iter().zip(origins_vec).collect();
    let decoded_by_sentence = match decoded_res.map_err(unavailable)? {
        Ok(map) => map,
        Err(DecodeBatchError::Oversized) => {
            return Err(ApiError(
                StatusCode::PAYLOAD_TOO_LARGE,
                "This example is too large to display. Choose another example.".into(),
            ));
        }
        Err(DecodeBatchError::Unavailable(message)) => return Err(unavailable(message)),
    };

    let mut output = Vec::with_capacity(occurrences.len());
    let mut output_bytes = 0usize;
    for occurrence in occurrences {
        let sentence = occurrence.reference.sentence_index;
        let origin = origins_by_sentence.get(&sentence).and_then(Option::as_ref);
        let (decoded, tokens) = decoded_by_sentence
            .get(&sentence)
            .and_then(Option::as_ref)
            .ok_or_else(|| {
                ApiError(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "This example is too large to display. Choose another example.".into(),
                )
            })?;
        let range = occurrence.reference.span.clone();
        if range.end > tokens.len() || range.start >= range.end {
            return Err(invalid(
                "The saved match span could not be read. Run this query again.",
            ));
        }
        let projected_captures = occurrence.reference.captures.iter().enumerate().map(|(index,capture)|{
            let descriptor=captures.get(capture.capture_key.index()).ok_or_else(||invalid("The saved capture could not be read. Run this query again."))?;
            if capture.span.end>tokens.len() || capture.span.start>capture.span.end{return Err(invalid("The saved capture span could not be read. Run this query again."));}
            let mut value=json!({"name":descriptor.name,"hygienic_path":descriptor.path,"span":{"start":capture.span.start,"end":capture.span.end}});
            if let Some(Some(span))=occurrence.capture_char_spans.get(index){value["char_span"]=json!({"start":span.start,"end":span.end});}
            Ok(value)
        }).collect::<Result<Vec<_>>>()?;
        let mut row = json!({"sequence":occurrence.sequence,"pattern":patterns.get(occurrence.reference.pattern_key.index()).ok_or_else(||invalid("This pattern is not part of the search."))?,"span":{"start":range.start,"end":range.end},"captures":projected_captures,"corpus_sentence_ordinal":sentence,"source_name":if occurrence.source_name.is_empty(){origin.map(|p|p.location.source_name.as_ref()).unwrap_or_default()}else{occurrence.source_name.as_ref()},"document_ordinal":occurrence.document_ordinal,"document_sentence_ordinal":occurrence.document_sentence_ordinal,"surface":lingrep_core::token_match::display_text(decoded, range.clone()),"tokens":&tokens[range.clone()],"left_context":&tokens[range.start.saturating_sub(24)..range.start],"right_context":&tokens[range.end..range.end.saturating_add(24).min(tokens.len())]});
        if matches!(projection, OccurrenceOutput::WorkSamples) {
            use lingrep_core::token_match::display_text;
            let mut before = display_text(decoded, 0..range.start);
            if range.start > 0 && tokens[range.start].space_before {
                before.push(' ');
            }
            let mut after = String::new();
            if tokens
                .get(range.end)
                .is_some_and(|token| token.space_before)
            {
                after.push(' ');
            }
            after.push_str(&display_text(decoded, range.end..tokens.len()));
            row["sentence_text"] = json!([before, row["surface"], after]);
        }
        if let Some(span) = &occurrence.char_span {
            row["char_span"] = json!({"start":span.start,"end":span.end});
        }
        if !occurrence.reference.interpretations.is_empty() {
            row["interpretations"] = json!(occurrence.reference.interpretations);
        }
        if !occurrence.reference.graph_bindings.is_empty() {
            row["graph_bindings"] = json!(
                occurrence
                    .reference
                    .graph_bindings
                    .iter()
                    .map(graph_binding)
                    .collect::<Vec<_>>()
            );
        }
        if let Some(lexical) = &occurrence.reference.lexical {
            row["lexical"] = json!({"term":lexical.term,"matched_by":lexical.matched_by});
        }
        if let Some(unit) = &occurrence.unit {
            row["unit"] = json!(unit);
        }
        let key = occurrence
            .key
            .as_ref()
            .map(|fields| {
                fields
                    .iter()
                    .map(|f| json!({"kind":"scalar","name":f.name,"value":f.value}))
                    .collect::<Vec<_>>()
            })
            .or_else(|| {
                occurrence.lexical_key.as_ref().map(|fields| {
                    fields
                        .iter()
                        .map(|f| match &f.value {
                            LexicalProjectedKeyValue::Scalar(value) => {
                                json!({"kind":"scalar","name":f.name,"value":value})
                            }
                            LexicalProjectedKeyValue::Segments(values) => {
                                json!({"kind":"list","name":f.name,"values":values})
                            }
                        })
                        .collect::<Vec<_>>()
                })
            });
        if let Some(key) = key {
            row["key"] = json!(key);
        }
        if !occurrence.tags.is_empty() {
            row["tags"] = json!(
                occurrence
                    .tags
                    .iter()
                    .map(|(name, value)| json!({"name":name,"value":value}))
                    .collect::<Vec<_>>()
            );
        }
        let mut document = serde_json::Map::new();
        if let Some(metadata) = origin
            .and_then(|p| p.rich.metadata_json.as_deref())
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
        {
            for key in ["title", "author"] {
                if let Some(value) = metadata.get(key) {
                    document.insert(key.into(), value.clone());
                }
            }
        }
        if !document.is_empty() {
            row["source_metadata"] = json!({"document":document});
        }
        if let Some(metadata) = &occurrence.source_metadata {
            row["source_metadata"] = json!(metadata);
        }
        if let Some(limit) = max_response_bytes {
            output_bytes =
                output_bytes.saturating_add(serde_json::to_vec(&row).map_err(unavailable)?.len());
            if output_bytes > limit {
                return Err(ApiError(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "These examples are too large to display together. Request fewer examples."
                        .into(),
                ));
            }
        }
        output.push(row);
    }
    Ok((output, origins_by_sentence))
}

#[derive(Deserialize)]
struct OccurrenceQuery {
    pattern: String,
    sentence: u64,
    /// JSON array of the occurrence's roles, as frequency lines list them.
    roles: String,
    analysis: String,
    fingerprint: String,
}

/// Finds the occurrence a frequency line names: the completed run's match of
/// `pattern` in corpus sentence `sentence` with exactly `roles`. The run must
/// read the same corpus analysis and compile the same catalog as the table
/// the line came from.
async fn occurrence(
    State(store): State<RequestStore>,
    Path(id): Path<String>,
    query: std::result::Result<Query<OccurrenceQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>> {
    let Query(query) = query.map_err(|_| invalid("The occurrence address could not be read."))?;
    let roles: Vec<crate::occurrence::Role> = serde_json::from_str(&query.roles)
        .map_err(|_| invalid("The occurrence address could not be read."))?;
    let _permit = store.coordinators.acquire_reading_permit().await;
    let (snapshot, ready, index) = completed(&store, &id).await?;
    if ready.identity.as_str() != query.analysis {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "This corpus analysis differs from the one the frequency table counts. The occurrence cannot be located.".into(),
        ));
    }
    let source = snapshot.source.clone();
    let fingerprint = store
        .coordinators
        .pattern_task(move || crate::patterns::web_catalog_fingerprint(&source))
        .await
        .map_err(unavailable)?;
    if fingerprint != query.fingerprint {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "This search's catalog differs from the one the frequency table counts. The occurrence cannot be located.".into(),
        ));
    }
    let key = snapshot
        .patterns
        .iter()
        .find(|pattern| pattern.name == query.pattern)
        .map(|p| p.key)
        .ok_or_else(|| invalid("Choose a pattern from this search."))?;
    let found = index
        .sentence_occurrences(key, query.sentence)
        .await
        .map_err(unavailable)?
        .into_iter()
        .find(|row| {
            let captures = row.reference.captures.iter().filter_map(|capture| {
                let descriptor = snapshot.capture_catalog.get(capture.capture_key.index())?;
                Some((descriptor.name.as_str(), capture.span.clone()))
            });
            crate::occurrence::roles(captures, &row.reference.graph_bindings) == roles
        })
        .ok_or_else(|| {
            ApiError(
                StatusCode::NOT_FOUND,
                "This search has no occurrence in that sentence with the same words in the same roles.".into(),
            )
        })?;
    Ok(Json(
        json!({"document":found.document_ordinal,"sequence":found.sequence}),
    ))
}

#[derive(Default, Deserialize)]
struct PageQuery {
    pattern: String,
    document: Option<u64>,
    after: Option<i64>,
    limit: Option<usize>,
    documents_offset: Option<u64>,
    q: Option<String>,
    continuous: Option<u8>,
}
#[derive(Clone, Serialize)]
struct Document {
    ordinal: u64,
    title: String,
    author: String,
    matches: u64,
}
struct Inventory {
    documents: Vec<Document>,
    total: u64,
    offset: u64,
    selected: Option<Document>,
    position: Option<u64>,
    previous: Option<u64>,
    next: Option<u64>,
}

async fn document_inventory(
    index: &ResultIndex,
    ready: &ReadyCorpus,
    pattern: PatternKey,
    selected: Option<u64>,
    offset: Option<u64>,
    q: &str,
) -> Result<Inventory> {
    if q.is_empty() {
        return document_inventory_unfiltered(index, ready, pattern, selected, offset).await;
    }
    document_inventory_filtered(index, ready, pattern, selected, offset, q).await
}

/// Indexed path when no title/author substring filter is active: resolves the
/// selected document's rank and neighbors via SQLite indexes and fetches
/// provenance only for the single displayed 100-document page (plus the
/// selected document if off-page).
async fn document_inventory_unfiltered(
    index: &ResultIndex,
    ready: &ReadyCorpus,
    pattern: PatternKey,
    selected: Option<u64>,
    offset: Option<u64>,
) -> Result<Inventory> {
    let total = index.document_count(pattern).await.map_err(unavailable)?;
    let selected_row = match selected {
        Some(document) => index
            .document_entry(pattern, document)
            .await
            .map_err(unavailable)?,
        None => None,
    };
    let (position, previous, next) = match &selected_row {
        Some(row) => {
            let (position, previous, next) = index
                .document_neighbors(pattern, row.first_sequence)
                .await
                .map_err(unavailable)?;
            (Some(position), previous, next)
        }
        None => (None, None, None),
    };
    let resolved = offset
        .unwrap_or_else(|| position.unwrap_or(0) / 100 * 100)
        .min(total.saturating_sub(1) / 100 * 100);
    let page = if total == 0 {
        Vec::new()
    } else {
        index
            .documents(pattern, resolved, 100)
            .await
            .map_err(unavailable)?
            .documents
    };
    let mut sentences: Vec<u64> = page.iter().map(|d| d.first_sentence).collect();
    let extra_selected = selected_row
        .as_ref()
        .filter(|sel| !page.iter().any(|document| document.ordinal == sel.ordinal));
    if let Some(sel) = extra_selected {
        sentences.push(sel.first_sentence);
    }
    let origins = if sentences.is_empty() {
        Vec::new()
    } else {
        provenance(ready, &sentences).await?
    };
    let mut found = None;
    let mut documents = Vec::with_capacity(page.len());
    for (document, origin) in page.iter().zip(origins.iter()) {
        let (title, author) = document_labels(origin.as_ref());
        let item = Document {
            ordinal: document.ordinal,
            title,
            author,
            matches: document.matches,
        };
        if Some(document.ordinal) == selected {
            found = Some(item.clone());
        }
        documents.push(item);
    }
    if found.is_none()
        && let Some(sel) = extra_selected
    {
        let (title, author) = document_labels(origins.last().and_then(Option::as_ref));
        found = Some(Document {
            ordinal: sel.ordinal,
            title,
            author,
            matches: sel.matches,
        });
    }
    Ok(Inventory {
        documents,
        total,
        offset: resolved,
        selected: found,
        position,
        previous,
        next,
    })
}

/// Single-pass filtered document inventory with strictly bounded O(1) memory
/// (at most 200 [`Document`] entries retained regardless of corpus size).
async fn document_inventory_filtered(
    index: &ResultIndex,
    ready: &ReadyCorpus,
    pattern: PatternKey,
    selected: Option<u64>,
    offset: Option<u64>,
    q: &str,
) -> Result<Inventory> {
    let query = q.to_lowercase();
    let mut start = 0;
    let mut total = 0u64;
    let mut found = None;
    let mut position = None;
    let mut previous = None;
    let mut next = None;
    let mut prior = None;

    // Bounded page buffers (at most 200 items retained across the single pass):
    // - When `offset` is `Some(desired)`: retain `window` (`desired..desired+100`)
    //   and `last_page` (the final aligned 100-item page if `desired` clamps).
    // - When `offset` is `None`: retain `first_page` (`0..100`) for the fallback
    //   when `selected` is absent, and `current_page`/`selected_page` for the
    //   aligned 100-item page containing `selected`.
    let mut primary_page = Vec::with_capacity(100);
    let mut secondary_page = Vec::with_capacity(100);
    let mut selected_page_locked = false;

    loop {
        let page = index
            .documents(pattern, start, 100)
            .await
            .map_err(unavailable)?;
        if page.documents.is_empty() {
            break;
        }
        let origins = provenance(
            ready,
            &page
                .documents
                .iter()
                .map(|d| d.first_sentence)
                .collect::<Vec<_>>(),
        )
        .await?;
        for (document, origin) in page.documents.iter().zip(origins) {
            let (title, author) = document_labels(origin.as_ref());
            if !format!("{title}\n{author}").to_lowercase().contains(&query) {
                continue;
            }
            let item = Document {
                ordinal: document.ordinal,
                title,
                author,
                matches: document.matches,
            };
            let is_selected = Some(document.ordinal) == selected;
            if is_selected {
                found = Some(item.clone());
                position = Some(total);
                previous = prior;
            } else if found.is_some() && next.is_none() {
                next = Some(document.ordinal);
            }
            if let Some(desired) = offset {
                if total >= desired && primary_page.len() < 100 {
                    primary_page.push(item.clone());
                }
                if total.is_multiple_of(100) {
                    secondary_page.clear();
                }
                secondary_page.push(item);
            } else {
                if total < 100 {
                    primary_page.push(item.clone());
                }
                if !selected_page_locked {
                    if total.is_multiple_of(100) {
                        secondary_page.clear();
                    }
                    secondary_page.push(item);
                    if is_selected {
                        selected_page_locked = true;
                    }
                } else if secondary_page.len() < 100 {
                    secondary_page.push(item);
                }
            }
            total += 1;
            prior = Some(document.ordinal);
        }
        start += page.documents.len() as u64;
        if start >= page.total {
            break;
        }
    }
    let resolved = offset
        .unwrap_or_else(|| position.unwrap_or(0) / 100 * 100)
        .min(total.saturating_sub(1) / 100 * 100);
    let documents = match offset {
        Some(desired) if resolved == desired => primary_page,
        Some(_) => secondary_page,
        None if found.is_some() => secondary_page,
        None => primary_page,
    };
    Ok(Inventory {
        documents,
        total,
        offset: resolved,
        selected: found,
        position,
        previous,
        next,
    })
}

async fn documents(
    State(store): State<RequestStore>,
    Path(id): Path<String>,
    query: std::result::Result<Query<PageQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>> {
    let Query(query) = query.map_err(|_| {
        invalid("The results address could not be read. Choose a pattern or document again.")
    })?;
    if query.q.as_ref().is_some_and(|q| q.len() > 256) {
        return Err(invalid(
            "The document filter is too long. Use a shorter title or author.",
        ));
    }
    let _permit = store.coordinators.acquire_reading_permit().await;
    let (snapshot, ready, index) = completed(&store, &id).await?;
    let key = snapshot
        .patterns
        .iter()
        .find(|pattern| pattern.name == query.pattern)
        .map(|p| p.key)
        .ok_or_else(|| invalid("Choose a pattern from this search."))?;
    let continuous = query.continuous == Some(1);
    let scoped = if continuous { None } else { query.document };
    let mut after = query.after.unwrap_or(-1);
    let limit = query.limit.unwrap_or(20).max(1);
    let filter_q = query.q.as_deref().unwrap_or_default();
    if continuous
        && let Some(selected_doc) = query.document
        && after < 0
        && let Some(entry) = index
            .document_entry(key, selected_doc)
            .await
            .map_err(unavailable)?
    {
        let admitted = if filter_q.is_empty() {
            true
        } else {
            let origins = provenance(&ready, &[entry.first_sentence]).await?;
            let (title, author) = document_labels(origins[0].as_ref());
            format!("{title}\n{author}")
                .to_lowercase()
                .contains(&filter_q.to_lowercase())
        };
        if admitted {
            after = entry.first_sequence as i64 - 1;
        }
    }
    let page = index
        .page(key, scoped, after, limit)
        .await
        .map_err(unavailable)?;
    let document = page
        .rows
        .first()
        .map(|row| row.document_ordinal)
        .or(query.document);
    let inventory = document_inventory(
        &index,
        &ready,
        key,
        document,
        query.documents_offset,
        filter_q,
    )
    .await?;
    let patterns = snapshot
        .patterns
        .iter()
        .map(|p| p.name.clone())
        .collect::<Vec<_>>();
    let (rows, origins_by_sentence) = project_occurrences_with_provenance(
        &ready,
        &page.rows,
        &snapshot.capture_catalog,
        &patterns,
        index.publication_attributes(),
        INSPECTION_BYTES,
        OccurrenceOutput::DocumentPage,
    )
    .await?;
    let mut landmark_rows = Vec::new();
    let mut seen = HashSet::new();
    for row in &page.rows {
        if seen.insert(row.document_ordinal) {
            landmark_rows.push((row.document_ordinal, row.reference.sentence_index));
        }
    }
    let landmark_docs: Vec<u64> = landmark_rows.iter().map(|&(doc, _)| doc).collect();
    let landmark_counts = index
        .document_matches(key, &landmark_docs)
        .await
        .map_err(unavailable)?;
    let mut landmarks = Vec::with_capacity(landmark_rows.len());
    for (doc, sentence) in landmark_rows {
        let count = landmark_counts.get(&doc).copied().unwrap_or(0);
        let origin = origins_by_sentence.get(&sentence).and_then(Option::as_ref);
        let (title, author) = document_labels(origin);
        landmarks.push(Document {
            ordinal: doc,
            title,
            author,
            matches: count,
        });
    }
    Ok(Json(
        json!({"rows":rows,"landmarks":landmarks,"after":after,"start":page.start,"matches_total":page.matches_total,"previous":page.previous,"next":page.next,"document":document,"documents":inventory.documents,"documents_total":inventory.total,"documents_offset":inventory.offset,"selected":inventory.selected,"position":inventory.position,"previous_document":inventory.previous,"next_document":inventory.next}),
    ))
}

#[derive(Deserialize)]
struct WorkSampleQuery {
    pattern: String,
    document: Option<u64>,
    offset: Option<u64>,
    after: Option<i64>,
}

struct WorkSamplePage {
    ordinal: u64,
    matches: u64,
    next: Option<u64>,
    first_sentence: u64,
    rows: usize,
}

async fn work_samples(
    State(store): State<RequestStore>,
    Path(id): Path<String>,
    query: std::result::Result<Query<WorkSampleQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>> {
    const WORKS_PER_PAGE: usize = 5;
    const MATCHES_PER_WORK: usize = 3;

    let Query(query) =
        query.map_err(|_| invalid("Choose a valid position in the work samples."))?;
    let after = query.after.unwrap_or(-1);
    let offset = query.offset.unwrap_or(0);
    if after < -1
        || after >= 0 && query.document.is_none()
        || offset > i64::MAX as u64
        || query
            .document
            .is_some_and(|ordinal| ordinal > i64::MAX as u64)
    {
        return Err(invalid("Choose a valid position in the work samples."));
    }
    let _permit = store.coordinators.acquire_reading_permit().await;
    let (snapshot, ready, index) = completed(&store, &id).await?;
    let pattern = snapshot
        .patterns
        .iter()
        .find(|pattern| pattern.name == query.pattern)
        .ok_or_else(|| invalid("Choose a pattern from this search."))?
        .key;
    let (documents, total, next) = if let Some(document) = query.document {
        (vec![document], None, None)
    } else {
        let page = index
            .documents(pattern, offset, WORKS_PER_PAGE)
            .await
            .map_err(unavailable)?;
        let end = offset + page.documents.len() as u64;
        (
            page.documents
                .iter()
                .map(|document| document.ordinal)
                .collect(),
            Some(page.total),
            (end < page.total).then_some(end),
        )
    };
    let mut pages = Vec::with_capacity(documents.len());
    let mut occurrences = Vec::new();
    for ordinal in documents {
        let page = index
            .page(pattern, Some(ordinal), after, MATCHES_PER_WORK)
            .await
            .map_err(unavailable)?;
        if let Some(first) = page.rows.first() {
            pages.push(WorkSamplePage {
                ordinal,
                matches: page.matches_total,
                next: page.next,
                first_sentence: first.reference.sentence_index,
                rows: page.rows.len(),
            });
            occurrences.extend(page.rows);
        }
    }
    let patterns = snapshot
        .patterns
        .iter()
        .map(|p| p.name.clone())
        .collect::<Vec<_>>();
    let (projected, origins_by_sentence) = project_occurrences_with_provenance(
        &ready,
        &occurrences,
        &snapshot.capture_catalog,
        &patterns,
        index.publication_attributes(),
        INSPECTION_BYTES,
        OccurrenceOutput::WorkSamples,
    )
    .await?;
    let mut rows = projected.into_iter();
    let groups: Vec<_> = pages
        .into_iter()
        .map(|page| {
            let origin = origins_by_sentence
                .get(&page.first_sentence)
                .and_then(Option::as_ref);
            let (title, author) = document_labels(origin);
            let edition = document_field(document_metadata(origin).as_ref(), "orthographic_style");
            let document = Document {
                ordinal: page.ordinal,
                title,
                author,
                matches: page.matches,
            };
            let rows: Vec<_> = rows.by_ref().take(page.rows).collect();
            json!({"document":document,"edition":edition,"rows":rows,"next":page.next})
        })
        .collect();
    Ok(Json(json!({"groups":groups,"total":total,"next":next})))
}

async fn export(State(store): State<RequestStore>, Path(id): Path<String>) -> Result<Response> {
    let (snapshot, ready, index) = {
        let _permit = store.coordinators.acquire_reading_permit().await;
        completed(&store, &id).await?
    };
    let coordinators = store.coordinators.clone();
    let patterns = snapshot
        .patterns
        .iter()
        .map(|p| p.name.clone())
        .collect::<Vec<_>>();
    let mut data = json!({"analysis_identity":lingrep_corpus::format_identity(ready.artifact.analysis_identity().as_bytes()),"source":snapshot.source});
    if let Some(vectors) = &snapshot.vectors_binding {
        data["vectors_blake3"] = json!(vectors.digest.to_hex().as_str());
    }
    let prefix = vec![json!({"schema":"lingrep.question-results/v1","type":"run","data":data})];
    let stream = futures::stream::try_unfold(
        (
            snapshot,
            ready,
            index,
            patterns,
            -1_i64,
            Some(prefix),
            coordinators,
        ),
        |(snapshot, ready, index, patterns, cursor, prefix, coordinators)| async move {
            if cursor == i64::MAX {
                return Ok(None);
            }
            let (rows, next, finished) = if let Some(prefix) = prefix {
                (prefix, cursor, false)
            } else {
                // Release the read permit before yielding each page to the HTTP stream.
                let _permit = coordinators.acquire_reading_permit().await;
                let page = index
                    .export_page(cursor, 16)
                    .await
                    .map_err(std::io::Error::other)?;
                if page.is_empty() {
                    (
                        vec![json!({
                            "schema": "lingrep.question-results/v1",
                            "type": "complete",
                            "data": {
                                "analysis_identity": lingrep_corpus::format_identity(ready.artifact.analysis_identity().as_bytes()),
                                "counters": {
                                    "matches_found": snapshot.patterns.iter().map(|p| p.exact_total).sum::<u64>()
                                }
                            }
                        })],
                        cursor,
                        true,
                    )
                } else {
                    let next = page.last().expect("nonempty page").sequence as i64;
                    let values = project_occurrences(
                        &ready,
                        &page,
                        &snapshot.capture_catalog,
                        &patterns,
                        index.publication_attributes(),
                        usize::MAX,
                        OccurrenceOutput::Matches,
                    )
                    .await
                    .map_err(|e| std::io::Error::other(e.1))?;
                    (
                        values
                            .into_iter()
                            .map(|row| json!({"schema":"lingrep.question-results/v1","type":"match","data":row}))
                            .collect(),
                        next,
                        false,
                    )
                }
            };
            let mut bytes = Vec::new();
            for row in rows {
                serde_json::to_writer(&mut bytes, &row).map_err(std::io::Error::other)?;
                bytes.push(b'\n');
            }
            Ok::<_, std::io::Error>(Some((
                bytes,
                (
                    snapshot,
                    ready,
                    index,
                    patterns,
                    if finished { i64::MAX } else { next },
                    None,
                    coordinators,
                ),
            )))
        },
    )
    .inspect(move |item| {
        if let Err(error) = item {
            tracing::error!(run = %id, %error, "question result export stream failed");
        }
    })
    .take_while(|item| futures::future::ready(!matches!(item, Ok(bytes) if bytes.is_empty())));

    Ok((
        [
            (header::CONTENT_TYPE, "application/x-ndjson; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=matches.jsonl",
            ),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response())
}

/// The published definition names of `source` in catalog order, or none when
/// it does not compile.
fn catalog_definitions(source: &str) -> Vec<String> {
    crate::patterns::compile_web_pattern(source)
        .map(|compiled| {
            crate::pattern_catalog::pattern_metadata_pairs(&compiled.matcher)
                .into_iter()
                .map(|(name, _)| name)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_untitled_document_is_named_by_its_source_file() {
        assert_eq!(
            super::document_title(String::new(), "Mori_O_Abeichizoku.conllu"),
            "Mori_O_Abeichizoku"
        );
        assert_eq!(super::document_title(String::new(), "notes.MD"), "notes");
        assert_eq!(super::document_title(String::new(), "a.b.tei"), "a.b.tei");
        assert_eq!(
            super::document_title("阿部一族".into(), "x.conllu"),
            "阿部一族"
        );
    }
    #[test]
    fn an_overflowed_run_lists_its_catalog_definitions_in_catalog_order() {
        let source =
            "key: [upos]\npos := [].\nkey: [lemma]\nnouns := [upos=NOUN].\nverbs := [upos=VERB].\n";
        assert_eq!(
            super::catalog_definitions(source),
            ["pos", "nouns", "verbs"]
        );
        assert!(super::catalog_definitions("npv := [upos=VERB").is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn batched_project_occurrences_and_document_inventory_match_reference_oracle() {
        use std::path::PathBuf;
        use std::sync::Arc;

        use lingrep_core::corpus::{
            AdmittedCorpusEvent, AdmittedSentence, DiscoveryReport, DocumentId, InventoryStats,
            MetadataScalar, MetadataValue, SourceDocument, TraversalHighWater, TraversalLimits,
        };
        use lingrep_core::spill::TempBudget;
        use lingrep_core::token_match::{AttributeKey, TokenInfo};
        use lingrep_corpus::{
            AdmissionIdentity, AnalysisInputs, BuildConfig, CorpusBuilder, DictionaryFingerprint,
            ReaderIdentity, TrustClass,
        };

        use crate::corpus_runtime::{CorpusIdentity, CorpusRuntime};
        use crate::result_index::ResultIndexWriter;
        use crate::server_context::ServerContext;
        use crate::snapshot::{MatchRef, PatternKey};

        let docs = 250usize;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("parity.lingcorpus");
        let mut builder = CorpusBuilder::begin(
            BuildConfig {
                analysis: AnalysisInputs::new(
                    ReaderIdentity::from_bytes([1; 32]),
                    AdmissionIdentity::from_bytes([2; 32]),
                    &lingrep_corpus::vibrato_fixture_descriptor(),
                ),
                analyzer: lingrep_corpus::AnalyzerRecord::vibrato_fixture(),
                dictionary: DictionaryFingerprint::from_bytes([3; 32]),
                trust: TrustClass::LocallyExecuted,
                temp_budget: Arc::new(TempBudget::new(32 * 1024 * 1024)),
                sentence_metadata_keys: Vec::new(),
            },
            &destination,
        )
        .unwrap();
        let sentence = vec![
            TokenInfo::default()
                .with_orth("走る")
                .with_lemma("走る")
                .with_pos("VERB"),
            TokenInfo::default()
                .with_orth("猫")
                .with_lemma("猫")
                .with_pos("NOUN"),
            TokenInfo::default().with_orth("。"),
        ];
        let observation = builder.source_observation();
        let mut inv_digest = blake3::Hasher::new();
        for doc in 0..docs {
            let logical = format!("doc_{doc:03}.md");
            inv_digest.update(&(logical.len() as u64).to_le_bytes());
            inv_digest.update(logical.as_bytes());
            builder
                .register_source_document(&SourceDocument {
                    ordinal: doc as u64,
                    logical_name: logical.clone(),
                    path: PathBuf::from(format!("/unused/{logical}")),
                    target_group_ordinal: doc as u64,
                    external_target: false,
                })
                .unwrap();
            observation.begin_source(&logical).unwrap();
            observation.update_source_bytes(logical.as_bytes()).unwrap();
            observation.end_source().unwrap();
            builder
                .push_corpus_event(&AdmittedCorpusEvent::DocumentStart {
                    id: DocumentId::new(doc as u64),
                    source_name: logical.clone(),
                    metadata: vec![
                        (
                            "title".into(),
                            MetadataValue::Scalar(MetadataScalar::String(format!("作品_{doc:03}"))),
                        ),
                        (
                            "author".into(),
                            MetadataValue::Scalar(MetadataScalar::String(format!(
                                "著者_{}",
                                doc % 2
                            ))),
                        ),
                    ]
                    .into_boxed_slice(),
                })
                .unwrap();
            let loc = builder
                .push_corpus_event(&AdmittedCorpusEvent::Sentence(AdmittedSentence {
                    annotations: Box::new([]),
                    structure: Default::default(),
                    id: lingrep_core::corpus::SentenceId::new(doc as u64),
                    document_id: DocumentId::new(doc as u64),
                    document_sentence_ordinal: 0,
                    text: String::new(),
                    words: None,
                    enhanced_graph: None,
                    active_labels: Box::new([]),
                    metadata: Box::new([]),
                }))
                .unwrap()
                .unwrap();
            builder.push_tokenized_sentence(&loc, &sentence).unwrap();
            builder
                .push_corpus_event(&AdmittedCorpusEvent::DocumentEnd {
                    id: DocumentId::new(doc as u64),
                })
                .unwrap();
        }
        let artifact = builder
            .finish(DiscoveryReport {
                inventory: InventoryStats {
                    documents: docs as u64,
                    distinct_targets: docs as u64,
                    alias_documents: 0,
                    max_target_multiplicity: 1,
                    external_targets: 0,
                },
                configured_limits: TraversalLimits::default(),
                observed: TraversalHighWater::default(),
                ordered_inventory_digest: *inv_digest.finalize().as_bytes(),
            })
            .unwrap();
        let identity = lingrep_corpus::format_identity(artifact.analysis_identity().as_bytes());
        let context = Arc::new(ServerContext::new("/unused/parity.dic").unwrap());
        let runtime = CorpusRuntime::from_ready(CorpusIdentity::new(&identity), artifact, context);
        let ready = runtime.ready().await.unwrap();
        let pub_attrs: Arc<[AttributeKey]> = Arc::from([
            AttributeKey::new("text").unwrap(),
            AttributeKey::new("lemma").unwrap(),
            AttributeKey::new("upos").unwrap(),
        ]);
        let pat0 = PatternKey::new(0).unwrap();
        let pub_attrs_bg = Arc::clone(&pub_attrs);
        let (index, occurrences) = tokio::task::spawn_blocking(move || {
            let mut writer = ResultIndexWriter::new(pub_attrs_bg).unwrap();
            let mut occs = Vec::new();
            for doc in 0..docs as u64 {
                let mut occ = super::ResultOccurrence::from_reference(
                    doc,
                    MatchRef {
                        sentence_index: doc,
                        span: 0..2,
                        pattern_key: pat0,
                        captures: Box::default(),
                        graph_bindings: Box::default(),
                        interpretations: Box::default(),
                        lexical: None,
                        in_chain: false,
                    },
                );
                occ.source_name = Arc::from(format!("doc_{doc:03}.md"));
                writer.append_occurrence(&mut occ).unwrap();
                occs.push(occ);
            }
            (writer.finish().unwrap(), occs)
        })
        .await
        .unwrap();

        // 1. Batched project_occurrences matches single-row projection identically (including duplicate sentence ordinals).
        let patterns = vec!["p0".to_owned()];
        let batch_input = vec![
            occurrences[0].clone(),
            occurrences[0].clone(),
            occurrences[120].clone(),
            occurrences[249].clone(),
        ];
        let batched = super::project_occurrences(
            &ready,
            &batch_input,
            &[],
            &patterns,
            &pub_attrs,
            super::INSPECTION_BYTES,
            super::OccurrenceOutput::Matches,
        )
        .await
        .ok()
        .unwrap();
        let mut single = Vec::new();
        for occ in &batch_input {
            single.extend(
                super::project_occurrences(
                    &ready,
                    std::slice::from_ref(occ),
                    &[],
                    &patterns,
                    &pub_attrs,
                    super::INSPECTION_BYTES,
                    super::OccurrenceOutput::Matches,
                )
                .await
                .ok()
                .unwrap(),
            );
        }
        assert_eq!(batched, single);

        // 2. Unfiltered and filtered document_inventory cover auto-page jump, clamped offset, unaligned offset, and absent selection.
        for q in ["", "著者_1"] {
            let matching: Vec<u64> = (0..docs as u64)
                .filter(|d| q.is_empty() || d % 2 == 1)
                .collect();
            let cases = [
                (None, None),
                (Some(matching[115]), None),
                (Some(matching[0]), Some(500)),
                (Some(matching[10]), Some(25)),
                (Some(999), None),
                (Some(999), Some(500)),
            ];
            for (sel, off) in cases {
                let inv = super::document_inventory(&index, &ready, pat0, sel, off, q)
                    .await
                    .ok()
                    .unwrap();
                let expected_pos =
                    sel.and_then(|s| matching.iter().position(|&d| d == s).map(|p| p as u64));
                let expected_offset = off
                    .unwrap_or_else(|| expected_pos.unwrap_or(0) / 100 * 100)
                    .min((matching.len() as u64).saturating_sub(1) / 100 * 100);
                let expected_docs: Vec<u64> = matching
                    .iter()
                    .skip(expected_offset as usize)
                    .take(100)
                    .copied()
                    .collect();
                assert_eq!(
                    inv.total,
                    matching.len() as u64,
                    "q={q:?} sel={sel:?} off={off:?}"
                );
                assert_eq!(
                    inv.offset, expected_offset,
                    "q={q:?} sel={sel:?} off={off:?}"
                );
                assert_eq!(
                    inv.position, expected_pos,
                    "q={q:?} sel={sel:?} off={off:?}"
                );
                assert_eq!(
                    inv.previous,
                    expected_pos
                        .and_then(|p| p.checked_sub(1))
                        .map(|p| matching[p as usize]),
                    "q={q:?} sel={sel:?} off={off:?}"
                );
                assert_eq!(
                    inv.next,
                    expected_pos.and_then(|p| matching.get(p as usize + 1).copied()),
                    "q={q:?} sel={sel:?} off={off:?}"
                );
                assert_eq!(
                    inv.documents.iter().map(|d| d.ordinal).collect::<Vec<_>>(),
                    expected_docs,
                    "q={q:?} sel={sel:?} off={off:?}"
                );
                assert_eq!(
                    inv.selected.as_ref().map(|d| d.ordinal),
                    expected_pos.map(|p| matching[p as usize]),
                    "q={q:?} sel={sel:?} off={off:?}"
                );
            }
        }
    }
}
