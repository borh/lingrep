//! The sample-text debugger: the `GET /sample` page and the `POST /api/sample`
//! endpoint that page (and nothing else) drives.
//!
//! The page is standalone by construction. It reads no corpus, holds no
//! workbench and takes no revision, so it renders while every declared corpus
//! is still building, and it does not re-post the corpus search's live
//! pattern.
//!
//! Admission runs in strict order so the cheapest checks reject first: size
//! caps (413) need no dictionary or worker slot; the semaphore (429) needs no
//! dictionary. The worker-slot permit is held across the `spawn_blocking().await`
//! for the whole analysis, not just the acquire. `store.context.tokenizer()` is resolved
//! *inside* the blocking closure because its first-call dictionary load is
//! synchronous I/O that must not run on the async executor; a load failure
//! surfaces as 503.

use std::sync::LazyLock;

use askama::Template;
use axum::{
    Form, Router,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use tokio::sync::Semaphore;

use crate::sample::analyze_sample;
use crate::state::{RequestStore, TokenizerUnavailable};
use crate::templates::{SampleResults, render_error};

// Matches `handlers::search::MAX_PATTERN_BYTES` and the compiler's 1 MiB
// source-byte ceiling, so a real grammar file can also be tested on sample text.
const MAX_PATTERN_BYTES: usize = 1 << 20;
const MAX_SAMPLE_BYTES: usize = 64 * 1024;
const MAX_SAMPLE_LINES: usize = 2000;

// Process-wide bound on concurrent sample analyses (each is a blocking job).
static SAMPLE_SLOTS: LazyLock<Semaphore> = LazyLock::new(|| {
    Semaphore::new(std::thread::available_parallelism().map_or(2, |n| n.get().div_ceil(2)))
});

#[derive(Deserialize)]
pub struct SampleForm {
    pub pattern: String,
    pub sample: String,
}

pub fn routes() -> Router<RequestStore> {
    Router::new().route("/sample", post(run_sample))
}

/// The `/sample` page, mounted outside `/api` beside the other pages.
pub fn page_routes() -> Router<RequestStore> {
    Router::new().route("/sample", get(sample_page))
}

#[derive(Template)]
#[template(path = "sample.html")]
struct SamplePage {
    seed_pattern: String,
    current_page: &'static str,
    catalog_keys: Vec<String>,
}

/// Seeded from the same `--patterns` default the index seeds from, so landing
/// here first still starts from the server's catalog. The two pages then
/// diverge: each pattern box persists its own draft under its own id.
async fn sample_page(State(store): State<RequestStore>) -> Html<String> {
    SamplePage {
        seed_pattern: store.default_pattern.to_string(),
        current_page: "sample",
        catalog_keys: store.catalogs.keys().map(str::to_owned).collect(),
    }
    .render().map_or_else(|_| {
        Html(
            "<div class=\"alert alert-error\" role=\"status\">Unable to render page. Please retry.</div>"
                .to_string(),
        )
    }, Html)
}

fn fragment(status: StatusCode, message: &str) -> Response {
    // Shared `ErrorFragment` markup with Askama escaping; the plain-text
    // fallback is unreachable for this static template but keeps the handler
    // panic-free. Messages are fixed literals — never interpolate user input.
    let html = render_error(message).unwrap_or_else(|_| message.to_owned());
    (status, Html(html)).into_response()
}

pub async fn run_sample(
    State(store): State<RequestStore>,
    Form(form): Form<SampleForm>,
) -> Response {
    if form.pattern.len() > MAX_PATTERN_BYTES
        || form.sample.len() > MAX_SAMPLE_BYTES
        || form.sample.lines().count() > MAX_SAMPLE_LINES
    {
        return fragment(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Sample too large to test — scan the corpus instead.",
        );
    }
    let Ok(_permit) = SAMPLE_SLOTS.try_acquire() else {
        return fragment(
            StatusCode::TOO_MANY_REQUESTS,
            "Server busy analyzing samples. Try again in a moment.",
        );
    };
    let pattern = form.pattern;
    let sample = form.sample;
    // The tokenizer's first-call dictionary load is synchronous I/O; resolve it
    // inside the blocking pool so the async executor never blocks. A load
    // failure returns `Err(TokenizerUnavailable)` (503); a genuine join failure
    // is the outer `Err` (500).
    let outcome = tokio::task::spawn_blocking(move || {
        let tokenizer = store.context.tokenizer()?;
        Ok::<_, TokenizerUnavailable>(analyze_sample(&pattern, &sample, &tokenizer))
    })
    .await;
    match outcome {
        Ok(Ok(analysis)) => SampleResults { analysis }.render().map(Html).map_or_else(
            |_| {
                fragment(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Could not render sample results.",
                )
            },
            IntoResponse::into_response,
        ),
        Ok(Err(TokenizerUnavailable)) => fragment(
            StatusCode::SERVICE_UNAVAILABLE,
            "Sample analysis is unavailable (dictionary not loaded).",
        ),
        Err(_) => fragment(StatusCode::INTERNAL_SERVER_ERROR, "Sample analysis failed."),
    }
}
