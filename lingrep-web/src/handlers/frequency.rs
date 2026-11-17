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

use lingrep_core::execution::CancellationFlag;

use crate::frequency::{
    Condition, Dimension, FrequencyViews, Outcome, QueryError, Sort, Unsupported,
};
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
        .route("/frequency/profile", get(profile))
        .route("/frequency/lines", get(lines))
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
    source: &'a str,
    patterns: &'a [crate::frequency::PatternSource],
    default_pattern: Option<&'a str>,
    workbench: Option<&'a str>,
}

#[derive(Serialize)]
struct Meta<'a> {
    pairs: Vec<PairMeta<'a>>,
    dimensions: BTreeMap<&'a str, &'a [Dimension]>,
    labels: &'a crate::metadata_labels::KeyLabels,
    value_names: &'static serde_json::Value,
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
                source: &pair.source,
                patterns: pair.patterns(),
                default_pattern: pair.default_pattern(),
                workbench: pair.workbench(),
            })
            .collect(),
        dimensions: views
            .pairs()
            .map(|pair| (pair.corpus.as_str(), views.dimensions(&pair.corpus)))
            .collect(),
        labels: views.labels(),
        value_names: crate::metadata_labels::value_names(),
    })
    .into_response()
}

#[derive(Deserialize)]
struct QueryParams {
    corpus: String,
    catalog: String,
    pattern: String,
    #[serde(default)]
    w: Option<String>,
    #[serde(default, rename = "where")]
    clause: Option<String>,
    #[serde(default)]
    sort: Sort,
    #[serde(default)]
    q: String,
    /// Exact key predicates as a JSON object of field names to values.
    #[serde(default)]
    k: Option<String>,
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
    #[serde(default)]
    q: String,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: usize,
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
    Unavailable,
}

pub(super) fn bad_request(message: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

pub(super) fn failure(error: &QueryError) -> Response {
    if error.is_request_error() {
        bad_request(error.to_string())
    } else {
        super::server_error(error)
    }
}

pub(super) fn reply_without_scan(
    views: &FrequencyViews,
    corpus: &str,
    catalog: &str,
    pattern: &str,
) -> Option<Response> {
    let Some(pair) = views.find(corpus, catalog) else {
        return Some(Json(Reply::<()>::Unavailable).into_response());
    };
    let known = pair.patterns().iter().any(|known| known.name == pattern);
    (!known).then(|| failure(&QueryError::UnknownPattern(pattern.to_owned())))
}

pub(super) fn conditions(raw: Option<&str>) -> Result<BTreeMap<String, Condition>, Box<Response>> {
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

/// Runs `work` under a scan permit, which it holds until it returns. Its
/// cancellation flag is raised when the request is dropped.
pub(super) async fn run_frequency_task<T: Send + 'static>(
    coordinators: Arc<crate::coordinator::CoordinatorRegistry>,
    work: impl FnOnce(&CancellationFlag) -> T + Send + 'static,
) -> Result<T, tokio::task::JoinError> {
    let cancel = CancellationFlag::new();
    let mut guard = super::export::CancelOnDrop {
        flag: cancel.clone(),
        armed: true,
    };
    let permit = coordinators.acquire_scan_permit().await;
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work(&cancel)
    })
    .await;
    guard.armed = false;
    result
}

async fn on_pair<T: Serialize + Send + 'static>(
    views: Arc<FrequencyViews>,
    coordinators: Arc<crate::coordinator::CoordinatorRegistry>,
    corpus: String,
    catalog: String,
    pattern: &str,
    work: impl FnOnce(
        &crate::frequency::FrequencyPair,
        &[Dimension],
        &CancellationFlag,
    ) -> Result<Result<T, Unsupported>, QueryError>
    + Send
    + 'static,
) -> Response {
    if let Some(reply) = reply_without_scan(&views, &corpus, &catalog, pattern) {
        return reply;
    }
    let started = Instant::now();
    let result = run_frequency_task(coordinators, move |cancel| {
        let reply = match views.find(&corpus, &catalog) {
            Some(pair) => match work(pair, views.dimensions(&pair.corpus), cancel)? {
                Ok(body) => Reply::Ok {
                    body,
                    ms: started.elapsed().as_secs_f64() * 1000.0,
                },
                Err(unsupported) => Reply::Unsupported(unsupported),
            },
            None => Reply::Unavailable,
        };
        Ok(Json(reply).into_response())
    })
    .await;
    match result {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => failure(&error),
        Err(error) => super::server_error(error),
    }
}

