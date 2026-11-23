use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, LazyLock};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use lingrep_core::token_match::{
    AttributeValueRef, BoundTokenSentence, TokenSchema, TokenSequence,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::corpus_runtime::{CacheReadiness, CorpusRuntime, ReadyCorpus};
use crate::state::RequestStore;

const MAX_VALUES: usize = 100;
const MAX_FIELDS: usize = 256;
const MAX_VALUE_BYTES: usize = 4096;
const MAX_QUERY_BYTES: usize = 256;
const MAX_CURSOR_BYTES: usize = MAX_VALUE_BYTES * 12 + 128;
const MAX_TOKEN_BYTES: usize = 1024 * 1024;
const MAX_JSON_BYTES: usize = 4 * 1024 * 1024;

pub(crate) fn routes() -> Router<RequestStore> {
    Router::new()
        .route("/config", get(config))
        .route("/data/{corpus}/tokens", get(tokens))
        .route("/data/{corpus}/metadata", get(metadata))
}

#[derive(Serialize)]
struct Dataset {
    label: String,
    documents: Option<u64>,
    sentences: Option<u64>,
    tokens: Option<u64>,
    analysis_identity: Option<String>,
    readiness: &'static str,
}

#[derive(Serialize)]
struct Configuration {
    datasets: BTreeMap<String, Dataset>,
    source: String,
    /// The published definitions of `source`, for the question page's definition menu.
    definitions: Vec<crate::patterns::QuestionDefinition>,
    catalog_fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    vectors_blake3: Option<String>,
    labels: Value,
    value_names: &'static Value,
}

async fn config(State(store): State<RequestStore>) -> Response {
    let mut datasets = BTreeMap::new();
    for (name, slot) in store.corpora.iter() {
        let state = slot.runtime.readiness().borrow().clone();
        let ready = slot.runtime.ready().await;
        let (documents, sentences, tokens, analysis_identity) =
            ready.as_ref().map_or((None, None, None, None), |ready| {
                (
                    Some(ready.artifact.document_count()),
                    Some(ready.artifact.sentence_count()),
                    Some(ready.artifact.token_count()),
                    Some(ready.identity.as_str().to_owned()),
                )
            });
        let readiness = if ready.is_some() {
            "ready"
        } else {
            match state {
                CacheReadiness::Checking => "checking",
                CacheReadiness::Building { .. } => "building",
                CacheReadiness::Failed { .. } => "failed",
                CacheReadiness::Ready { .. } => "checking",
            }
        };
        datasets.insert(
            name.as_str().to_owned(),
            Dataset {
                label: if slot.display_name.is_empty() {
                    name.as_str().to_owned()
                } else {
                    slot.display_name.clone()
                },
                documents,
                sentences,
                tokens,
                analysis_identity,
                readiness,
            },
        );
    }
    static LABELS: LazyLock<Value> = LazyLock::new(|| {
        serde_json::from_str::<Value>(include_str!("../labels/soranoha-catalog.json"))
            .expect("bundled metadata labels are valid JSON")["keys"]
            .clone()
    });
    let mut labels = LABELS.clone();
    if let (Some(labels), Value::Object(custom)) = (
        labels.as_object_mut(),
        store.frequency.labels().question_labels(),
    ) {
        for (key, names) in custom {
            let entry = labels.entry(key).or_insert_with(|| serde_json::json!({}));
            for field in ["labels", "values"] {
                if !names[field].is_null() {
                    entry[field] = names[field].clone();
                }
            }
        }
    }
    let source = store.default_pattern.clone();
    let (definitions, catalog_fingerprint) = match store
        .coordinators
        .pattern_task(move || {
            (
                crate::patterns::question_definitions(&source),
                crate::patterns::web_catalog_fingerprint(&source),
            )
        })
        .await
    {
        Ok(published) => published,
        Err(error) => return crate::handlers::pattern_task_error(error),
    };
    Json(Configuration {
        datasets,
        source: store.default_pattern.to_string(),
        definitions,
        catalog_fingerprint,
        vectors_blake3: crate::patterns::word_vectors().map(|table| table.digest().to_string()),
        labels,
        value_names: crate::metadata_labels::value_names(),
    })
    .into_response()
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({"error":message}))).into_response()
}

async fn selected(
    store: &RequestStore,
    corpus: &str,
) -> Result<(Arc<CorpusRuntime>, ReadyCorpus), (StatusCode, &'static str)> {
    let Some(slot) = store.corpora.resolve(Some(corpus)) else {
        return Err((StatusCode::NOT_FOUND, "Unknown corpus."));
    };
    let runtime = Arc::clone(&slot.runtime);
    runtime.start_or_observe().await;
    let Some(ready) = runtime.ready().await else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "The corpus is not ready. Try again shortly.",
        ));
    };
    Ok((runtime, ready))
}

async fn corrupt(runtime: &CorpusRuntime, ready: &ReadyCorpus) -> Response {
    runtime.invalidator().invalidate_corrupt(ready).await;
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "Corpus data could not be read. Try again or ask the server operator to check this corpus.",
    )
}

#[derive(Deserialize)]
struct TokenQuery {
    sentence: u64,
}
#[derive(Serialize)]
pub(crate) struct TokenRow {
    pub(crate) index: usize,
    /// Whether displayed text has a space between this token and the one
    /// before it ([`lingrep_core::token_match::display_space_before`]).
    /// Omitted when false.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) space_before: bool,
    pub(crate) attributes: BTreeMap<String, Value>,
}
#[derive(Serialize)]
struct StoredSentence {
    analysis_identity: String,
    sentence: u64,
    tokens: Vec<TokenRow>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    spans: Vec<lingrep_core::token_match::SentenceSpan>,
}

