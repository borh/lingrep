//! The compare page: `GET /compare` and `GET /api/contrast`, which ranks one
//! pattern's entries between two document scopes of a bound count table.

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

use super::frequency::{bad_request, conditions, run_frequency_task};
use crate::frequency::{ContrastAnswer, ContrastQuery, Scope, ScopeUnsupported};
use crate::state::RequestStore;

#[derive(Template)]
#[template(path = "compare.html")]
struct ComparePage {
    current_page: &'static str,
    frequency_view: bool,
}

pub fn page_routes() -> Router<RequestStore> {
    Router::new().route("/compare", get(page))
}

pub fn routes() -> Router<RequestStore> {
    Router::new().route("/contrast", get(contrast))
}

async fn page(State(store): State<RequestStore>) -> Response {
    crate::handlers::render_or_500(ComparePage {
        current_page: "compare",
        frequency_view: !store.frequency.is_empty(),
    })
}

#[derive(Deserialize)]
struct ContrastParams {
    corpus: String,
    catalog: String,
    pattern: String,
    /// Conditions of the `--where` scope, as the Frequencies view's `w`.
    #[serde(default)]
    w: Option<String>,
    /// Conditions of the `--versus` scope.
    #[serde(default)]
    v: Option<String>,
    /// A `--where` clause written as text, used instead of `w`.
    #[serde(default, rename = "where")]
    where_clause: Option<String>,
    /// A `--versus` clause written as text, used instead of `v`.
    #[serde(default)]
    versus: Option<String>,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: usize,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Reply {
    Ok {
        #[serde(flatten)]
        body: ContrastAnswer,
        ms: f64,
    },
    Unsupported(ScopeUnsupported),
    Unavailable,
}

async fn contrast(
    State(store): State<RequestStore>,
    Query(params): Query<ContrastParams>,
) -> Response {
    let scopes = match (
        conditions(params.w.as_deref()),
        conditions(params.v.as_deref()),
    ) {
        (Ok(first), Ok(second)) => [
            Scope {
                conditions: first,
                clause: params.where_clause,
            },
            Scope {
                conditions: second,
                clause: params.versus,
            },
        ],
        (Err(response), _) | (_, Err(response)) => return *response,
    };
    let query = ContrastQuery {
        pattern: params.pattern,
        scopes,
        offset: params.offset,
        limit: params.limit,
    };
    let views = Arc::clone(&store.frequency);
    let (corpus, catalog) = (params.corpus, params.catalog);
    let started = Instant::now();
    let result = run_frequency_task(Arc::clone(&store.coordinators), move |cancellation| {
        let Some(pair) = views.find(&corpus, &catalog) else {
            return Ok(Reply::Unavailable);
        };
        Ok(
            match pair
                .contrast(views.dimensions(&pair.corpus), &query, cancellation)
                .map_err(|error| error.to_string())?
            {
                Ok(body) => Reply::Ok {
                    body,
                    ms: started.elapsed().as_secs_f64() * 1000.0,
                },
                Err(unsupported) => Reply::Unsupported(unsupported),
            },
        )
    })
    .await;
    match result {
        Ok(Ok(reply)) => Json(reply).into_response(),
        Ok(Err(message)) => bad_request(message),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