async fn query(State(store): State<RequestStore>, Query(params): Query<QueryParams>) -> Response {
    let conditions = match conditions(params.w.as_deref()) {
        Ok(conditions) => conditions,
        Err(response) => return *response,
    };
    let key = match params.k.as_deref().map(serde_json::from_str).transpose() {
        Ok(key) => key.unwrap_or_default(),
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("k: {error}")})),
            )
                .into_response();
        }
    };
    let pattern = params.pattern.clone();
    let query = crate::frequency::Query {
        pattern: params.pattern,
        conditions,
        clause: params.clause,
        sort: params.sort,
        filter: params.q,
        key,
        offset: params.offset,
        limit: params.limit,
    };
    on_pair(
        Arc::clone(&store.frequency),
        Arc::clone(&store.coordinators),
        params.corpus,
        params.catalog,
        &pattern,
        move |pair, dimensions, cancel| match pair.answer(dimensions, &query, cancel) {
            Ok(Outcome::Answer(answer)) => Ok(Ok(answer)),
            Ok(Outcome::Unsupported(unsupported)) => Ok(Err(unsupported)),
            Err(error) => Err(error),
        },
    )
    .await
}

async fn values(State(store): State<RequestStore>, Query(params): Query<ValuesParams>) -> Response {
    let conditions = match conditions(params.w.as_deref()) {
        Ok(conditions) => conditions,
        Err(response) => return *response,
    };
    let pattern = params.pattern.clone();
    on_pair(
        Arc::clone(&store.frequency),
        Arc::clone(&store.coordinators),
        params.corpus,
        params.catalog,
        &pattern,
        move |pair, dimensions, cancel| {
            pair.values(
                dimensions,
                &params.pattern,
                &params.key,
                &conditions,
                &crate::frequency::ValueQuery {
                    filter: params.q,
                    offset: params.offset,
                    limit: params.limit,
                },
                cancel,
            )
        },
    )
    .await
}

#[derive(Deserialize)]
struct EntryParams {
    corpus: String,
    catalog: String,
    pattern: String,
    /// The entry's group, in the JSON form of a frequency row.
    entry: String,
    #[serde(default)]
    w: Option<String>,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: usize,
}

fn entry_group(raw: &str) -> Result<serde_json::Value, Box<Response>> {
    serde_json::from_str(raw)
        .map_err(|error| Box::new(bad_request(format!("entry is not a group: {error}"))))
}

async fn profile(State(store): State<RequestStore>, Query(params): Query<EntryParams>) -> Response {
    let conditions = match conditions(params.w.as_deref()) {
        Ok(conditions) => conditions,
        Err(response) => return *response,
    };
    let group = match entry_group(&params.entry) {
        Ok(group) => group,
        Err(response) => return *response,
    };
    let pattern = params.pattern.clone();
    on_pair(
        Arc::clone(&store.frequency),
        Arc::clone(&store.coordinators),
        params.corpus,
        params.catalog,
        &pattern,
        move |pair, dimensions, cancel| {
            pair.profile(dimensions, &params.pattern, &group, &conditions, cancel)
        },
    )
    .await
}