pub(crate) fn token_rows(sentence: &BoundTokenSentence, schema: &TokenSchema) -> Vec<TokenRow> {
    token_rows_for_keys(sentence, schema.keys())
}

pub(crate) fn token_rows_for_keys(
    sentence: &BoundTokenSentence,
    keys: &[lingrep_core::token_match::AttributeKey],
) -> Vec<TokenRow> {
    (0..sentence.len())
        .map(|index| TokenRow {
            index,
            space_before: lingrep_core::token_match::display_space_before(sentence, index),
            attributes: keys
                .iter()
                .filter_map(|key| {
                    let value = match sentence.value_key(index, key)? {
                        AttributeValueRef::Scalar(value) => Value::String(value.to_owned()),
                        AttributeValueRef::StringSet(values) => Value::Array(
                            values
                                .iter()
                                .map(|value| Value::String(value.to_string()))
                                .collect(),
                        ),
                    };
                    Some((key.as_str().to_owned(), value))
                })
                .collect(),
        })
        .collect()
}

async fn tokens(
    State(store): State<RequestStore>,
    Path(corpus): Path<String>,
    query: Result<Query<TokenQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return error(StatusCode::BAD_REQUEST, "Choose a valid sentence number.");
    };
    let (runtime, ready) = match selected(&store, &corpus).await {
        Ok(value) => value,
        Err((status, message)) => return error(status, message),
    };
    if query.sentence >= ready.artifact.sentence_count() {
        return error(StatusCode::NOT_FOUND, "Sentence not found.");
    }
    let permit = store.coordinators.acquire_reading_permit().await;
    let Some(reader) = ready
        .reader()
        .ok()
        .and_then(|reader| reader.bind(&ready.identity))
    else {
        return corrupt(&runtime, &ready).await;
    };
    let schema = match ready.artifact.token_schema() {
        Ok(schema) => schema,
        Err(_) => return corrupt(&runtime, &ready).await,
    };
    let identity = ready.identity.as_str().to_owned();
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        reader
            .sentence_attributes_bounded(query.sentence, MAX_TOKEN_BYTES)
            .map(|sentence| {
                sentence.map(|sentence| StoredSentence {
                    analysis_identity: identity,
                    sentence: query.sentence,
                    spans: sentence.spans().to_vec(),
                    tokens: token_rows(&sentence, &schema),
                })
            })
    })
    .await;
    match result {
        Ok(Ok(Some(sentence))) => match serde_json::to_vec(&sentence) {
            Ok(bytes) if bytes.len() <= MAX_JSON_BYTES => (
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response(),
            _ => error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "This sentence is too large to inspect. Choose another example.",
            ),
        },
        Ok(Ok(None)) => error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "This sentence is too large to inspect. Choose another example.",
        ),
        Ok(Err(_)) => corrupt(&runtime, &ready).await,
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Sentence inspection is unavailable. Try again.",
        ),
    }
}

#[derive(Default, Deserialize)]
struct MetadataQuery {
    field: Option<String>,
    q: Option<String>,
    after: Option<String>,
    locale: Option<String>,
}
impl MetadataQuery {
    fn validate(&self) -> Result<(), &'static str> {
        if self
            .field
            .as_ref()
            .is_some_and(|v| v.len() > MAX_QUERY_BYTES)
            || self.q.as_ref().is_some_and(|v| v.len() > MAX_QUERY_BYTES)
            || self
                .after
                .as_ref()
                .is_some_and(|v| v.len() > MAX_CURSOR_BYTES)
        {
            return Err("The metadata query is too long.");
        }
        if self
            .locale
            .as_deref()
            .is_some_and(|locale| !crate::metadata_labels::LOCALES.contains(&locale))
        {
            return Err("Choose English (en) or Japanese (ja) for metadata labels.");
        }
        Ok(())
    }
}
#[derive(Clone, Serialize)]
pub(crate) struct Field {
    key: String,
    list: bool,
    numeric: bool,
    min: Option<f64>,
    max: Option<f64>,
    present: u64,
}
/// Immutable, corpus-scoped summary of document metadata fields.
///
/// Bounded by `MAX_FIELDS` (256) entries regardless of corpus size, and keyed
/// in [`ReadyCorpus`] by `Arc<CorpusArtifact>` pointer identity so rebuilding
/// a corrupt artifact automatically invalidates it.
#[derive(Clone)]
pub(crate) struct CachedMetadataSummary {
    fields: Arc<[Field]>,
    documents: u64,
    oversized_fields: Arc<BTreeSet<String>>,
}
#[derive(Debug, PartialEq, Serialize)]
struct MetadataChoice {
    value: Value,
    label: String,
    documents: u64,
}
#[derive(Serialize)]
struct MetadataResult {
    fields: Vec<Field>,
    documents: u64,
    values: Vec<MetadataChoice>,
    next: Option<String>,
}
#[derive(Default)]
struct MetadataSummary {
    fields: BTreeMap<String, crate::provenance_view::MetadataFieldSummary>,
    oversized_fields: BTreeSet<String>,
    documents: u64,
}

fn value_label(field: &str, value: &str) -> String {
    if field != "ndc" {
        return value.to_owned();
    }
    static NAMES: LazyLock<BTreeMap<&'static str, String>> = LazyLock::new(|| {
        include_str!("../../lingrep-core/src/corpus/bccwj/data/ndc9.tsv")
            .lines()
            .filter_map(|line| {
                let (code, names) = line.split_once('\t')?;
                Some((
                    code,
                    names
                        .split('\t')
                        .filter(|name| !name.is_empty())
                        .collect::<Vec<_>>()
                        .join("・"),
                ))
            })
            .collect()
    });
    match NAMES.get(value).filter(|name| !name.is_empty()) {
        Some(name) => format!("{value} {name}"),
        None => value.to_owned(),
    }
}

