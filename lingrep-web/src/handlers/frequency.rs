//! The frequency view: `GET /frequencies` and the JSON endpoints it reads.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use askama::Template;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};

use crate::frequency::{Condition, Dimension, FrequencyViews, Outcome, Sort, Unsupported};
use crate::state::RequestStore;

#[derive(Template)]
#[template(path = "frequency.html")]
struct FrequencyPage {
    current_page: &'static str,
    frequency_view: bool,
}

pub fn page_routes() -> Router<RequestStore> {
    Router::new().route("/frequencies", get(page))
}

pub fn routes() -> Router<RequestStore> {
    Router::new()
        .route("/frequency", get(query))
        .route("/frequency/meta", get(meta))
        .route("/frequency/values", get(values))
}

async fn page(State(store): State<RequestStore>) -> Response {
    crate::handlers::render_or_500(FrequencyPage {
        current_page: "frequencies",
        frequency_view: !store.frequency.is_empty(),
    })
}

#[derive(Serialize)]
struct PairMeta<'a> {
    corpus: &'a str,
    corpus_name: &'a str,
    catalog: &'a str,
    patterns: &'a [crate::frequency::PatternSource],
    workbench: Option<&'a str>,
}

#[derive(Serialize)]
struct Meta<'a> {
    pairs: Vec<PairMeta<'a>>,
    dimensions: BTreeMap<&'a str, &'a [Dimension]>,
}

async fn meta(State(store): State<RequestStore>) -> Response {
    let views = &store.frequency;
    Json(Meta {
        pairs: views
            .pairs()
            .map(|pair| PairMeta {
                corpus: pair.corpus.as_str(),
                corpus_name: &pair.corpus_display,
                catalog: &pair.catalog,
                patterns: pair.patterns(),
                workbench: pair.workbench(),
            })
            .collect(),
        dimensions: views
            .pairs()
            .map(|pair| (pair.corpus.as_str(), views.dimensions(&pair.corpus)))
            .collect(),
    })
    .into_response()
}

#[derive(Deserialize)]
struct QueryParams {
    corpus: String,
    catalog: String,
    pattern: String,
    /// The conditions as JSON, keyed by metadata key.
    #[serde(default)]
    w: Option<String>,
    /// A clause written as text, used instead of `w`.
    #[serde(default, rename = "where")]
    clause: Option<String>,
    #[serde(default)]
    sort: Sort,
    #[serde(default)]
    q: String,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: usize,
}

#[derive(Deserialize)]
struct ValuesParams {
    corpus: String,
    catalog: String,
    pattern: String,
    key: String,
    #[serde(default)]
    w: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Reply<T> {
    Ok {
        #[serde(flatten)]
        body: T,
        ms: f64,
    },
    Unsupported(Unsupported),
    /// No table counts this catalog over this corpus.
    Unavailable,
}

fn bad_request(message: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

fn conditions(raw: Option<&str>) -> Result<BTreeMap<String, Condition>, Box<Response>> {
    raw.filter(|raw| !raw.is_empty())
        .map(serde_json::from_str)
        .transpose()
        .map(Option::unwrap_or_default)
        .map_err(|error| {
            Box::new(bad_request(format!(
                "w is not a set of conditions: {error}"
            )))
        })
}

/// Runs `work` on the pair named by `corpus` and `catalog` off the executor.
async fn on_pair<T: Serialize + Send + 'static>(
    views: Arc<FrequencyViews>,
    corpus: String,
    catalog: String,
    work: impl FnOnce(
        &crate::frequency::FrequencyPair,
        &[Dimension],
    ) -> Result<Result<T, Unsupported>, String>
    + Send
    + 'static,
) -> Response {
    let started = Instant::now();
    let result = tokio::task::spawn_blocking(move || {
        let Some(pair) = views.find(&corpus, &catalog) else {
            return Ok(Reply::Unavailable);
        };
        Ok(match work(pair, views.dimensions(&pair.corpus))? {
            Ok(body) => Reply::Ok {
                body,
                ms: started.elapsed().as_secs_f64() * 1000.0,
            },
            Err(unsupported) => Reply::Unsupported(unsupported),
        })
    })
    .await;
    match result {
        Ok(Ok(reply)) => Json(reply).into_response(),
        Ok(Err(message)) => bad_request(message),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn query(State(store): State<RequestStore>, Query(params): Query<QueryParams>) -> Response {
    let conditions = match conditions(params.w.as_deref()) {
        Ok(conditions) => conditions,
        Err(response) => return *response,
    };
    let query = crate::frequency::Query {
        pattern: params.pattern,
        conditions,
        clause: params.clause,
        sort: params.sort,
        filter: params.q,
        offset: params.offset,
        limit: params.limit,
    };
    on_pair(
        Arc::clone(&store.frequency),
        params.corpus,
        params.catalog,
        move |pair, dimensions| match pair.answer(dimensions, &query) {
            Ok(Outcome::Answer(answer)) => Ok(Ok(answer)),
            Ok(Outcome::Unsupported(unsupported)) => Ok(Err(unsupported)),
            Err(error) => Err(error.to_string()),
        },
    )
    .await
}

#[derive(Serialize)]
struct Values {
    values: Vec<(String, u64)>,
}

async fn values(State(store): State<RequestStore>, Query(params): Query<ValuesParams>) -> Response {
    let conditions = match conditions(params.w.as_deref()) {
        Ok(conditions) => conditions,
        Err(response) => return *response,
    };
    on_pair(
        Arc::clone(&store.frequency),
        params.corpus,
        params.catalog,
        move |pair, dimensions| {
            pair.values(dimensions, &params.pattern, &params.key, &conditions)
                .map(|values| values.map(|values| Values { values }))
                .map_err(|error| error.to_string())
        },
    )
    .await
}
