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
const MAX_CURSOR_BYTES: usize = MAX_VALUE_BYTES * 6 + 2;
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
    labels: Value,
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
    Json(Configuration {
        datasets,
        source: store.default_pattern.to_string(),
        labels: LABELS.clone(),
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
    pub(crate) attributes: BTreeMap<String, Value>,
}
#[derive(Serialize)]
struct StoredSentence {
    analysis_identity: String,
    sentence: u64,
    tokens: Vec<TokenRow>,
}

pub(crate) fn token_rows(sentence: &BoundTokenSentence, schema: &TokenSchema) -> Vec<TokenRow> {
    (0..sentence.len())
        .map(|index| TokenRow {
            index,
            attributes: schema
                .keys()
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
        Ok(())
    }
}
#[derive(Serialize)]
struct Field {
    key: String,
    list: bool,
    numeric: bool,
    min: Option<f64>,
    max: Option<f64>,
    present: u64,
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
    fields: BTreeMap<String, Field>,
    documents: u64,
    values: BTreeMap<String, MetadataChoice>,
    more: bool,
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

impl MetadataSummary {
    fn observe(&mut self, metadata: Value, query: &MetadataQuery) -> Result<(), &'static str> {
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
            let field = self.fields.entry(key.clone()).or_insert_with(|| Field {
                key: key.clone(),
                list: false,
                numeric: true,
                min: None,
                max: None,
                present: 0,
            });
            field.present += 1;
            field.list |= value.is_array();
            field.numeric &= value.is_number();
            if let Some(number) = value.as_f64() {
                field.min = Some(field.min.map_or(number, |old| old.min(number)));
                field.max = Some(field.max.map_or(number, |old| old.max(number)));
            }
            if query.field.as_deref() != Some(key) {
                continue;
            }
            let members = if let Value::Array(values) = value {
                values.as_slice()
            } else {
                std::slice::from_ref(value)
            };
            let mut seen = BTreeSet::new();
            let needle = query.q.as_deref().unwrap_or("").to_lowercase();
            for value in members {
                if value.is_array() || value.is_object() {
                    continue;
                }
                let raw = value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned);
                if raw.len() > MAX_VALUE_BYTES {
                    return Err("A metadata value exceeds the display limit.");
                }
                let label = value_label(key, &raw);
                let cursor = value.to_string();
                if query.after.as_ref().is_some_and(|after| cursor <= *after)
                    || !label.to_lowercase().contains(&needle)
                    || !seen.insert(cursor.clone())
                {
                    continue;
                }
                self.values
                    .entry(cursor)
                    .or_insert_with(|| MetadataChoice {
                        value: value.clone(),
                        label,
                        documents: 0,
                    })
                    .documents += 1;
                if self.values.len() > MAX_VALUES {
                    self.values.pop_last();
                    self.more = true;
                }
            }
        }
        Ok(())
    }
    fn finish(self) -> MetadataResult {
        let next = self
            .more
            .then(|| self.values.last_key_value().map(|(key, _)| key.clone()))
            .flatten();
        MetadataResult {
            fields: self.fields.into_values().collect(),
            documents: self.documents,
            values: self.values.into_values().collect(),
            next,
        }
    }
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
    let permit = store.coordinators.acquire_reading_permit().await;
    let provenance = match ready.provenance().await {
        Ok(view) => view,
        Err(_) => return corrupt(&runtime, &ready).await,
    };
    let mut summary = MetadataSummary::default();
    let mut cursor = 0;
    loop {
        let before = cursor;
        let page = match provenance.document_metadata_page(cursor).await {
            Ok(page) => page,
            Err(_) => return corrupt(&runtime, &ready).await,
        };
        if page.is_empty() {
            break;
        }
        for row in page {
            cursor = row.first_sentence + row.sentence_count;
            let value = match serde_json::from_str(row.metadata_json.as_deref().unwrap_or("{}")) {
                Ok(value) => value,
                Err(_) => return corrupt(&runtime, &ready).await,
            };
            if let Err(message) = summary.observe(value, &query) {
                return error(StatusCode::UNPROCESSABLE_ENTITY, message);
            }
        }
        if cursor <= before {
            return corrupt(&runtime, &ready).await;
        }
    }
    drop(permit);
    Json(summary.finish()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

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
                assert!(
                    body["values"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|value| value["value"] == "913"
                            && value["label"].as_str().unwrap().contains("小説"))
                );
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
    fn metadata_counts_documents_once_per_value_and_keeps_ndc_codes() {
        let mut summary = MetadataSummary::default();
        let query = MetadataQuery {
            field: Some("ndc".into()),
            q: Some("小説".into()),
            ..Default::default()
        };
        summary
            .observe(
                serde_json::json!({"ndc":["913","913","019"],"year":1940}),
                &query,
            )
            .unwrap();
        summary
            .observe(serde_json::json!({"ndc":["913"],"year":1960}), &query)
            .unwrap();
        let result = summary.finish();
        assert_eq!(result.documents, 2);
        assert_eq!(
            result.values,
            vec![MetadataChoice {
                value: Value::String("913".into()),
                label: "913 日本文学・小説・物語".into(),
                documents: 2
            }]
        );
        let year = result.fields.iter().find(|f| f.key == "year").unwrap();
        assert!(year.numeric);
        assert_eq!(
            (year.min, year.max, year.present),
            (Some(1940.0), Some(1960.0), 2)
        );
        assert!(result.fields.iter().find(|f| f.key == "ndc").unwrap().list);
    }

    #[test]
    fn value_pages_are_bounded_sorted_and_count_early_and_late_occurrences() {
        let mut summary = MetadataSummary::default();
        let query = MetadataQuery {
            field: Some("title".into()),
            ..Default::default()
        };
        for n in (0..150).rev().chain(0..150) {
            summary
                .observe(serde_json::json!({"title":format!("{n:03}")}), &query)
                .unwrap();
            assert!(summary.values.len() <= MAX_VALUES);
        }
        let result = summary.finish();
        assert_eq!(result.values.len(), 100);
        assert_eq!(result.next.as_deref(), Some("\"099\""));
        assert!(result.values.iter().all(|v| v.documents == 2));
        let query = MetadataQuery {
            field: Some("title".into()),
            after: result.next,
            ..Default::default()
        };
        let mut summary = MetadataSummary::default();
        for n in 0..150 {
            summary
                .observe(serde_json::json!({"title":format!("{n:03}")}), &query)
                .unwrap();
        }
        let result = summary.finish();
        assert_eq!(result.values.len(), 50);
        assert_eq!(result.values[0].value, "100");
        assert_eq!(result.next, None);
    }

    #[test]
    fn similarly_spelled_metadata_values_keep_distinct_json_types() {
        let mut summary = MetadataSummary::default();
        summary
            .observe(
                serde_json::json!({"mixed":["1",1,"true",true]}),
                &MetadataQuery {
                    field: Some("mixed".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let result = summary.finish();
        assert_eq!(result.values.len(), 4);
        assert!(
            result
                .values
                .iter()
                .any(|choice| choice.value == serde_json::json!(1))
        );
        assert!(
            result
                .values
                .iter()
                .any(|choice| choice.value == serde_json::json!("1"))
        );
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
}