fn has_oversized_scalar(value: &Value) -> bool {
    let members = if let Value::Array(values) = value {
        values.as_slice()
    } else {
        std::slice::from_ref(value)
    };
    for member in members {
        if member.is_array() || member.is_object() {
            continue;
        }
        // Only JSON strings can exceed MAX_VALUE_BYTES (4096); numbers, booleans,
        // and null serialize to at most a few dozen bytes.
        if member
            .as_str()
            .is_some_and(|raw| raw.len() > MAX_VALUE_BYTES)
        {
            return true;
        }
    }
    false
}

impl MetadataSummary {
    fn observe_document(&mut self, metadata: Value) -> Result<(), &'static str> {
        let metadata = metadata
            .as_object()
            .ok_or("Document metadata is not an object.")?;
        self.documents += 1;
        for (key, value) in metadata {
            if key.len() > MAX_QUERY_BYTES {
                return Err("A metadata field name exceeds the display limit.");
            }
            if !self.fields.contains_key(key) && self.fields.len() >= MAX_FIELDS {
                return Err("This corpus has too many metadata fields to display.");
            }
            let field = self
                .fields
                .entry(key.clone())
                .or_insert_with(crate::provenance_view::MetadataFieldSummary::new);
            field.observe_json(value);
            if !self.oversized_fields.contains(key) && has_oversized_scalar(value) {
                self.oversized_fields.insert(key.clone());
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn observe(&mut self, metadata: Value, query: &MetadataQuery) -> Result<(), &'static str> {
        self.observe_document(metadata)?;
        if let Some(field) = query.field.as_deref()
            && self.oversized_fields.contains(field)
        {
            return Err("A metadata value exceeds the display limit.");
        }
        Ok(())
    }

    fn into_cached(self) -> CachedMetadataSummary {
        let mut fields = self
            .fields
            .into_iter()
            .map(|(key, summary)| Field {
                key,
                list: summary.lists,
                numeric: summary.numeric,
                min: summary.min,
                max: summary.max,
                present: summary.present,
            })
            .collect::<Vec<_>>();
        fields.sort_by(|a, b| b.present.cmp(&a.present).then_with(|| a.key.cmp(&b.key)));
        CachedMetadataSummary {
            fields: fields.into(),
            documents: self.documents,
            oversized_fields: Arc::new(self.oversized_fields),
        }
    }

    #[cfg(test)]
    fn finish(self) -> MetadataResult {
        let cached = self.into_cached();
        MetadataResult {
            fields: cached.fields.to_vec(),
            documents: cached.documents,
            values: Vec::new(),
            next: None,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DistributionCursor {
    documents: u64,
    kind: String,
    value: String,
}

#[derive(Debug)]
enum DistributionError {
    Corpus(lingrep_corpus::CorpusError),
    InvalidCursor,
    OversizedValue,
}

async fn collect_distribution(
    stream: impl futures::Stream<
        Item = Result<(String, Option<String>, u64), lingrep_corpus::CorpusError>,
    >,
    query: &MetadataQuery,
    labels: &crate::metadata_labels::KeyLabels,
) -> Result<(Vec<MetadataChoice>, Option<String>), DistributionError> {
    use futures::TryStreamExt;
    let after = query
        .after
        .as_ref()
        .filter(|encoded| !encoded.is_empty())
        .map(|encoded| serde_json::from_str::<DistributionCursor>(encoded))
        .transpose()
        .map_err(|_| DistributionError::InvalidCursor)?;
    let mut seeking = after.is_some();
    let needle = query.q.as_deref().unwrap_or("").to_lowercase();
    let mut values = Vec::new();
    let mut last = None;
    futures::pin_mut!(stream);
    while let Some((kind, encoded, documents)) =
        stream.try_next().await.map_err(DistributionError::Corpus)?
    {
        let encoded = encoded.ok_or(DistributionError::OversizedValue)?;
        let cursor = DistributionCursor {
            documents,
            kind,
            value: encoded,
        };
        if seeking {
            if Some(&cursor) == after.as_ref() {
                seeking = false;
            }
            continue;
        }
        let value: Value =
            serde_json::from_str(&cursor.value).map_err(|_| DistributionError::InvalidCursor)?;
        let raw = value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned);
        if raw.len() > MAX_VALUE_BYTES {
            return Err(DistributionError::OversizedValue);
        }
        let field = query.field.as_deref().unwrap_or("");
        let label = value
            .as_str()
            .and_then(|value| {
                labels.value_label(field, value, query.locale.as_deref().unwrap_or("en"))
            })
            .unwrap_or_else(|| value_label(field, &raw));
        if !label.to_lowercase().contains(&needle) {
            continue;
        }
        if values.len() == MAX_VALUES {
            return Ok((
                values,
                last.map(|cursor| {
                    serde_json::to_string(&cursor).expect("distribution cursor serializes")
                }),
            ));
        }
        values.push(MetadataChoice {
            value,
            label,
            documents,
        });
        last = Some(cursor);
    }
    if seeking {
        return Err(DistributionError::InvalidCursor);
    }
    Ok((values, None))
}

/// A corpus's summary, or the display limit its metadata exceeds.
pub(crate) type MetadataSummaryOutcome = Result<Arc<CachedMetadataSummary>, &'static str>;

fn unreadable_metadata(message: &str) -> lingrep_corpus::CorpusError {
    lingrep_corpus::CorpusError::InvalidFormat {
        component: "provenance.sqlite",
        message: message.to_owned(),
    }
}

/// Holds a reading permit for the scan only, so a request waiting on another
/// request's scan holds none.
async fn load_metadata_summary(
    store: &RequestStore,
    ready: &ReadyCorpus,
    provenance: Arc<lingrep_corpus::ProvenanceView>,
) -> Result<MetadataSummaryOutcome, lingrep_corpus::CorpusError> {
    let coordinators = Arc::clone(&store.coordinators);
    crate::corpus_context::detached_memo(
        ready.metadata_summary_cache(),
        Arc::clone(&ready.artifact),
        Arc::ptr_eq,
        move || async move {
            let _permit = coordinators.acquire_reading_permit().await;
            scan_metadata_summary(&provenance).await
        },
    )
    .await
}

async fn scan_metadata_summary(
    provenance: &lingrep_corpus::ProvenanceView,
) -> Result<MetadataSummaryOutcome, lingrep_corpus::CorpusError> {
    let mut summary = MetadataSummary::default();
    let mut cursor = 0u64;
    loop {
        let before = cursor;
        let page = provenance.document_metadata_page(cursor).await?;
        let Some(last) = page.last() else {
            break;
        };
        cursor = last.first_sentence.saturating_add(last.sentence_count);
        let (updated, status) = tokio::task::spawn_blocking(move || {
            for row in page {
                let Ok(value) = serde_json::from_str(row.metadata_json.as_deref().unwrap_or("{}"))
                else {
                    return (
                        summary,
                        Err(unreadable_metadata("document metadata is not JSON")),
                    );
                };
                if let Err(message) = summary.observe_document(value) {
                    return (summary, Ok(Err(message)));
                }
            }
            (summary, Ok(Ok(())))
        })
        .await
        .map_err(|_| unreadable_metadata("the metadata scan stopped"))?;
        if let Err(message) = status? {
            return Ok(Err(message));
        }
        summary = updated;
        if cursor <= before {
            return Err(unreadable_metadata("the document cursor did not advance"));
        }
    }
    Ok(Ok(Arc::new(summary.into_cached())))
}

async fn metadata(
    State(store): State<RequestStore>,
    Path(corpus): Path<String>,
    query: Result<Query<MetadataQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return error(StatusCode::BAD_REQUEST, "Choose a valid metadata query.");
    };
    if let Err(message) = query.validate() {
        return error(StatusCode::BAD_REQUEST, message);
    }
    let (runtime, ready) = match selected(&store, &corpus).await {
        Ok(value) => value,
        Err((status, message)) => return error(status, message),
    };
    let provenance = match ready.provenance().await {
        Ok(view) => view,
        Err(_) => return corrupt(&runtime, &ready).await,
    };
    let cached = match load_metadata_summary(&store, &ready, Arc::clone(&provenance)).await {
        Ok(Ok(summary)) => summary,
        Ok(Err(message)) => return error(StatusCode::UNPROCESSABLE_ENTITY, message),
        Err(_) => return corrupt(&runtime, &ready).await,
    };
    if let Some(field) = query.field.as_deref()
        && cached.oversized_fields.contains(field)
    {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "A metadata value exceeds the display limit.",
        );
    }
    let mut result = MetadataResult {
        fields: cached.fields.to_vec(),
        documents: cached.documents,
        values: Vec::new(),
        next: None,
    };
    if let Some(field) = query.field.as_deref() {
        let _permit = store.coordinators.acquire_reading_permit().await;
        match collect_distribution(
            provenance.document_metadata_distribution(field),
            &query,
            store.frequency.labels(),
        )
        .await
        {
            Ok((values, next)) => {
                result.values = values;
                result.next = next;
            }
            Err(DistributionError::InvalidCursor) => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "This metadata page is no longer valid. Start from the first page.",
                );
            }
            Err(DistributionError::OversizedValue) => {
                return error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "A metadata value exceeds the display limit.",
                );
            }
            Err(DistributionError::Corpus(cause)) => {
                tracing::warn!(%cause,"metadata distribution could not be read");
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Metadata values could not be loaded. Try again.",
                );
            }
        }
    }
    Json(result).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn question_configuration_includes_custom_key_and_value_labels() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("labels.json");
        std::fs::write(&path, r#"{"vocabulary":"local","edition":"1","source":"local","licence":"MIT",
            "keys":{"@document.author":{"labels":{"ja":"執筆者"}},"@document.subject":{"values":{"lit":{"labels":{"en":"Literature","ja":"文学"}}}}}}"#).unwrap();
        let labels = crate::metadata_labels::KeyLabels::read(&[path]).unwrap();
        let store = RequestStore::default().with_frequency(Arc::new(
            crate::frequency::FrequencyViews::default().with_labels(labels),
        ));
        let response = routes()
            .with_state(store)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/config")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["labels"]["@document.author"]["labels"]["ja"], "執筆者");
        assert_eq!(
            body["labels"]["@document.subject"]["values"]["lit"]["labels"]["ja"],
            "文学"
        );
        assert_eq!(
            body["labels"]["@document.ndc"]["labels"]["en"],
            "NDC classification"
        );
        assert_eq!(
            body["value_names"]["ud-v2"]["dep.rel"]["nsubj"]["ja"],
            "主語"
        );
    }

    #[tokio::test]
    async fn question_configuration_lists_the_served_catalogs_definitions_marking_count_only_ones()
    {
        let store = RequestStore::default().with_default_pattern(
            "key: [upos]\npos := [].\nphrase := [lemma=cat] [text=is].\nset S := {a}.\nword := [upos=NOUN].\n",
        );
        let response = routes()
            .with_state(store)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/config")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(
            body["definitions"],
            serde_json::json!([
                {"name": "pos", "count_only": true},
                {"name": "phrase", "count_only": false},
                {"name": "word", "count_only": false},
            ])
        );
    }

    #[tokio::test]
    async fn question_configuration_publishes_the_served_catalogs_fingerprint() {
        let source = "word := [upos=NOUN].\n";
        let response = routes()
            .with_state(RequestStore::default().with_default_pattern(source))
            .oneshot(
                axum::http::Request::builder()
                    .uri("/config")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["source"], source);
        assert_eq!(
            body["catalog_fingerprint"],
            lingrep_core::fsm::catalog_fingerprint(source)
        );
    }

    #[tokio::test]
    #[ignore = "requires LINGREP_QUESTION_TEST_CORPUS pointing to a prepared Soranoha author corpus"]
    async fn soranoha_author_metadata_and_tokens_use_real_prepared_data() {
        let path = std::env::var("LINGREP_QUESTION_TEST_CORPUS").unwrap();
        let store = RequestStore::default();
        let runtime = Arc::new(
            CorpusRuntime::open(path, Arc::clone(&store.context))
                .await
                .unwrap(),
        );
        let identity = runtime.ready().await.unwrap().identity.as_str().to_owned();
        let app = routes().with_state(RequestStore::with_runtime(store.config.clone(), runtime));
        for path in [
            "/config",
            "/data/default/metadata?field=ndc&q=%E5%B0%8F%E8%AA%AC",
            "/data/default/metadata?field=author",
            "/data/default/tokens?sentence=0",
        ] {
            let response = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .uri(path)
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            if path == "/config" {
                assert_eq!(body["datasets"]["default"]["readiness"], "ready");
                assert_eq!(body["datasets"]["default"]["analysis_identity"], identity);
                assert!(body["datasets"]["default"]["documents"].as_u64().unwrap() > 0);
            } else if path.contains("metadata") {
                assert!(
                    body["fields"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|field| field["key"] == "ndc" && field["list"] == true)
                );
                let values = body["values"].as_array().unwrap();
                assert!(
                    values
                        .windows(2)
                        .all(|pair| pair[0]["documents"].as_u64().unwrap()
                            >= pair[1]["documents"].as_u64().unwrap())
                );
                let fields = body["fields"].as_array().unwrap();
                assert!(
                    fields
                        .windows(2)
                        .all(|pair| pair[0]["present"].as_u64().unwrap()
                            >= pair[1]["present"].as_u64().unwrap())
                );
                if path.contains("field=ndc") {
                    assert!(values.iter().any(|value| value["value"] == "913"
                        && value["label"].as_str().unwrap().contains("小説")));
                }
            } else {
                assert_eq!(body["analysis_identity"], identity);
                assert!(
                    body["tokens"][0]["attributes"]
                        .as_object()
                        .unwrap()
                        .contains_key("unidic.cform")
                );
            }
        }
    }

    #[test]
    fn token_json_preserves_canonical_provider_keys_and_set_members() {
        use lingrep_core::token_match::{AttributeKind, ProjectedAttributeValue, SchemaAttribute};
        let schema = TokenSchema::new([
            SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
            SchemaAttribute::new("sudachi.reading", AttributeKind::Scalar).unwrap(),
            SchemaAttribute::new("feat.Number", AttributeKind::StringSet).unwrap(),
        ])
        .unwrap();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        let sentence = BoundTokenSentence::from_projected_attribute_values(
            &projection,
            1,
            vec![
                ProjectedAttributeValue::scalar("猫"),
                ProjectedAttributeValue::string_set(["Sing", "Plur"]),
                ProjectedAttributeValue::scalar("ネコ"),
            ],
        )
        .unwrap();
        let json = serde_json::to_value(token_rows(&sentence, &schema)).unwrap();
        assert_eq!(
            json,
            serde_json::json!([{"index":0,"attributes":{"text":"猫","sudachi.reading":"ネコ","feat.Number":["Plur","Sing"]}}])
        );
    }

    #[test]
    fn a_token_row_reports_the_space_the_source_has_before_it() {
        use lingrep_core::token_match::{
            AttributeKind, CharSpan, ProjectedAttributeValue, SchemaAttribute,
        };
        let schema =
            TokenSchema::new([SchemaAttribute::new("text", AttributeKind::Scalar).unwrap()])
                .unwrap();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        // "NHK news を": a space before `news` and before `を`.
        let rows = |spans: Vec<CharSpan>| {
            let sentence = BoundTokenSentence::from_projected_attribute_values(
                &projection,
                3,
                ["NHK", "news", "を"]
                    .map(ProjectedAttributeValue::scalar)
                    .to_vec(),
            )
            .unwrap()
            .with_char_offsets(Some(std::sync::Arc::new(spans)));
            serde_json::to_value(token_rows(&sentence, &schema)).unwrap()
        };
        assert_eq!(
            rows(vec![
                CharSpan::new(0, 3),
                CharSpan::new(4, 8),
                CharSpan::new(9, 10)
            ]),
            serde_json::json!([
                {"index":0,"attributes":{"text":"NHK"}},
                {"index":1,"space_before":true,"attributes":{"text":"news"}},
                {"index":2,"space_before":true,"attributes":{"text":"を"}},
            ])
        );
        assert_eq!(
            rows(vec![
                CharSpan::new(0, 3),
                CharSpan::new(3, 7),
                CharSpan::new(7, 8)
            ]),
            serde_json::json!([
                {"index":0,"attributes":{"text":"NHK"}},
                {"index":1,"attributes":{"text":"news"}},
                {"index":2,"attributes":{"text":"を"}},
            ])
        );
    }

    #[tokio::test]
    async fn ready_tokens_are_identity_bound_and_keep_unidic_attributes() {
        use crate::corpus_runtime::{CorpusRuntime, test_ready_corpus};
        use lingrep_core::token_match::TokenInfo;
        let (directory, _) = test_ready_corpus(vec![vec![
            TokenInfo::default()
                .with_orth("か")
                .with_lemma("か")
                .with_cform("基本形"),
        ]]);
        let store = RequestStore::default();
        let runtime = CorpusRuntime::open(
            directory.path().join("unit.lingcorpus"),
            Arc::clone(&store.context),
        )
        .await
        .unwrap();
        let expected = runtime.ready().await.unwrap().identity.as_str().to_owned();
        let config = store.config.clone();
        let app = routes().with_state(RequestStore::with_runtime(config, Arc::new(runtime)));
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/data/default/tokens?sentence=0")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["analysis_identity"], expected);
        assert_eq!(body["sentence"], 0);
        assert_eq!(body["tokens"][0]["attributes"]["text"], "か");
        assert_eq!(body["tokens"][0]["attributes"]["unidic.cform"], "基本形");
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/data/default/metadata")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["documents"], 1);
        assert_eq!(body["fields"], serde_json::json!([]));
        for (path, status) in [
            ("/data/default/tokens?sentence=1", StatusCode::NOT_FOUND),
            (
                "/data/default/tokens?sentence=invalid",
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .uri(path)
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status);
        }
    }

    #[tokio::test]
    async fn oversized_tokens_are_rejected_before_decode_and_corrupt_metadata_is_json() {
        use crate::corpus_runtime::{CorpusRuntime, test_ready_corpus};
        use lingrep_core::token_match::TokenInfo;
        let (directory, _) = test_ready_corpus(vec![vec![
            TokenInfo::default()
                .with_orth("か")
                .with_cform("x".repeat(MAX_TOKEN_BYTES)),
        ]]);
        let store = RequestStore::default();
        let runtime = Arc::new(
            CorpusRuntime::open(
                directory.path().join("unit.lingcorpus"),
                Arc::clone(&store.context),
            )
            .await
            .unwrap(),
        );
        let ready = runtime.ready().await.unwrap();
        let app = routes().with_state(RequestStore::with_runtime(
            store.config.clone(),
            Arc::clone(&runtime),
        ));
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/data/default/tokens?sentence=0")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(ready.decoded_block_count(), 0);
        std::fs::remove_file(directory.path().join("unit.lingcorpus/provenance.sqlite")).unwrap();
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/data/default/metadata")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("Corpus data could not be read")
        );
        assert!(
            !body["error"]
                .as_str()
                .unwrap()
                .contains(directory.path().to_str().unwrap())
        );
        assert!(runtime.ready().await.is_none());
    }

    #[test]
    fn fields_follow_document_frequency_and_numeric_bounds_remain_natural() {
        let mut summary = MetadataSummary::default();
        let query = MetadataQuery {
            field: Some("ndc".into()),
            ..Default::default()
        };
        summary
            .observe(
                serde_json::json!({"ndc":["913","913"],"year":1960,"rare":true}),
                &query,
            )
            .unwrap();
        summary
            .observe(serde_json::json!({"ndc":["019"],"year":1940}), &query)
            .unwrap();
        let result = summary.finish();
        assert_eq!(result.documents, 2);
        assert_eq!(
            result
                .fields
                .iter()
                .map(|field| field.key.as_str())
                .collect::<Vec<_>>(),
            ["ndc", "year", "rare"]
        );
        let year = &result.fields[1];
        assert!(year.numeric);
        assert_eq!(
            (year.min, year.max, year.present),
            (Some(1940.0), Some(1960.0), 2)
        );
        assert!(result.fields[0].list);
    }

    #[tokio::test]
    async fn frequency_pages_keep_popular_values_and_ties_without_skips() {
        let mut rows = vec![("text".into(), Some("\"zzpopular\"".into()), 20)];
        rows.extend((0..150).map(|n| ("text".into(), Some(format!("\"value{n:03}\"")), 2)));
        let query = MetadataQuery {
            field: Some("title".into()),
            after: Some(String::new()),
            ..Default::default()
        };
        let (first, next) = collect_distribution(
            futures::stream::iter(rows.clone().into_iter().map(Ok)),
            &query,
            &crate::metadata_labels::KeyLabels::default(),
        )
        .await
        .unwrap();
        assert_eq!(first.len(), 100);
        assert_eq!(first[0].value, "zzpopular");
        assert_eq!(first[0].documents, 20);
        assert_eq!(first[99].value, "value098");
        let query = MetadataQuery {
            after: next,
            field: query.field,
            ..Default::default()
        };
        let (second, next) = collect_distribution(
            futures::stream::iter(rows.into_iter().map(Ok)),
            &query,
            &crate::metadata_labels::KeyLabels::default(),
        )
        .await
        .unwrap();
        assert_eq!(second.len(), 51);
        assert_eq!(second[0].value, "value099");
        assert!(next.is_none());
    }

    #[tokio::test]
    async fn distribution_filter_uses_ndc_names_and_preserves_json_types() {
        let query = MetadataQuery {
            field: Some("ndc".into()),
            q: Some("小説".into()),
            ..Default::default()
        };
        let rows = vec![
            Ok(("text".into(), Some("\"019\"".into()), 10)),
            Ok(("text".into(), Some("\"913\"".into()), 2)),
        ];
        let (values, next) = collect_distribution(
            futures::stream::iter(rows),
            &query,
            &crate::metadata_labels::KeyLabels::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            values,
            vec![MetadataChoice {
                value: serde_json::json!("913"),
                label: "913 日本文学・小説・物語".into(),
                documents: 2
            }]
        );
        assert!(next.is_none());
        let query = MetadataQuery {
            field: Some("mixed".into()),
            ..Default::default()
        };
        let rows = vec![
            Ok(("integer".into(), Some("1".into()), 1)),
            Ok(("text".into(), Some("\"1\"".into()), 1)),
        ];
        let (values, _) = collect_distribution(
            futures::stream::iter(rows),
            &query,
            &crate::metadata_labels::KeyLabels::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            values.iter().map(|v| v.value.clone()).collect::<Vec<_>>(),
            [serde_json::json!(1), serde_json::json!("1")]
        );
        let query = MetadataQuery {
            after: Some("not a cursor".into()),
            ..query
        };
        assert!(matches!(
            collect_distribution(
                futures::stream::empty(),
                &query,
                &crate::metadata_labels::KeyLabels::default()
            )
            .await,
            Err(DistributionError::InvalidCursor)
        ));
    }

    #[tokio::test]
    async fn localized_value_search_preserves_types_counts_and_raw_codes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("labels.json");
        std::fs::write(&path, r#"{"vocabulary":"subjects","edition":"1","source":"local","licence":"MIT",
            "keys":{"@document.subject":{"values":{"1":{"labels":{"en":"Literature","ja":"文学"}}}}}}"#).unwrap();
        let labels = crate::metadata_labels::KeyLabels::read(&[path]).unwrap();
        for (locale, search, expected) in [
            ("en", "literature", "1 Literature"),
            ("ja", "文学", "1 文学"),
        ] {
            let query = MetadataQuery {
                field: Some("subject".into()),
                q: Some(search.into()),
                locale: Some(locale.into()),
                ..Default::default()
            };
            let rows = vec![
                Ok(("integer".into(), Some("1".into()), 7)),
                Ok(("text".into(), Some("\"1\"".into()), 3)),
            ];
            let (values, _) = collect_distribution(futures::stream::iter(rows), &query, &labels)
                .await
                .unwrap();
            assert_eq!(
                values,
                vec![MetadataChoice {
                    value: serde_json::json!("1"),
                    label: expected.into(),
                    documents: 3
                }]
            );
        }
    }

    #[test]
    fn metadata_rejects_unbounded_fields_strings_and_queries() {
        let mut summary = MetadataSummary::default();
        let fields = (0..=MAX_FIELDS)
            .map(|n| (format!("key{n}"), Value::Null))
            .collect();
        assert!(
            summary
                .observe(Value::Object(fields), &MetadataQuery::default())
                .is_err()
        );
        assert!(
            MetadataQuery {
                q: Some("x".repeat(MAX_QUERY_BYTES + 1)),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        let mut summary = MetadataSummary::default();
        assert!(
            summary
                .observe(
                    serde_json::json!({"title":"x".repeat(MAX_VALUE_BYTES+1)}),
                    &MetadataQuery {
                        field: Some("title".into()),
                        ..Default::default()
                    }
                )
                .is_err()
        );
    }

    #[tokio::test]
    async fn config_observes_pending_corpus_and_unknown_data_returns_json() {
        let store = RequestStore::default().with_default_pattern("word := [upos=VERB].");
        let app = routes().with_state(store);
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/config")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["source"], "word := [upos=VERB].");
        assert_eq!(body["datasets"]["default"]["readiness"], "checking");
        assert!(body["datasets"]["default"]["documents"].is_null());
        assert!(body["datasets"]["default"]["analysis_identity"].is_null());
        for path in ["/data/unknown/tokens?sentence=0", "/data/unknown/metadata"] {
            let response = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .uri(path)
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            let body: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(body["error"], "Unknown corpus.");
        }
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/data/default/metadata")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["error"], "The corpus is not ready. Try again shortly.");
    }

    fn string_metadata(key: &str, value: String) -> (String, lingrep_core::corpus::MetadataValue) {
        use lingrep_core::corpus::{MetadataScalar, MetadataValue};
        (
            key.to_owned(),
            MetadataValue::Scalar(MetadataScalar::String(value)),
        )
    }

    /// A ready one-document corpus carrying `metadata`, with the store that
    /// serves it. The directory holds the artifact.
    async fn metadata_corpus(
        metadata: Vec<(String, lingrep_core::corpus::MetadataValue)>,
    ) -> (tempfile::TempDir, RequestStore, ReadyCorpus) {
        use std::path::PathBuf;

        use lingrep_core::corpus::{
            AdmittedCorpusEvent, AdmittedSentence, DiscoveryReport, DocumentId, InventoryStats,
            SourceDocument, TraversalHighWater, TraversalLimits,
        };
        use lingrep_core::spill::TempBudget;
        use lingrep_core::token_match::TokenInfo;
        use lingrep_corpus::{
            AdmissionIdentity, AnalysisInputs, BuildConfig, CorpusBuilder, DictionaryFingerprint,
            ReaderIdentity, TrustClass,
        };

        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("meta-cache.lingcorpus");
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
                temp_budget: Arc::new(TempBudget::new(16 * 1024 * 1024)),
                sentence_metadata_keys: Vec::new(),
            },
            &destination,
        )
        .unwrap();
        let logical = "doc.md".to_owned();
        let mut inv_digest = blake3::Hasher::new();
        inv_digest.update(&(logical.len() as u64).to_le_bytes());
        inv_digest.update(logical.as_bytes());
        builder
            .register_source_document(&SourceDocument {
                ordinal: 0,
                logical_name: logical.clone(),
                path: PathBuf::from("/unused/doc.md"),
                target_group_ordinal: 0,
                external_target: false,
            })
            .unwrap();
        let observation = builder.source_observation();
        observation.begin_source(&logical).unwrap();
        observation.update_source_bytes(b"doc").unwrap();
        observation.end_source().unwrap();
        builder
            .push_corpus_event(&AdmittedCorpusEvent::DocumentStart {
                id: DocumentId::new(0),
                source_name: logical.clone(),
                metadata: metadata.into_boxed_slice(),
            })
            .unwrap();
        let loc = builder
            .push_corpus_event(&AdmittedCorpusEvent::Sentence(AdmittedSentence {
                annotations: Box::new([]),
                structure: Default::default(),
                id: lingrep_core::corpus::SentenceId::new(0),
                document_id: DocumentId::new(0),
                document_sentence_ordinal: 0,
                text: String::new(),
                words: None,
                enhanced_graph: None,
                active_labels: Box::new([]),
                metadata: Box::new([]),
            }))
            .unwrap()
            .unwrap();
        builder
            .push_tokenized_sentence(&loc, &[TokenInfo::default().with_orth("猫")])
            .unwrap();
        builder
            .push_corpus_event(&AdmittedCorpusEvent::DocumentEnd {
                id: DocumentId::new(0),
            })
            .unwrap();
        let artifact = builder
            .finish(DiscoveryReport {
                inventory: InventoryStats {
                    documents: 1,
                    distinct_targets: 1,
                    alias_documents: 0,
                    max_target_multiplicity: 1,
                    external_targets: 0,
                },
                configured_limits: TraversalLimits::default(),
                observed: TraversalHighWater::default(),
                ordered_inventory_digest: *inv_digest.finalize().as_bytes(),
            })
            .unwrap();
        let store = RequestStore::default();
        let identity = lingrep_corpus::format_identity(artifact.analysis_identity().as_bytes());
        let runtime = Arc::new(CorpusRuntime::from_ready(
            crate::corpus_runtime::CorpusIdentity::new(&identity),
            artifact,
            Arc::clone(&store.context),
        ));
        let ready = runtime.ready().await.unwrap();
        let store = RequestStore::with_runtime(store.config.clone(), runtime);
        (directory, store, ready)
    }

    async fn every_reading_permit(store: &RequestStore) -> Vec<tokio::sync::OwnedSemaphorePermit> {
        let mut permits = Vec::new();
        for _ in 0..crate::coordinator::READING_CONCURRENCY {
            permits.push(store.coordinators.acquire_reading_permit().await);
        }
        permits
    }

    fn metadata_request(field: &str) -> axum::http::Request<axum::body::Body> {
        axum::http::Request::builder()
            .uri(format!("/data/default/metadata?field={field}"))
            .body(axum::body::Body::empty())
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn metadata_summary_memoizes_per_artifact_and_enforces_per_field_oversized_limit() {
        let (_directory, store, ready) = metadata_corpus(vec![
            string_metadata("author", "夏目漱石".into()),
            string_metadata("huge", "x".repeat(MAX_VALUE_BYTES + 1)),
        ])
        .await;
        let app = routes().with_state(store);

        // First call populates cache; querying `author` succeeds even though `huge` has >4 KiB value.
        let r1 = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/data/default/metadata?field=author")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r1.status(), StatusCode::OK);
        let b1 = r1.into_body().collect().await.unwrap().to_bytes();
        let cached_ptr = {
            let guard = ready.metadata_summary_cache().lock().await;
            Arc::clone(
                guard
                    .as_ref()
                    .expect("summary must be cached")
                    .1
                    .as_ref()
                    .expect("the summary is within the display limits"),
            )
        };

        // Second call reuses the exact same cached Arc and returns identical bytes.
        let r2 = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/data/default/metadata?field=author")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r2.status(), StatusCode::OK);
        let b2 = r2.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(b1, b2);
        {
            let guard = ready.metadata_summary_cache().lock().await;
            assert!(Arc::ptr_eq(
                &cached_ptr,
                guard.as_ref().unwrap().1.as_ref().unwrap()
            ));
        }

        // Querying the oversized field still returns 422 from the cached oversized_fields set.
        let r3 = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/data/default/metadata?field=huge")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r3.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_corpus_over_the_field_limit_is_scanned_once() {
        let (_directory, store, ready) = metadata_corpus(
            (0..=MAX_FIELDS)
                .map(|field| string_metadata(&format!("field{field}"), "x".into()))
                .collect(),
        )
        .await;
        let app = routes().with_state(store);
        for _ in 0..2 {
            let response = app
                .clone()
                .oneshot(metadata_request("field0"))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            let body: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(
                body["error"],
                "This corpus has too many metadata fields to display."
            );
        }
        let guard = ready.metadata_summary_cache().lock().await;
        let (artifact, outcome) = guard.as_ref().expect("the outcome is cached");
        assert!(Arc::ptr_eq(artifact, &ready.artifact));
        assert_eq!(
            outcome.as_ref().err(),
            Some(&"This corpus has too many metadata fields to display.")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_request_waiting_on_another_requests_summary_holds_no_reading_permit() {
        let (_directory, store, ready) =
            metadata_corpus(vec![string_metadata("author", "夏目漱石".into())]).await;
        let app = routes().with_state(store.clone());
        let scanning = ready.metadata_summary_cache().lock().await;
        let waiting = tokio::spawn(app.oneshot(metadata_request("author")));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!waiting.is_finished());
        let permits = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            every_reading_permit(&store),
        )
        .await
        .expect("every reading permit is free while the request waits");
        drop(permits);
        drop(scanning);
        assert_eq!(waiting.await.unwrap().unwrap().status(), StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_summary_scan_whose_request_is_cancelled_finishes_and_is_cached() {
        let (_directory, store, ready) =
            metadata_corpus(vec![string_metadata("author", "夏目漱石".into())]).await;
        let app = routes().with_state(store.clone());
        // With every reading permit taken, the scan starts and stops at its
        // own permit while it holds the memo. A request only reading the memo
        // releases it at once, so a memo held across the pause is the scan's.
        let permits = every_reading_permit(&store).await;
        let request = tokio::spawn(app.clone().oneshot(metadata_request("author")));
        let cache = ready.metadata_summary_cache();
        let pause = std::time::Duration::from_millis(50);
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if cache.try_lock().is_err() {
                    tokio::time::sleep(pause).await;
                    if cache.try_lock().is_err() {
                        break;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the scan starts");
        request.abort();
        assert!(request.await.expect_err("aborted request").is_cancelled());
        drop(permits);
        assert!(
            cache.lock().await.is_some(),
            "the scan the cancelled request started is cached"
        );
        let response = app.oneshot(metadata_request("author")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
