//! The sample-text debugger: the `GET /sample` page and the `POST /api/sample`
//! endpoint that page (and nothing else) drives.
//!
//! The page holds no workbench and does not re-post the corpus search's live
//! pattern. A hit's debug link names a workbench revision, which the page
//! reads once for the pattern to open with. It reads a corpus only to load the
//! stored sentence a `sentence` reference names; without one it renders while
//! every declared corpus is still building.
//!
//! Admission runs in strict order so the cheapest checks reject first: size
//! caps (413) need no dictionary or worker slot; the semaphore (429) needs no
//! dictionary. The blocking task retains the worker slot until analysis ends,
//! including after request cancellation. `store.context.plan()` is resolved
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
use lingrep_core::analysis::AnalyzerPlan;
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
    /// Corpus key for comparing the corpus and sample token schemas.
    #[serde(default)]
    pub corpus: Option<String>,
    /// Corpus sentence ordinal whose stored analysis replaces tokenizing
    /// `sample`. The page stops sending it once the text is edited.
    #[serde(default)]
    pub corpus_sentence: Option<u64>,
}

/// A corpus sentence's stored tokens, its token schema and its analyzer's name.
type StoredSentence = (
    lingrep_core::token_match::BoundTokenSentence,
    lingrep_core::token_match::TokenSchema,
    String,
);

/// `None` when the corpus is unknown or not ready, or the sentence is absent
/// or larger than the sample limit.
async fn stored_sentence(
    store: &RequestStore,
    corpus: Option<&str>,
    sentence: u64,
) -> Option<StoredSentence> {
    let slot = store.corpora.resolve(corpus)?;
    let ready = slot.runtime.ready().await?;
    // Stored sentences carry the schema's token attributes; the enhanced
    // graph is stored apart from them and is not read here.
    let schema = lingrep_core::token_match::TokenSchema::new(
        ready
            .artifact
            .token_schema()
            .ok()?
            .attributes()
            .iter()
            .filter(|attribute| {
                attribute.provider() != lingrep_core::token_match::AttributeProvider::EnhancedGraph
            })
            .cloned()
            .collect::<Vec<_>>(),
    )
    .ok()?;
    let analyzer = ready.artifact.analyzer();
    let name = format!("{} {}", analyzer.family, analyzer.profile);
    let reader = ready.reader().ok()?.bind(&ready.identity)?;
    let words = tokio::task::spawn_blocking(move || {
        reader.sentence_attributes_bounded(sentence, MAX_SAMPLE_BYTES)
    })
    .await
    .ok()?
    .ok()??;
    Some((words, schema, name))
}

pub fn routes() -> Router<RequestStore> {
    Router::new().route("/sample", post(run_sample))
}

/// The `/sample` page, mounted outside `/api` beside the other pages.
pub fn page_routes() -> Router<RequestStore> {
    Router::new().route("/sample", get(sample_page))
}

#[derive(Deserialize)]
struct SampleQuery {
    corpus: Option<String>,
    sentence: Option<u64>,
    /// With `revision`, the Workbench run whose pattern seeds the page.
    workbench: Option<String>,
    revision: Option<u64>,
}

/// The corpus sentence the page opens with.
struct OpenedSentence {
    corpus: String,
    ordinal: u64,
    text: String,
}

#[derive(Template)]
#[template(path = "sample.html")]
struct SamplePage {
    opened: Option<OpenedSentence>,
    seed_pattern: String,
    current_page: &'static str,
    frequency_view: bool,
    catalog_keys: Vec<String>,
}

/// Seeded from the pattern of the Workbench run a hit's debug link names while
/// that run is displayed, and otherwise from the same `--patterns` default the
/// index seeds from, so landing here first still starts from the server's
/// catalog. The two pages then diverge: each pattern box persists its own
/// draft under its own id.
async fn sample_page(
    State(store): State<RequestStore>,
    axum::extract::Query(query): axum::extract::Query<SampleQuery>,
) -> Html<String> {
    let mut opened = None;
    if let Some(ordinal) = query.sentence
        && let Some((words, ..)) = stored_sentence(&store, query.corpus.as_deref(), ordinal).await
        && let Some(slot) = store.corpora.resolve(query.corpus.as_deref())
    {
        opened = Some(OpenedSentence {
            corpus: slot.name.as_str().to_owned(),
            ordinal,
            text: lingrep_core::token_match::display_text(&words, 0..words.len()),
        });
    }
    let mut seed_pattern = None;
    if let (Some(workbench), Some(revision)) = (query.workbench.as_deref(), query.revision)
        && let Ok(snapshot) = store
            .coordinators
            .displayed_snapshot(
                &crate::revision::WorkbenchId::new(workbench),
                crate::revision::RevisionId(revision),
            )
            .await
    {
        seed_pattern = Some(snapshot.source.to_string());
    }
    SamplePage {
        opened,
        seed_pattern: seed_pattern.unwrap_or_else(|| store.default_pattern.to_string()),
        current_page: "sample",
        frequency_view: !store.frequency.is_empty(),
        catalog_keys: store.catalogs.keys().map(str::to_owned).collect(),
    }
    .render().map_or_else(|_| {
        Html(
            "<div class=\"alert alert-error\" role=\"status\">Unable to render page. Please retry.</div>"
                .to_string(),
        )
    }, Html)
}

