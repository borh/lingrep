use std::collections::{BTreeMap, HashSet};
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
use lingrep_core::execution::SentenceAdmission;
use lingrep_core::fsm::{GraphBinding, LexicalProjectedKeyValue};
use lingrep_corpus::SentenceProvenance;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::coordinator::{CoordinatorView, RevisionRequest};
use crate::corpus_runtime::ReadyCorpus;
use crate::question_corpus::token_rows;
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
    let slot = store.corpora.resolve(Some(&body.dataset)).ok_or_else(|| {
        invalid("This corpus is not available on this server. Choose another corpus.")
    })?;
    slot.runtime.start_or_observe().await;
    let clause = body.condition;
    let scope = crate::search_scope::SearchScope::parse(Some(
        &json!({"clause":if clause.trim().is_empty(){None}else{Some(clause)}}).to_string(),
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
                retained: Some(30),
                document_reach: true,
                search_scope: scope,
                index_results: true,
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

async fn project_run(
    store: &RequestStore,
    id: &str,
    view: &CoordinatorView,
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
                let group = project_occurrences(
                    ready,
                    &page.rows,
                    &snapshot.capture_catalog,
                    &definitions,
                    index.publication_attributes(),
                    None,
                )
                .await?;
                preview_bytes = preview_bytes
                    .saturating_add(serde_json::to_vec(&group).map_err(unavailable)?.len());
                if preview_bytes > PREVIEW_BYTES {
                    return Err(ApiError(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "The result preview is too large to display. Search fewer patterns.".into(),
                    ));
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
            for occurrence in summary.preview.iter() {
                let name = definitions
                    .get(occurrence.reference.pattern_key.index())
                    .cloned()
                    .unwrap_or_default();
                let group = project_occurrences(
                    ready,
                    std::slice::from_ref(occurrence),
                    &summary.capture_catalog,
                    &definitions,
                    &summary.publication_attributes,
                    None,
                )
                .await?;
                preview_bytes = preview_bytes
                    .saturating_add(serde_json::to_vec(&group).map_err(unavailable)?.len());
                if preview_bytes > PREVIEW_BYTES {
                    return Err(ApiError(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "The result preview is too large to display. Search fewer patterns.".into(),
                    ));
                }
                rows.entry(name).or_insert_with(Vec::new).extend(group);
            }
        }
    }
    if let Some(ready) = &current_ready {
        contract_run = Some(
            json!({"analysis_identity":lingrep_corpus::format_identity(ready.artifact.analysis_identity().as_bytes())}),
        );
    }
    let diagnostics = if attempt
        .failure
        .as_ref()
        .is_some_and(|failure| failure.category == ErrorCategory::Compile)
    {
        let source = attempt.source.clone();
        tokio::task::spawn_blocking(move||crate::patterns::compile_web_pattern(&source).err().map(|error|error.diagnostics.into_iter().map(|diagnostic|{
            let location=diagnostic.location();let span=location.byte_range();
            json!({"schema":"lingrep.diagnostic/v1","type":"diagnostic","data":{"stage":"catalog","severity":"error","message":diagnostic.message,"location":{"source_name":location.source_name(),"source_span":{"start":span.start,"end":span.end},"line":location.line(),"column":location.column()}}})
        }).collect::<Vec<_>>()).unwrap_or_default()).await.map_err(unavailable)?
    } else {
        Vec::new()
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
        error: attempt.failure.as_ref().map(|e| e.message.clone()),
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
        (store, id, receiver, None::<Value>, false, false),
        |(store, id, mut receiver, prior, wait, finished)| async move {
            if finished {
                return None;
            }
            if wait && receiver.changed().await.is_err() {
                return None;
            }
            let view = receiver.borrow_and_update().clone();
            let projected = match project_run(&store, &id, &view).await {
                Ok(projected) => projected,
                Err(error) => {
                    let event = Event::default()
                        .event("update")
                        .data(json!({"id":id,"reset":false,"patch":{"phase":"failed","error":error.1,"version":VERSION.fetch_add(1,Ordering::Relaxed)},"append":{}}).to_string());
                    return Some((
                        Ok::<_, Infallible>(event),
                        (store, id, receiver, prior, false, true),
                    ));
                }
            };
            let finished = projected.phase != "running";
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
            if let Some(old) = &prior {
                patch.retain(|key, value| key == "version" || old.get(key) != Some(value));
            }
            let event = Event::default()
                .event("update")
                .data(json!({"id":id,"reset":reset,"patch":patch,"append":append}).to_string());
            Some((
                Ok::<_, Infallible>(event),
                (store, id, receiver, Some(value), true, finished),
            ))
        },
    );
    Ok(Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response())
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
fn document_labels(provenance: Option<&SentenceProvenance>) -> (String, String) {
    let metadata = provenance
        .and_then(|p| p.rich.metadata_json.as_deref())
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    let field = |key: &str| {
        metadata
            .as_ref()
            .and_then(|v| v.get(key))
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
            .unwrap_or_default()
    };
    (field("title"), field("author"))
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
async fn project_occurrences(
    ready: &ReadyCorpus,
    occurrences: &[ResultOccurrence],
    captures: &[CaptureDescriptor],
    patterns: &[String],
    publication_attributes: &[lingrep_core::token_match::AttributeKey],
    max_response_bytes: Option<usize>,
) -> Result<Vec<Value>> {
    let reader = ready
        .reader()
        .map_err(unavailable)?
        .bind(&ready.identity)
        .ok_or_else(expired)?;
    let provenance = provenance(
        ready,
        &occurrences
            .iter()
            .map(|r| r.reference.sentence_index)
            .collect::<Vec<_>>(),
    )
    .await?;
    let schema = ready.artifact.token_schema().map_err(unavailable)?;
    let attributes: Arc<[_]> = publication_attributes
        .iter()
        .filter(|key| schema.keys().contains(key))
        .cloned()
        .collect();
    let mut output = Vec::with_capacity(occurrences.len());
    let mut output_bytes = 0usize;
    for (occurrence, origin) in occurrences.iter().zip(provenance) {
        let reader = reader.clone();
        let sentence = occurrence.reference.sentence_index;
        let attributes = Arc::clone(&attributes);
        let decoded = tokio::task::spawn_blocking(move || {
            reader.sentence_projection_bounded(sentence, &attributes, INSPECTION_BYTES)
        })
        .await
        .map_err(unavailable)?
        .map_err(unavailable)?
        .ok_or_else(|| {
            ApiError(
                StatusCode::PAYLOAD_TOO_LARGE,
                "This example is too large to display. Choose another example.".into(),
            )
        })?;
        let tokens = token_rows(&decoded, &schema);
        let range = occurrence.reference.span.clone();
        if range.end > tokens.len() || range.start >= range.end {
            return Err(invalid(
                "The saved match span could not be read. Run this query again.",
            ));
        }
        let text = |range: std::ops::Range<usize>| {
            tokens[range]
                .iter()
                .map(|token| {
                    token
                        .attributes
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                })
                .collect::<String>()
        };
        let projected_captures = occurrence.reference.captures.iter().enumerate().map(|(index,capture)|{
            let descriptor=captures.get(capture.capture_key.index()).ok_or_else(||invalid("The saved capture could not be read. Run this query again."))?;
            if capture.span.end>tokens.len() || capture.span.start>capture.span.end{return Err(invalid("The saved capture span could not be read. Run this query again."));}
            let mut value=json!({"name":descriptor.name,"hygienic_path":descriptor.path,"span":{"start":capture.span.start,"end":capture.span.end}});
            if let Some(Some(span))=occurrence.capture_char_spans.get(index){value["char_span"]=json!({"start":span.start,"end":span.end});}
            Ok(value)
        }).collect::<Result<Vec<_>>>()?;
        let mut row = json!({"sequence":occurrence.sequence,"pattern":patterns.get(occurrence.reference.pattern_key.index()).ok_or_else(||invalid("This pattern is not part of the search."))?,"span":{"start":range.start,"end":range.end},"captures":projected_captures,"corpus_sentence_ordinal":sentence,"source_name":if occurrence.source_name.is_empty(){origin.as_ref().map(|p|p.location.source_name.as_ref()).unwrap_or_default()}else{occurrence.source_name.as_ref()},"document_ordinal":occurrence.document_ordinal,"document_sentence_ordinal":occurrence.document_sentence_ordinal,"surface":text(range.clone()),"tokens":&tokens[range.clone()],"left_context":&tokens[range.start.saturating_sub(24)..range.start],"right_context":&tokens[range.end..range.end.saturating_add(24).min(tokens.len())]});
        if let Some(span) = &occurrence.char_span {
            row["char_span"] = json!({"start":span.start,"end":span.end});
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
            .as_ref()
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
    Ok(output)
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
    if crate::patterns::web_catalog_fingerprint(&snapshot.source) != query.fingerprint {
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
    first_sequence: Option<u64>,
}
async fn document_inventory(
    index: &ResultIndex,
    ready: &ReadyCorpus,
    pattern: PatternKey,
    selected: Option<u64>,
    offset: Option<u64>,
    q: &str,
) -> Result<Inventory> {
    let query = q.to_lowercase();
    let mut start = 0;
    let mut total = 0;
    let mut result = Vec::new();
    let mut found = None;
    let mut position = None;
    let mut previous = None;
    let mut next = None;
    let mut prior = None;
    let mut first_sequence = None;
    let desired = offset.unwrap_or(0);
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
            if Some(document.ordinal) == selected {
                found = Some(item.clone());
                position = Some(total);
                previous = prior;
                first_sequence = Some(document.first_sequence);
            } else if found.is_some() && next.is_none() {
                next = Some(document.ordinal);
            }
            if total >= desired && result.len() < 100 {
                result.push(item);
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
    if resolved != desired {
        return Box::pin(document_inventory(
            index,
            ready,
            pattern,
            selected,
            Some(resolved),
            q,
        ))
        .await;
    }
    Ok(Inventory {
        documents: result,
        total,
        offset: desired,
        selected: found,
        position,
        previous,
        next,
        first_sequence,
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
    let mut inventory = document_inventory(
        &index,
        &ready,
        key,
        query.document,
        query.documents_offset,
        query.q.as_deref().unwrap_or_default(),
    )
    .await?;
    if continuous
        && query.document.is_some()
        && after < 0
        && let Some(first) = inventory.first_sequence
    {
        after = first as i64 - 1;
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
    if document != query.document {
        inventory = document_inventory(
            &index,
            &ready,
            key,
            document,
            query.documents_offset,
            query.q.as_deref().unwrap_or_default(),
        )
        .await?;
    }
    let patterns = snapshot
        .patterns
        .iter()
        .map(|p| p.name.clone())
        .collect::<Vec<_>>();
    let rows = project_occurrences(
        &ready,
        &page.rows,
        &snapshot.capture_catalog,
        &patterns,
        index.publication_attributes(),
        Some(32 * 1024 * 1024),
    )
    .await?;
    let mut landmarks = Vec::new();
    let mut seen = HashSet::new();
    for row in &page.rows {
        if seen.insert(row.document_ordinal) {
            let count = index
                .count(key, Some(row.document_ordinal))
                .await
                .map_err(unavailable)?;
            let origin = provenance(&ready, &[row.reference.sentence_index]).await?;
            let (title, author) = document_labels(origin[0].as_ref());
            landmarks.push(Document {
                ordinal: row.document_ordinal,
                title,
                author,
                matches: count,
            });
        }
    }
    Ok(Json(
        json!({"rows":rows,"landmarks":landmarks,"after":after,"start":page.start,"matches_total":page.matches_total,"previous":page.previous,"next":page.next,"document":document,"documents":inventory.documents,"documents_total":inventory.total,"documents_offset":inventory.offset,"selected":inventory.selected,"position":inventory.position,"previous_document":inventory.previous,"next_document":inventory.next}),
    ))
}

async fn export(State(store): State<RequestStore>, Path(id): Path<String>) -> Result<Response> {
    let permit = store.coordinators.acquire_reading_permit().await;
    let (snapshot, ready, index) = completed(&store, &id).await?;
    let patterns = snapshot
        .patterns
        .iter()
        .map(|p| p.name.clone())
        .collect::<Vec<_>>();
    let prefix = vec![
        json!({"schema":"lingrep.question-results/v1","type":"run","data":{"analysis_identity":lingrep_corpus::format_identity(ready.artifact.analysis_identity().as_bytes()),"source":snapshot.source}}),
    ];
    let stream=futures::stream::try_unfold((snapshot,ready,index,patterns,-1_i64,Some(prefix),permit),|(snapshot,ready,index,patterns,cursor,prefix,permit)|async move{
        if cursor==i64::MAX{return Ok(None);}
        let (rows,next,finished)=if let Some(prefix)=prefix{(prefix,cursor,false)}else{
            let page=index.export_page(cursor,16).await.map_err(std::io::Error::other)?;
            if page.is_empty(){(vec![json!({"schema":"lingrep.question-results/v1","type":"complete","data":{"analysis_identity":lingrep_corpus::format_identity(ready.artifact.analysis_identity().as_bytes()),"counters":{"matches_found":snapshot.patterns.iter().map(|p|p.exact_total).sum::<u64>()}}})],cursor,true)}else{
                let next=page.last().expect("nonempty page").sequence as i64;
                let values=project_occurrences(&ready,&page,&snapshot.capture_catalog,&patterns,index.publication_attributes(),None).await.map_err(|e|std::io::Error::other(e.1))?;
                (values.into_iter().map(|row|json!({"schema":"lingrep.question-results/v1","type":"match","data":row})).collect(),next,false)
            }
        };
        let mut bytes=Vec::new();for row in rows{serde_json::to_writer(&mut bytes,&row).map_err(std::io::Error::other)?;bytes.push(b'\n');}
        Ok::<_,std::io::Error>(Some((bytes,(snapshot,ready,index,patterns,if finished{i64::MAX}else{next},None,permit))))
    }).inspect(move |item| {
        if let Err(error) = item {
            tracing::error!(run = %id, %error, "question result export stream failed");
        }
    }).take_while(|item|futures::future::ready(!matches!(item,Ok(bytes) if bytes.is_empty())));

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