async fn lines(State(store): State<RequestStore>, Query(params): Query<EntryParams>) -> Response {
    let conditions = match conditions(params.w.as_deref()) {
        Ok(conditions) => conditions,
        Err(response) => return *response,
    };
    let group = match entry_group(&params.entry) {
        Ok(group) => group,
        Err(response) => return *response,
    };
    let pattern = params.pattern.clone();
    on_pair(
        Arc::clone(&store.frequency),
        Arc::clone(&store.coordinators),
        params.corpus,
        params.catalog,
        &pattern,
        move |pair, dimensions, cancel| {
            pair.lines(
                cancel,
                dimensions,
                &params.pattern,
                &group,
                &conditions,
                params.offset,
                params.limit,
            )
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::{Future, poll_fn};
    use std::task::Poll;

    async fn status_and_body(response: Response) -> (StatusCode, String) {
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("body");
        (status, String::from_utf8(body.to_vec()).expect("text"))
    }

    fn over_temp_budget() -> std::io::Error {
        Arc::new(lingrep_core::spill::TempBudget::new(1))
            .reserve(2)
            .expect_err("a reservation above the budget")
    }

    /// Frequencies and the question page show one header: the same route
    /// links, with the current page marked, and the same colour tokens; on
    /// Frequencies the Help link opens the site help dialog.
    #[test]
    fn frequencies_and_the_question_page_share_one_header() {
        let routes = |html: &str| {
            let (_, rest) = html.split_once("<nav class=\"routes\"").expect("routes");
            rest.split_once("</nav>").expect("routes end").0.to_owned()
        };
        let question = crate::question_assets::page(serde_json::json!({}), true).0;
        let frequencies = FrequencyPage {
            current_page: "frequencies",
            frequency_view: true,
        }
        .render()
        .expect("page");
        assert!(routes(&question).contains(r#"href="./" aria-current="page""#));
        assert!(routes(&frequencies).contains(r#"href="frequencies" aria-current="page""#));
        let help_dialog =
            r#" data-site-help-open aria-haspopup="dialog" aria-controls="site-help""#;
        assert!(routes(&frequencies).contains(help_dialog));
        assert_eq!(
            routes(&question).replace(r#" aria-current="page""#, ""),
            routes(&frequencies)
                .replace(r#" aria-current="page""#, "")
                .replace(help_dialog, "")
        );
        for html in [&question, &frequencies] {
            assert!(html.contains(r#"href="assets/questions/site.css""#));
        }
    }

    #[tokio::test]
    async fn a_query_error_returns_400_with_its_message_only_when_the_request_caused_it() {
        use crate::frequency::QueryError;
        let (_directory, views) = crate::frequency::tests::one_pair_views();
        let views = Arc::new(views);
        let registry = Arc::new(crate::coordinator::CoordinatorRegistry::new(1));
        let reply = |error: fn() -> QueryError| {
            on_pair(
                Arc::clone(&views),
                Arc::clone(&registry),
                "default".into(),
                "p.lx".into(),
                "p",
                move |_, _, _| Err::<Result<(), Unsupported>, _>(error()),
            )
        };

        for (error, message) in [
            (
                (|| QueryError::UnknownPattern("absent".into())) as fn() -> QueryError,
                "unknown pattern",
            ),
            (
                || QueryError::ResourceLimit {
                    resource: "offset",
                    limit: 7,
                },
                "offset limit of 7",
            ),
            (
                || QueryError::Table(lingrep_corpus::CountTableError::GroupLimit(7)),
                "exceeds the 7 encoded-value limit",
            ),
            (|| QueryError::Metadata(over_temp_budget()), "limit-bytes=1"),
            (
                || QueryError::Table(lingrep_corpus::CountTableError::Io(over_temp_budget())),
                "limit-bytes=1",
            ),
        ] {
            let (status, body) = status_and_body(reply(error).await).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert!(body.contains(message), "{body}");
        }
        for error in [
            (|| QueryError::Metadata(std::io::Error::other("/srv/spool/.tmpAbCdEf: gone")))
                as fn() -> QueryError,
            || QueryError::Metadata(std::io::ErrorKind::StorageFull.into()),
            || QueryError::Table(lingrep_corpus::CountTableError::Corrupt("/srv/t".into())),
            || QueryError::Table(lingrep_corpus::CountTableError::Cancelled),
            || QueryError::Lines("sentence 3 holds 1 occurrences, /srv/t counts 2".into()),
        ] {
            let (status, body) = status_and_body(reply(error).await).await;
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
            assert!(!body.contains("/srv"), "{body}");
            assert!(body.contains("Try again"), "{body}");
        }
    }

    #[tokio::test]
    async fn a_request_that_needs_no_scan_is_answered_while_the_scan_permit_is_held() {
        let (_directory, views) = crate::frequency::tests::one_pair_views();
        let views = Arc::new(views);
        let registry = Arc::new(crate::coordinator::CoordinatorRegistry::new(1));
        let _scanning = registry.try_acquire_scan_permit().expect("permit");
        for (views, corpus, pattern, status, text) in [
            (
                Arc::new(FrequencyViews::default()),
                "default",
                "p",
                StatusCode::OK,
                r#"{"status":"unavailable"}"#,
            ),
            (
                Arc::clone(&views),
                "absent",
                "p",
                StatusCode::OK,
                r#"{"status":"unavailable"}"#,
            ),
            (
                Arc::clone(&views),
                "default",
                "absent",
                StatusCode::BAD_REQUEST,
                r#"{"error":"unknown pattern \"absent\""}"#,
            ),
        ] {
            let response = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                on_pair(
                    views,
                    Arc::clone(&registry),
                    corpus.into(),
                    "p.lx".into(),
                    pattern,
                    |_, _, _| Ok(Ok::<(), Unsupported>(())),
                ),
            )
            .await
            .unwrap_or_else(|_| panic!("{corpus}/{pattern} waited for the scan permit"));
            assert_eq!(status_and_body(response).await, (status, text.to_owned()));
        }
    }

    #[tokio::test]
    async fn cancelled_queries_retain_shared_admission_until_blocking_work_finishes() {
        let registry = Arc::new(crate::coordinator::CoordinatorRegistry::new(1));
        let (started, running) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let task = tokio::spawn(run_frequency_task(Arc::clone(&registry), move |_| {
            started.send(()).expect("started");
            wait.recv().expect("release");
        }));
        running.await.expect("running");
        let mut next = Box::pin(registry.acquire_scan_permit());
        assert!(
            poll_fn(|cx| Poll::Ready(next.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        task.abort();
        assert!(task.await.expect_err("cancelled request").is_cancelled());
        assert!(
            poll_fn(|cx| Poll::Ready(next.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        release.send(()).expect("release work");
        let permit = tokio::time::timeout(std::time::Duration::from_secs(1), next)
            .await
            .expect("next request admitted");
        drop(permit);
    }

    #[tokio::test]
    async fn an_aborted_request_cancels_its_work_which_then_frees_admission() {
        let registry = Arc::new(crate::coordinator::CoordinatorRegistry::new(1));
        let (started, running) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(run_frequency_task(Arc::clone(&registry), move |cancel| {
            started.send(()).expect("started");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !cancel.is_cancelled() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }));
        running.await.expect("running");
        let mut next = Box::pin(registry.acquire_scan_permit());
        assert!(
            poll_fn(|cx| Poll::Ready(next.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        task.abort();
        let permit = tokio::time::timeout(std::time::Duration::from_secs(1), next)
            .await
            .expect("next request admitted once the work stops");
        drop(permit);
    }
}