fn fragment(status: StatusCode, key: &str, message: &str) -> Response {
    // Shared `ErrorFragment` markup with Askama escaping; the plain-text
    // fallback is unreachable for this static template but keeps the handler
    // panic-free. Messages are fixed literals — never interpolate user input.
    let html = render_error(key, message).unwrap_or_else(|_| message.to_owned());
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
            "sample.too_large",
            "The sample exceeds the test limits. Shorten it or search a corpus with this pattern.",
        );
    }
    let Ok(permit) = SAMPLE_SLOTS.try_acquire() else {
        return fragment(
            StatusCode::TOO_MANY_REQUESTS,
            "sample.busy",
            "Server busy analyzing samples. Try again in a moment.",
        );
    };
    let pattern = form.pattern;
    let sample = form.sample;
    if let Some(sentence) = form.corpus_sentence {
        let Some((words, schema, analyzer)) =
            stored_sentence(&store, form.corpus.as_deref(), sentence).await
        else {
            return fragment(
                StatusCode::NOT_FOUND,
                "sample.stored_unavailable",
                "This corpus sentence could not be read. Edit the text to analyze it with the sample tokenizer.",
            );
        };
        let report = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            crate::sample::analyze_stored_sentence(&pattern, &schema, &words, analyzer)
        })
        .await;
        return match report {
            Ok(report) => SampleResults {
                analysis: report.analysis,
                run: report.run,
            }
            .render()
            .map(Html)
            .map_or_else(
                |_| {
                    fragment(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "sample.render_failed",
                        "Could not render sample results.",
                    )
                },
                IntoResponse::into_response,
            ),
            Err(_) => fragment(
                StatusCode::INTERNAL_SERVER_ERROR,
                "sample.render_failed",
                "Could not render sample results.",
            ),
        };
    }
    let corpus = super::pattern_editor::corpus_schema(&store, form.corpus.as_deref()).await;
    // The tokenizer's first-call dictionary load is synchronous I/O; resolve it
    // inside the blocking pool so the async executor never blocks. A load
    // failure returns `Err(TokenizerUnavailable)` (503); a genuine join failure
    // is the outer `Err` (500).
    let outcome = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let plan = store.context.plan()?;
        let analyzer = lingrep_corpus::AnalyzerRecord::from_descriptor(plan.descriptor());
        let dictionary = store
            .context
            .dict_path()
            .and_then(|path| path.file_name())
            .map(|name| format!(" · {}", name.to_string_lossy()))
            .unwrap_or_default();
        let mut report = analyze_sample(
            &pattern,
            &sample,
            &plan,
            format!("{} {}{dictionary}", analyzer.family, analyzer.profile),
        );
        if let (Some(run), Some((schema, corpus_analyzer))) = (&mut report.run, corpus)
            && schema != *plan.schema()
        {
            run.corpus_analyzer = Some(corpus_analyzer);
        }
        Ok::<_, TokenizerUnavailable>(report)
    })
    .await;
    match outcome {
        Ok(Ok(report)) => SampleResults {
            analysis: report.analysis,
            run: report.run,
        }
        .render()
        .map(Html)
        .map_or_else(
            |_| {
                fragment(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "sample.render_failed",
                    "Could not render sample results.",
                )
            },
            IntoResponse::into_response,
        ),
        Ok(Err(TokenizerUnavailable::NoDictionary)) => fragment(
            StatusCode::SERVICE_UNAVAILABLE,
            "sample.unavailable",
            "Sample analysis requires a dictionary. Run `ling dictionary install` or pass \
             --dict-path, then restart the server.",
        ),
        Ok(Err(TokenizerUnavailable::LoadFailed)) => fragment(
            StatusCode::SERVICE_UNAVAILABLE,
            "sample.unavailable",
            "Sample analysis is unavailable because the tokenizer dictionary could not be loaded.",
        ),
        Err(_) => fragment(
            StatusCode::INTERNAL_SERVER_ERROR,
            "sample.failed",
            "Sample analysis failed.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelling_a_queued_sample_retains_its_worker_slot() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        let (started, start) = std::sync::mpsc::channel();
        let blocker = runtime.spawn_blocking(move || {
            started.send(()).unwrap();
            wait.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        });
        start
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let before = SAMPLE_SLOTS.available_permits();
        let during = runtime.block_on(async {
            {
                let pending = run_sample(
                    State(RequestStore::default()),
                    Form(SampleForm {
                        pattern: "p := [].".into(),
                        sample: "猫".into(),
                        corpus: None,
                        corpus_sentence: None,
                    }),
                );
                tokio::pin!(pending);
                assert!(futures::poll!(&mut pending).is_pending());
            }
            SAMPLE_SLOTS.available_permits()
        });
        release.send(()).unwrap();
        runtime.block_on(blocker).unwrap();
        drop(runtime);
        assert_eq!(during, before - 1);
        assert_eq!(SAMPLE_SLOTS.available_permits(), before);
    }
}
